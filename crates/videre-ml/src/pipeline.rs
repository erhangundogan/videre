use anyhow::Result;
use rusqlite::Connection;

pub struct FacesRunResult {
    pub total_faces: usize,
    pub write_errors: usize,
    pub images_processed: usize,
    pub detect_errors: usize,
}

/// Detects, embeds, and writes faces for the given (path, hash) pairs -
/// callers are responsible for deciding which hashes need processing (e.g.
/// "not already in the faces table" for incremental use, or "everything"
/// for --reprocess). Chunks work by `batch` images per embedding call, same
/// as dupe-faces has always done.
pub fn run_face_pipeline(
    conn: &Connection,
    to_process: &[(String, String)],
    batch: usize,
    dry_run: bool,
    silent: bool,
) -> Result<FacesRunResult> {
    use crate::{face_align, face_detect, face_embed, face_models};
    use half::f16;

    if to_process.is_empty() {
        return Ok(FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 });
    }

    let (det_path, rec_path) = face_models::buffalo_l_paths()?;
    let mut detector = face_detect::FaceDetector::new(&det_path)?;
    let mut embedder = face_embed::FaceEmbedder::new(&rec_path)?;

    let progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);

    let mut total_faces = 0usize;
    let mut write_errors = 0usize;
    let mut images_processed = 0usize;
    let mut detect_errors = 0usize;

    for chunk in to_process.chunks(batch) {
        struct ChunkEntry {
            path: String,
            hash: String,
            detections: Vec<face_detect::Detection>,
            n_crops: usize,
        }
        let mut chunk_entries: Vec<ChunkEntry> = Vec::new();
        let mut chunk_crops: Vec<image::RgbImage> = Vec::new();

        for (path, hash) in chunk {
            images_processed += 1;
            let img = match load_image(path) {
                Some(i) => i,
                None => { progress.tick(); continue; }
            };
            let detections = match detector.detect(&img) {
                Ok(d) => d,
                Err(e) => {
                    progress.println(&format!("detect failed {path}: {e}"));
                    detect_errors += 1;
                    progress.tick();
                    continue;
                }
            };
            if detections.is_empty() { progress.tick(); continue; }

            let crops: Vec<image::RgbImage> = detections.iter()
                .map(|d| face_align::align_face(&img, &d.landmarks))
                .collect();

            let n_crops = crops.len();
            chunk_crops.extend(crops);
            chunk_entries.push(ChunkEntry { path: path.clone(), hash: hash.clone(), detections, n_crops });
            progress.tick();
        }

        if chunk_crops.is_empty() { continue; }

        let all_embeddings = match embedder.embed_batch(&chunk_crops) {
            Ok(e) => e,
            Err(e) => {
                progress.println(&format!("embed_batch failed: {e}"));
                detect_errors += chunk_entries.len();
                continue;
            }
        };

        let mut emb_offset = 0;
        for entry in &chunk_entries {
            let n = entry.n_crops;
            let embs = &all_embeddings[emb_offset..emb_offset + n];
            emb_offset += n;

            let rows: Vec<videre_core::face_db::FaceRow> = entry.detections.iter().zip(embs.iter()).map(|(det, emb)| {
                let [x1, y1, x2, y2] = det.bbox;
                let bbox = format!("{},{},{},{}", x1 as i32, y1 as i32, (x2 - x1) as i32, (y2 - y1) as i32);
                let lm_str: String = det.landmarks.iter()
                    .flat_map(|[x, y]| [x.to_string(), y.to_string()])
                    .collect::<Vec<_>>().join(",");
                let embedding: Vec<u8> = emb.iter()
                    .flat_map(|&v| f16::from_f32(v).to_le_bytes())
                    .collect();
                videre_core::face_db::FaceRow {
                    hash: entry.hash.clone(), bbox, landmark: Some(lm_str),
                    embedding, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0,
                }
            }).collect();

            total_faces += rows.len();
            if !dry_run {
                if let Err(e) = videre_core::face_db::replace_faces_for_hash(conn, &entry.hash, &rows) {
                    progress.println(&format!("write failed {}: {e}", entry.path));
                    write_errors += 1;
                }
            }
        }
    }

    progress.finish();

    Ok(FacesRunResult { total_faces, write_errors, images_processed, detect_errors })
}

pub struct ClusteringResult {
    pub total_faces: usize,
    pub clustered_faces: usize,
    pub cluster_count: usize,
}

/// Clusters `faces` (each `(id, embedding, min_bbox_side_px)`) after gating out
/// low-quality faces, which come back as unassigned singletons (`None`) instead
/// of being clustered. Two independent quality signals, a face failing either
/// one is held out:
///
///   * Size (`min_face_px`): tiny face crops upscale to ArcFace's 112px input
///     as mostly blur.
///   * Distinctiveness (`max_generic_sim`): faces that are occluded
///     (sunglasses/masks), non-frontal (profile), blurry, or outright false
///     detections (a carved statue face) carry little identity information, so
///     ArcFace maps them close to the population-average embedding. Such faces
///     all point in a similar generic direction regardless of who they are.
///
/// Either way, if these faces are clustered they pile up into one large *mixed*
/// junk cluster (which then gets centroid-merged into an even bigger one), so
/// they are held out. Distinctiveness is measured as cosine similarity to the
/// L2-normalized mean of every input embedding; a face is gated when that
/// similarity exceeds `max_generic_sim` (use >= 1.0 to disable the signal).
/// Returns assignments for every input face.
pub fn cluster_with_quality_gate(
    faces: &[(i64, Vec<f32>, f32)],
    eps: f32,
    min_cluster_size: usize,
    merge_sim: f32,
    min_face_px: f32,
    max_generic_sim: f32,
) -> Vec<(i64, Option<i64>)> {
    let global_mean = normalized_mean(faces.iter().map(|(_, e, _)| e));

    let mut quality: Vec<(i64, Vec<f32>)> = Vec::new();
    let mut low_quality_ids: Vec<i64> = Vec::new();
    for (id, emb, side) in faces {
        let too_small = *side < min_face_px;
        let too_generic = !global_mean.is_empty()
            && cosine_sim(emb, &global_mean) > max_generic_sim;
        if too_small || too_generic {
            low_quality_ids.push(*id);
        } else {
            quality.push((*id, emb.clone()));
        }
    }
    let mut assignments =
        videre_core::face_cluster::cluster_faces(&quality, eps, min_cluster_size, merge_sim);
    assignments.extend(low_quality_ids.into_iter().map(|id| (id, None)));
    assignments
}

/// L2-normalized mean of a set of embeddings, or an empty vec if there are none
/// (or they cancel to zero length).
fn normalized_mean<'a>(embs: impl Iterator<Item = &'a Vec<f32>>) -> Vec<f32> {
    let mut sum: Vec<f32> = Vec::new();
    for e in embs {
        if sum.is_empty() {
            sum = e.clone();
        } else {
            for (s, v) in sum.iter_mut().zip(e) { *s += v; }
        }
    }
    let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 { for s in &mut sum { *s /= norm; } } else { sum.clear(); }
    sum
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Re-runs two-stage clustering (average-linkage, then a centroid-merge pass
/// that reunites one person's fragmented sub-clusters) over every face
/// embedding currently in the database - safe to call whether or not
/// run_face_pipeline found anything new, since re-clustering is idempotent.
/// Returns `None` when there are no faces in the database to cluster; callers
/// decide whether/how to report that.
pub fn run_clustering(
    conn: &Connection,
    eps: f32,
    min_cluster_size: usize,
    merge_sim: f32,
    min_face_px: f32,
    max_generic_sim: f32,
) -> Result<Option<ClusteringResult>> {
    let all_faces = videre_core::face_db::load_faces_for_clustering(conn)?;
    if all_faces.is_empty() {
        return Ok(None);
    }
    let assignments = cluster_with_quality_gate(
        &all_faces, eps, min_cluster_size, merge_sim, min_face_px, max_generic_sim,
    );
    videre_core::face_db::update_cluster_assignments(conn, &assignments)?;
    let clustered_faces = assignments.iter().filter(|(_, c)| c.is_some()).count();
    let cluster_count = assignments
        .iter()
        .filter_map(|(_, c)| *c)
        .collect::<std::collections::HashSet<_>>()
        .len();
    Ok(Some(ClusteringResult { total_faces: all_faces.len(), clustered_faces, cluster_count }))
}

fn load_image(path: &str) -> Option<image::DynamicImage> {
    if path.to_lowercase().ends_with(".heic") {
        #[cfg(target_os = "macos")]
        {
            return videre_core::heic::heic_via_quicklook(path, "faces");
        }
        #[cfg(not(target_os = "macos"))]
        return None;
    }
    image::open(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use videre_core::face_db;

    #[test]
    fn run_face_pipeline_on_empty_input_is_a_noop() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        let result = run_face_pipeline(&conn, &[], 8, false, true).unwrap();
        assert_eq!(result.total_faces, 0);
        assert_eq!(result.write_errors, 0);
        assert_eq!(result.images_processed, 0);
        assert_eq!(result.detect_errors, 0);
    }

    #[test]
    fn run_clustering_on_empty_db_does_not_error() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        let result = run_clustering(&conn, 0.6, 3, 0.35, 50.0, 0.4).unwrap();
        assert!(result.is_none());
    }

    fn l2(mut v: Vec<f32>) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut v { *x /= n; }
        v
    }

    #[test]
    fn quality_gate_excludes_tiny_faces_from_clustering() {
        // Two real identities of large faces plus two tiny faces that, by
        // embedding, sit right on identity A. With the gate active the tiny
        // faces must be left unassigned (None) rather than joining A.
        let a = || l2(vec![1.0, 0.02, 0.0]);
        let b = || l2(vec![0.0, 0.02, 1.0]);
        let faces = vec![
            (1, a(), 200.0), (2, a(), 180.0), (3, a(), 160.0),
            (4, b(), 200.0), (5, b(), 190.0), (6, b(), 170.0),
            (7, a(), 20.0), (8, a(), 15.0), // tiny, would otherwise join A
        ];
        // max_generic_sim = 1.0 disables the distinctiveness gate for this test.
        let result = cluster_with_quality_gate(&faces, 0.3, 3, 1.0, 50.0, 1.0);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_eq!(map[&7], None, "tiny face must be gated out of clustering");
        assert_eq!(map[&8], None, "tiny face must be gated out of clustering");
        assert!(map[&1].is_some(), "large faces still cluster");
        assert_eq!(map[&1], map[&2], "identity A stays together");
        assert_ne!(map[&1], map[&4], "identity A and B stay distinct");
    }

    #[test]
    fn distinctiveness_gate_excludes_generic_large_faces() {
        // Five faces along e0 dominate the population, so the global mean points
        // ~e0; those faces are "generic" (high similarity to the mean) and must
        // be gated out even though they are large and would otherwise cluster.
        // Three faces along e1 are distinctive (low similarity to the mean) and
        // must survive to form their own cluster.
        let gen = |noise: f32| l2(vec![1.0, noise, 0.0]);
        let dist = |noise: f32| l2(vec![0.0, noise, 1.0]);
        let faces = vec![
            (1, gen(0.01), 300.0), (2, gen(0.02), 300.0), (3, gen(0.03), 300.0),
            (4, gen(0.04), 300.0), (5, gen(0.05), 300.0),
            (6, dist(0.01), 300.0), (7, dist(0.02), 300.0), (8, dist(0.03), 300.0),
        ];
        // size gate off (min_face_px=0), distinctiveness gate at 0.6.
        let result = cluster_with_quality_gate(&faces, 0.3, 3, 1.0, 0.0, 0.6);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        for id in 1..=5 {
            assert_eq!(map[&id], None, "generic (near-average) large face {id} must be gated out");
        }
        assert!(map[&6].is_some(), "distinctive faces must still cluster");
        assert_eq!(map[&6], map[&7], "distinctive identity stays together");
        assert_eq!(map[&7], map[&8], "distinctive identity stays together");
    }
}
