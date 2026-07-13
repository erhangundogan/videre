use anyhow::Result;
use rusqlite::Connection;

pub struct FacesRunResult {
    pub total_faces: usize,
    pub write_errors: usize,
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
        return Ok(FacesRunResult { total_faces: 0, write_errors: 0 });
    }

    if !silent { eprintln!("Processing {} images...", to_process.len()); }

    let (det_path, rec_path) = face_models::buffalo_l_paths()?;
    let mut detector = face_detect::FaceDetector::new(&det_path)?;
    let mut embedder = face_embed::FaceEmbedder::new(&rec_path)?;

    let mut total_faces = 0usize;
    let mut write_errors = 0usize;

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
            let img = match load_image(path) {
                Some(i) => i,
                None => continue,
            };
            let detections = match detector.detect(&img) {
                Ok(d) => d,
                Err(e) => { eprintln!("detect failed {path}: {e}"); continue; }
            };
            if detections.is_empty() { continue; }

            let crops: Vec<image::RgbImage> = detections.iter()
                .map(|d| face_align::align_face(&img, &d.landmarks))
                .collect();

            if !silent { eprintln!("[faces] {path}: {} face(s)", detections.len()); }
            let n_crops = crops.len();
            chunk_crops.extend(crops);
            chunk_entries.push(ChunkEntry { path: path.clone(), hash: hash.clone(), detections, n_crops });
        }

        if chunk_crops.is_empty() { continue; }

        let all_embeddings = match embedder.embed_batch(&chunk_crops) {
            Ok(e) => e,
            Err(e) => { eprintln!("embed_batch failed: {e}"); continue; }
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
                    eprintln!("write failed {}: {e}", entry.path);
                    write_errors += 1;
                }
            }
        }
    }

    Ok(FacesRunResult { total_faces, write_errors })
}

/// Re-runs DBSCAN clustering over every face embedding currently in the
/// database - safe to call whether or not run_face_pipeline found anything
/// new, since re-clustering is idempotent.
pub fn run_clustering(
    conn: &Connection,
    eps: f32,
    min_cluster_size: usize,
    silent: bool,
) -> Result<()> {
    let all_embs = videre_core::face_db::load_face_embeddings(conn)?;
    if all_embs.is_empty() {
        if !silent {
            eprintln!("No faces in DB to cluster.");
        }
        return Ok(());
    }
    let assignments = videre_core::face_cluster::dbscan_cosine(&all_embs, eps, min_cluster_size);
    videre_core::face_db::update_cluster_assignments(conn, &assignments)?;
    if !silent {
        let clustered = assignments.iter().filter(|(_, c)| c.is_some()).count();
        eprintln!("Clustering complete: {}/{} faces assigned to clusters (eps={:.2}).", clustered, all_embs.len(), eps);
    }
    Ok(())
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
    }

    #[test]
    fn run_clustering_on_empty_db_does_not_error() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        run_clustering(&conn, 0.6, 3, true).unwrap();
    }
}
