use anyhow::Result;
use rusqlite::Connection;

/// Per-stage timing accumulator for `videre faces --profile`. All durations
/// are cumulative across every image processed; `format_profile_report`
/// divides by the relevant count to report per-image averages. Load time is
/// tracked separately for HEIC (goes through a `qlmanage` subprocess) vs.
/// everything else, since that's the one stage known to differ sharply by
/// file type - see `docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md`.
#[derive(Debug, Default, Clone)]
pub struct ProfileStats {
    pub load_heic: std::time::Duration,
    pub load_other: std::time::Duration,
    pub detect: std::time::Duration,
    pub align: std::time::Duration,
    pub embed: std::time::Duration,
    pub db_write: std::time::Duration,
    pub count_heic: usize,
    pub count_other: usize,
}

impl ProfileStats {
    /// Merges another worker's (or the coordinator's) stats into this one -
    /// used once the pipeline is multi-threaded (Task 6) to combine each
    /// worker's local accumulator plus the coordinator's own db_write timing
    /// into a single report at the end of the run.
    pub fn merge(&mut self, other: ProfileStats) {
        self.load_heic += other.load_heic;
        self.load_other += other.load_other;
        self.detect += other.detect;
        self.align += other.align;
        self.embed += other.embed;
        self.db_write += other.db_write;
        self.count_heic += other.count_heic;
        self.count_other += other.count_other;
    }
}

/// Formats a `--profile` report: per-image averages for each pipeline stage,
/// with load time split HEIC vs. other. Divisions guard against zero counts
/// (an empty or all-one-type run) rather than panicking.
pub fn format_profile_report(stats: &ProfileStats) -> String {
    let total = stats.count_heic + stats.count_other;
    let avg_ms = |total: std::time::Duration, count: usize| -> u128 {
        if count == 0 { 0 } else { total.as_millis() / count as u128 }
    };
    let mut s = format!(
        "--profile: {total} image(s) ({} heic, {} other)",
        stats.count_heic, stats.count_other
    );
    if total == 0 {
        return s;
    }
    s.push_str(&format!(
        "\nload: heic avg {}ms (n={}), other avg {}ms (n={})",
        avg_ms(stats.load_heic, stats.count_heic), stats.count_heic,
        avg_ms(stats.load_other, stats.count_other), stats.count_other,
    ));
    s.push_str(&format!("\ndetect: avg {}ms", avg_ms(stats.detect, total)));
    s.push_str(&format!("\nalign: avg {}ms", avg_ms(stats.align, total)));
    s.push_str(&format!("\nembed: avg {}ms", avg_ms(stats.embed, total)));
    s.push_str(&format!("\ndb_write: avg {}ms", avg_ms(stats.db_write, total)));
    s
}

/// Splits `items` into `workers` partitions round-robin (item at index `i`
/// goes to partition `i % workers`), not contiguous chunks - this spreads
/// any clustering in the input order (e.g. a run of HEIC files from one
/// photo-import session sitting contiguously) evenly across workers instead
/// of letting one worker inherit a disproportionately slow subset. Panics if
/// `workers` is 0 (a caller bug, not a runtime condition to handle
/// gracefully - `--workers` is validated to be at least 1 before this is
/// called).
pub fn round_robin_partition<T: Clone>(items: &[T], workers: usize) -> Vec<Vec<T>> {
    assert!(workers > 0, "round_robin_partition requires at least 1 worker");
    let mut parts: Vec<Vec<T>> = vec![Vec::new(); workers];
    for (i, item) in items.iter().enumerate() {
        parts[i % workers].push(item.clone());
    }
    parts
}

pub struct FacesRunResult {
    pub total_faces: usize,
    pub write_errors: usize,
    pub images_processed: usize,
    pub detect_errors: usize,
}

/// One worker thread's report of a single image's (or, for
/// `EmbedBatchError`, a whole failed chunk's) outcome, sent to the
/// coordinator thread over an `mpsc` channel. The coordinator is the only
/// thing that touches the database (see Task 6) - `Faces` carries everything
/// needed to call `replace_faces_for_hash` there. Per-image error messages
/// (`skipping ...`, `detect failed ...`) are printed by the worker itself via
/// the shared, thread-safe `Progress::println` (see
/// `videre_core::progress::Progress`'s doc comment) - `ImageError`/
/// `EmbedBatchError` carry counts only, not message text, since the text was
/// already printed at the point of failure.
pub enum WorkerMsg {
    NoFace { hash: String },
    Faces { hash: String, rows: Vec<videre_core::face_db::FaceRow> },
    ImageError,
    EmbedBatchError { n: usize },
}

/// Updates `result`'s counters for one `WorkerMsg` - the part of handling a
/// message that's pure bookkeeping, independent of the (impure) DB write a
/// `Faces` message also triggers on the coordinator. Extracted so this
/// bookkeeping is unit-testable without a real `Connection`.
pub fn apply_worker_msg_counts(result: &mut FacesRunResult, msg: &WorkerMsg) {
    match msg {
        WorkerMsg::NoFace { .. } => {
            result.images_processed += 1;
        }
        WorkerMsg::Faces { rows, .. } => {
            result.images_processed += 1;
            result.total_faces += rows.len();
        }
        WorkerMsg::ImageError => {
            result.images_processed += 1;
            result.detect_errors += 1;
        }
        WorkerMsg::EmbedBatchError { n } => {
            result.images_processed += n;
            result.detect_errors += n;
        }
    }
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
    mut profile: Option<&mut ProfileStats>,
    workers: usize,
) -> Result<FacesRunResult> {
    use crate::{face_align, face_detect, face_embed, face_models};

    if to_process.is_empty() {
        return Ok(FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 });
    }

    let (det_path, rec_path) = face_models::buffalo_l_paths()?;
    let progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);

    let worker_count = workers.max(1);
    let intra_threads = std::thread::available_parallelism()
        .map(|n| (n.get() / worker_count).max(1))
        .unwrap_or(1);
    let partitions = round_robin_partition(to_process, worker_count);
    let want_profile = profile.is_some();

    let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };

    std::thread::scope(|scope| -> Result<()> {
        let (tx, rx) = std::sync::mpsc::channel::<WorkerMsg>();

        // Spawn one worker per partition, keeping each ScopedJoinHandle (not
        // discarding it) so its returned ProfileStats - and any Err from
        // model loading inside the worker - actually reaches the caller.
        // thread::scope only guarantees threads are joined before the scope
        // returns; it does not automatically surface their return values.
        let handles: Vec<std::thread::ScopedJoinHandle<Result<ProfileStats>>> = partitions
            .iter()
            .map(|partition| {
                let tx = tx.clone();
                let det_path = det_path.clone();
                let rec_path = rec_path.clone();
                let progress = &progress;
                scope.spawn(move || -> Result<ProfileStats> {
                    let mut local_profile = ProfileStats::default();
                    let mut detector = face_detect::FaceDetector::new(&det_path, intra_threads)?;
                    let mut embedder = face_embed::FaceEmbedder::new(&rec_path, intra_threads)?;

                    for chunk in partition.chunks(batch) {
                        struct ChunkEntry {
                            hash: String,
                            detections: Vec<face_detect::Detection>,
                            n_crops: usize,
                        }
                        let mut chunk_entries: Vec<ChunkEntry> = Vec::new();
                        let mut chunk_crops: Vec<image::RgbImage> = Vec::new();

                        for (path, hash) in chunk {
                            let load_start = std::time::Instant::now();
                            let img = match load_image(path) {
                                Ok(i) => i,
                                Err(msg) => {
                                    progress.println(&format!("skipping {path}: {msg}"));
                                    let _ = tx.send(WorkerMsg::ImageError);
                                    progress.tick();
                                    continue;
                                }
                            };
                            if want_profile {
                                let d = load_start.elapsed();
                                let is_heic = path.as_bytes().len() >= 5
                                    && path.as_bytes()[path.len() - 5..].eq_ignore_ascii_case(b".heic");
                                if is_heic { local_profile.load_heic += d; local_profile.count_heic += 1; }
                                else { local_profile.load_other += d; local_profile.count_other += 1; }
                            }

                            let detect_start = std::time::Instant::now();
                            let detections = match detector.detect(&img) {
                                Ok(d) => d,
                                Err(e) => {
                                    progress.println(&format!("detect failed {path}: {e}"));
                                    let _ = tx.send(WorkerMsg::ImageError);
                                    progress.tick();
                                    continue;
                                }
                            };
                            if want_profile { local_profile.detect += detect_start.elapsed(); }

                            if detections.is_empty() {
                                let _ = tx.send(WorkerMsg::NoFace { hash: hash.clone() });
                                progress.tick();
                                continue;
                            }

                            let align_start = std::time::Instant::now();
                            let crops: Vec<image::RgbImage> = detections.iter()
                                .map(|d| face_align::align_face(&img, &d.landmarks))
                                .collect();
                            if want_profile { local_profile.align += align_start.elapsed(); }

                            let n_crops = crops.len();
                            chunk_crops.extend(crops);
                            chunk_entries.push(ChunkEntry { hash: hash.clone(), detections, n_crops });
                            progress.tick();
                        }

                        if chunk_crops.is_empty() { continue; }

                        let embed_start = std::time::Instant::now();
                        let all_embeddings = match embedder.embed_batch(&chunk_crops) {
                            Ok(e) => e,
                            Err(e) => {
                                progress.println(&format!("embed_batch failed: {e}"));
                                let _ = tx.send(WorkerMsg::EmbedBatchError { n: chunk_entries.len() });
                                continue;
                            }
                        };
                        if want_profile { local_profile.embed += embed_start.elapsed(); }

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
                                    .flat_map(|&v| half::f16::from_f32(v).to_le_bytes())
                                    .collect();
                                videre_core::face_db::FaceRow {
                                    hash: entry.hash.clone(), bbox, landmark: Some(lm_str),
                                    embedding, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0,
                                }
                            }).collect();
                            let _ = tx.send(WorkerMsg::Faces { hash: entry.hash.clone(), rows });
                        }
                    }
                    Ok(local_profile)
                })
            })
            .collect();
        drop(tx); // coordinator's own handle - workers hold the rest, channel closes once all clones drop

        for msg in rx {
            apply_worker_msg_counts(&mut result, &msg);
            match msg {
                WorkerMsg::Faces { hash, rows } => {
                    if !dry_run {
                        let write_start = std::time::Instant::now();
                        let write_result = videre_core::face_db::replace_faces_for_hash(conn, &hash, &rows);
                        if let Some(p) = profile.as_deref_mut() { p.db_write += write_start.elapsed(); }
                        match write_result {
                            Ok(()) => { let _ = videre_core::face_db::mark_scanned(conn, &hash); }
                            Err(e) => {
                                progress.println(&format!("write failed {hash}: {e}"));
                                result.write_errors += 1;
                            }
                        }
                    }
                }
                WorkerMsg::NoFace { hash } => {
                    if !dry_run { let _ = videre_core::face_db::mark_scanned(conn, &hash); }
                }
                WorkerMsg::ImageError | WorkerMsg::EmbedBatchError { .. } => {}
            }
        }

        // Join every worker, propagating both a thread panic and the
        // worker's own Result<ProfileStats> error, and merge each worker's
        // timing into the caller's accumulator (if profiling was requested).
        for handle in handles {
            let worker_profile = handle
                .join()
                .map_err(|_| anyhow::anyhow!("face detection worker thread panicked"))??;
            if let Some(p) = profile.as_deref_mut() {
                p.merge(worker_profile);
            }
        }
        Ok(())
    })?;

    progress.finish();
    Ok(result)
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
    silent: bool,
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
    // The average-linkage pass below is O(n^2) - never silent about starting
    // it, even under --silent's per-image progress suppression, since a
    // large library's clustering pass can itself take real time with no
    // other visible output in between (this was previously silent end-to-end,
    // which looked identical to a hang once the face count grew large).
    if !silent {
        eprintln!("Clustering {} face(s) (eps={eps:.2})...", quality.len());
    }
    let mut assignments =
        videre_core::face_cluster::cluster_faces(&quality, eps, min_cluster_size, merge_sim, silent);
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
    silent: bool,
) -> Result<Option<ClusteringResult>> {
    let all_faces = videre_core::face_db::load_faces_for_clustering(conn)?;
    if all_faces.is_empty() {
        return Ok(None);
    }
    let assignments = cluster_with_quality_gate(
        &all_faces, eps, min_cluster_size, merge_sim, min_face_px, max_generic_sim, silent,
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

fn load_image(path: &str) -> Result<image::DynamicImage, String> {
    if path.to_lowercase().ends_with(".heic") {
        #[cfg(target_os = "macos")]
        {
            return videre_core::heic::heic_via_quicklook(path, "faces").ok_or_else(|| {
                format!(
                    "could not read/convert HEIC file {path} (missing, timed out, or unreadable - is its drive connected?)"
                )
            });
        }
        #[cfg(not(target_os = "macos"))]
        return Err(format!("HEIC decoding is only supported on macOS: {path}"));
    }
    let timeout_path = path.to_string();
    videre_core::io_timeout::run_with_timeout(videre_core::io_timeout::DEFAULT_IO_TIMEOUT, move || {
        image::open(&timeout_path)
    })
    .map_err(|_| {
        format!(
            "timed out reading {path} after {}s (file may be unreachable - is its drive connected?)",
            videre_core::io_timeout::DEFAULT_IO_TIMEOUT.as_secs()
        )
    })
    .and_then(|r| r.map_err(|e| format!("could not read {path}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use videre_core::face_db;

    #[test]
    fn profile_stats_merge_sums_all_fields() {
        let mut a = ProfileStats {
            load_heic: std::time::Duration::from_millis(100),
            load_other: std::time::Duration::from_millis(50),
            detect: std::time::Duration::from_millis(200),
            align: std::time::Duration::from_millis(10),
            embed: std::time::Duration::from_millis(80),
            db_write: std::time::Duration::from_millis(5),
            count_heic: 2,
            count_other: 3,
        };
        let b = ProfileStats {
            load_heic: std::time::Duration::from_millis(20),
            load_other: std::time::Duration::from_millis(10),
            detect: std::time::Duration::from_millis(40),
            align: std::time::Duration::from_millis(2),
            embed: std::time::Duration::from_millis(16),
            db_write: std::time::Duration::from_millis(1),
            count_heic: 1,
            count_other: 1,
        };
        a.merge(b);
        assert_eq!(a.load_heic, std::time::Duration::from_millis(120));
        assert_eq!(a.load_other, std::time::Duration::from_millis(60));
        assert_eq!(a.detect, std::time::Duration::from_millis(240));
        assert_eq!(a.align, std::time::Duration::from_millis(12));
        assert_eq!(a.embed, std::time::Duration::from_millis(96));
        assert_eq!(a.db_write, std::time::Duration::from_millis(6));
        assert_eq!(a.count_heic, 3);
        assert_eq!(a.count_other, 4);
    }

    #[test]
    fn format_profile_report_computes_per_image_averages() {
        let stats = ProfileStats {
            load_heic: std::time::Duration::from_millis(1000),
            load_other: std::time::Duration::from_millis(400),
            detect: std::time::Duration::from_millis(500),
            align: std::time::Duration::from_millis(50),
            embed: std::time::Duration::from_millis(200),
            db_write: std::time::Duration::from_millis(10),
            count_heic: 2,
            count_other: 4,
        };
        let report = format_profile_report(&stats);
        assert_eq!(
            report,
            "--profile: 6 image(s) (2 heic, 4 other)\n\
             load: heic avg 500ms (n=2), other avg 100ms (n=4)\n\
             detect: avg 83ms\n\
             align: avg 8ms\n\
             embed: avg 33ms\n\
             db_write: avg 1ms"
        );
    }

    #[test]
    fn format_profile_report_handles_zero_counts_without_dividing_by_zero() {
        let stats = ProfileStats::default();
        let report = format_profile_report(&stats);
        assert_eq!(report, "--profile: 0 image(s) (0 heic, 0 other)");
    }

    #[test]
    fn run_face_pipeline_on_empty_input_is_a_noop() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        let result = run_face_pipeline(&conn, &[], 8, false, true, None, 4).unwrap();
        assert_eq!(result.total_faces, 0);
        assert_eq!(result.write_errors, 0);
        assert_eq!(result.images_processed, 0);
        assert_eq!(result.detect_errors, 0);
    }

    #[test]
    fn load_image_missing_file_returns_descriptive_error() {
        let err = load_image("/no/such/path/does-not-exist.jpg").unwrap_err();
        assert!(err.contains("/no/such/path/does-not-exist.jpg"), "error should name the path: {err}");
    }

    #[test]
    fn run_clustering_on_empty_db_does_not_error() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        let result = run_clustering(&conn, 0.6, 3, 0.35, 50.0, 0.4, true).unwrap();
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
        let result = cluster_with_quality_gate(&faces, 0.3, 3, 1.0, 50.0, 1.0, true);
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
        let result = cluster_with_quality_gate(&faces, 0.3, 3, 1.0, 0.0, 0.6, true);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        for id in 1..=5 {
            assert_eq!(map[&id], None, "generic (near-average) large face {id} must be gated out");
        }
        assert!(map[&6].is_some(), "distinctive faces must still cluster");
        assert_eq!(map[&6], map[&7], "distinctive identity stays together");
        assert_eq!(map[&7], map[&8], "distinctive identity stays together");
    }

    #[test]
    fn round_robin_partition_covers_every_item_exactly_once() {
        let items: Vec<i32> = (0..10).collect();
        let parts = round_robin_partition(&items, 3);
        assert_eq!(parts.len(), 3);
        let mut seen: Vec<i32> = parts.iter().flatten().copied().collect();
        seen.sort();
        assert_eq!(seen, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn round_robin_partition_assigns_by_index_modulo_worker_count() {
        let items: Vec<i32> = (0..6).collect();
        let parts = round_robin_partition(&items, 3);
        assert_eq!(parts[0], vec![0, 3]);
        assert_eq!(parts[1], vec![1, 4]);
        assert_eq!(parts[2], vec![2, 5]);
    }

    #[test]
    fn round_robin_partition_more_workers_than_items_leaves_some_empty() {
        let items: Vec<i32> = vec![10, 20];
        let parts = round_robin_partition(&items, 5);
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], vec![10]);
        assert_eq!(parts[1], vec![20]);
        assert!(parts[2].is_empty());
        assert!(parts[3].is_empty());
        assert!(parts[4].is_empty());
    }

    #[test]
    fn round_robin_partition_empty_items_returns_empty_partitions() {
        let items: Vec<i32> = vec![];
        let parts = round_robin_partition(&items, 4);
        assert_eq!(parts.len(), 4);
        assert!(parts.iter().all(|p: &Vec<i32>| p.is_empty()));
    }

    #[test]
    #[should_panic]
    fn round_robin_partition_zero_workers_panics() {
        let items: Vec<i32> = vec![1, 2, 3];
        round_robin_partition(&items, 0);
    }

    fn sample_face_row(hash: &str) -> videre_core::face_db::FaceRow {
        videre_core::face_db::FaceRow {
            hash: hash.to_string(),
            bbox: "0,0,10,10".to_string(),
            landmark: None,
            embedding: vec![0u8; 1024],
            cluster_id: None,
            person_label: None,
            confirmed: 0,
            is_primary: 0,
        }
    }

    #[test]
    fn apply_worker_msg_no_face_increments_images_processed_only() {
        let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };
        apply_worker_msg_counts(&mut result, &WorkerMsg::NoFace { hash: "h1".into() });
        assert_eq!(result.images_processed, 1);
        assert_eq!(result.total_faces, 0);
        assert_eq!(result.detect_errors, 0);
    }

    #[test]
    fn apply_worker_msg_faces_increments_processed_and_total_faces() {
        let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };
        let rows = vec![sample_face_row("h1"), sample_face_row("h1")];
        apply_worker_msg_counts(&mut result, &WorkerMsg::Faces { hash: "h1".into(), rows });
        assert_eq!(result.images_processed, 1);
        assert_eq!(result.total_faces, 2);
    }

    #[test]
    fn apply_worker_msg_image_error_increments_processed_and_detect_errors() {
        let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };
        apply_worker_msg_counts(&mut result, &WorkerMsg::ImageError);
        assert_eq!(result.images_processed, 1);
        assert_eq!(result.detect_errors, 1);
    }

    #[test]
    fn apply_worker_msg_embed_batch_error_increments_processed_and_detect_errors_by_n() {
        let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };
        apply_worker_msg_counts(&mut result, &WorkerMsg::EmbedBatchError { n: 5 });
        assert_eq!(result.images_processed, 5);
        assert_eq!(result.detect_errors, 5);
    }
}
