use anyhow::Result;
use videre_core::face_db;
use videre_ml::pipeline::{run_clustering, run_face_pipeline, ClusteringResult, FacesRunResult};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct FacesArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,
    #[arg(long)] reprocess: bool,
    /// Skip detection; just re-run clustering on existing embeddings
    #[arg(long)] recluster: bool,
    #[arg(long, default_value = "8")] batch: usize,
    #[arg(long)] dry_run: bool,
    #[arg(long)] silent: bool,
    /// Average-linkage cosine-distance radius (0 = identical, 2 = opposite). Default 0.6.
    #[arg(long, default_value = "0.6")] eps: f32,
    /// Minimum faces per cluster (below this, faces are left as singletons). Default 3.
    #[arg(long, default_value = "3")] min_cluster_size: usize,
    /// Centroid-merge similarity: after clustering, clusters whose mean embeddings
    /// are at least this cosine-similar are merged (reunites one person's fragmented
    /// clusters). 0 = identical direction required, 1 = disables merging. Default 0.35.
    #[arg(long, default_value = "0.35")] merge_sim: f32,
    /// Minimum face size (smaller bbox side, px) to take part in clustering. Smaller
    /// faces embed poorly and pile into a mixed junk cluster, so they are held out as
    /// unassigned singletons. 0 disables the gate. Default 80.
    #[arg(long, default_value = "80")] min_face_size: f32,
    /// Process at most N not-yet-scanned images this run, then stop. Resumable: each
    /// run records what it scanned (including images with no faces) and a rerun
    /// continues where it left off. Clustering is skipped on a limited run - run
    /// `videre faces --recluster` when you're done scanning.
    #[arg(long)] limit: Option<usize>,
    /// Distinctiveness gate: faces whose embedding is more than this cosine-similar to
    /// the population-average face (occluded/profile/blurry/false detections) are held
    /// out of clustering. Lower = stricter. 1 disables. Default 0.4.
    #[arg(long, default_value = "0.4")] max_generic_sim: f32,
}

pub fn run(args: FacesArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    if !db.exists() {
        anyhow::bail!("{:?} does not exist", db);
    }
    let conn = videre_core::db::open_wal(&db)?;
    face_db::create_faces_table(&conn)?;

    // 1. Determine which hashes to process
    let all_paths: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT path, hash FROM file_hashes WHERE ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')"
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    // Skip anything already scanned. The marker (`faces_scanned`) records every
    // processed hash including images with zero faces, which is what makes reruns
    // resumable instead of re-detecting the whole no-face population every time.
    // Union in hashes that already have faces so a first run after upgrading (when
    // the marker table is empty but faces exist) doesn't redo that work.
    let skip_hashes: std::collections::HashSet<String> = if args.reprocess {
        std::collections::HashSet::new()
    } else {
        let mut s: std::collections::HashSet<String> =
            face_db::scanned_hashes(&conn)?.into_iter().collect();
        s.extend(face_db::hashes_with_faces(&conn)?);
        s
    };

    // Dedup by hash, drop skipped, cap at --limit for a partial/lazy pass.
    let to_process = face_db::select_unscanned(&all_paths, &skip_hashes, args.limit);

    if args.recluster || to_process.is_empty() {
        if !args.silent && to_process.is_empty() && !args.recluster {
            eprintln!("All hashes already processed.");
        }
        // Skip detection; jump straight to clustering
        if !args.dry_run {
            let clustering = run_clustering(&conn, args.eps, args.min_cluster_size, args.merge_sim, args.min_face_size, args.max_generic_sim)?;
            if !args.silent {
                eprintln!("{}", format_clustering_only_summary(clustering, args.eps));
            }
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let result = run_face_pipeline(&conn, &to_process, args.batch, args.dry_run, args.silent)?;

    // Cluster at the end of a full pass, but skip it on a partial (--limit) run:
    // clustering is an O(n^2) whole-library step and re-running it after every
    // small chunk is wasted work. On a limited run, tell the user to cluster once
    // they've finished scanning.
    let clustering = if !args.dry_run && args.limit.is_none() {
        run_clustering(&conn, args.eps, args.min_cluster_size, args.merge_sim, args.min_face_size, args.max_generic_sim)?
    } else {
        None
    };

    if !args.silent {
        eprintln!("{}", format_summary(&result, clustering, args.eps, started.elapsed()));
        if args.limit.is_some() && !args.dry_run {
            let remaining = face_db::scanned_hashes(&conn)?.len();
            eprintln!(
                "partial run (--limit): {remaining} image(s) scanned so far; rerun to continue, then 'videre faces --recluster' to cluster"
            );
        }
    }

    if result.write_errors > 0 || result.detect_errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Assembles the single consolidated summary line printed after both
/// detection and clustering finish. `pub(crate)` since `watch.rs`'s faces
/// stage does not call this one (it has no per-run elapsed-time figure to
/// report - see `format_clustering_only_summary` for its equivalent), but
/// keeping visibility consistent with its sibling function below.
pub(crate) fn format_summary(
    result: &FacesRunResult,
    clustering: Option<ClusteringResult>,
    eps: f32,
    elapsed: std::time::Duration,
) -> String {
    let mut s = format!(
        "{} image(s) processed, {} face(s) found",
        result.images_processed, result.total_faces
    );
    if let Some(c) = &clustering {
        s.push_str(&format!(
            ", {}/{} clustered into {} people (eps={:.2})",
            c.clustered_faces, c.total_faces, c.cluster_count, eps
        ));
    }
    s.push_str(&format!(", done in {}s", elapsed.as_secs()));
    let error_count = result.write_errors + result.detect_errors;
    if error_count > 0 {
        s.push_str(&format!(", {error_count} error(s) (see above)"));
    }
    s
}

/// Assembles the summary line for the `--recluster` (and "nothing new to
/// process, but recluster anyway") path, where no detection ran this
/// invocation - so there is no image count or elapsed-time figure to report.
/// `pub(crate)` since `watch.rs`'s faces stage also calls this.
pub(crate) fn format_clustering_only_summary(clustering: Option<ClusteringResult>, eps: f32) -> String {
    match clustering {
        Some(c) => format!(
            "{}/{} faces clustered into {} people (eps={:.2})",
            c.clustered_faces, c.total_faces, c.cluster_count, eps
        ),
        None => format!("no faces in database to cluster (eps={eps:.2})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_summary_no_errors() {
        let result = FacesRunResult { total_faces: 187, write_errors: 0, images_processed: 234, detect_errors: 0 };
        let clustering = Some(ClusteringResult { total_faces: 187, clustered_faces: 152, cluster_count: 14 });
        let summary = format_summary(&result, clustering, 0.6, std::time::Duration::from_secs(41));
        assert_eq!(
            summary,
            "234 image(s) processed, 187 face(s) found, 152/187 clustered into 14 people (eps=0.60), done in 41s"
        );
    }

    #[test]
    fn format_summary_with_errors() {
        let result = FacesRunResult { total_faces: 187, write_errors: 2, images_processed: 234, detect_errors: 1 };
        let clustering = Some(ClusteringResult { total_faces: 187, clustered_faces: 152, cluster_count: 14 });
        let summary = format_summary(&result, clustering, 0.6, std::time::Duration::from_secs(41));
        assert_eq!(
            summary,
            "234 image(s) processed, 187 face(s) found, 152/187 clustered into 14 people (eps=0.60), done in 41s, 3 error(s) (see above)"
        );
    }

    #[test]
    fn format_summary_no_faces_found() {
        let result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 234, detect_errors: 0 };
        let summary = format_summary(&result, None, 0.6, std::time::Duration::from_secs(41));
        assert_eq!(summary, "234 image(s) processed, 0 face(s) found, done in 41s");
    }

    #[test]
    fn format_clustering_only_summary_some() {
        let clustering = Some(ClusteringResult { total_faces: 187, clustered_faces: 152, cluster_count: 14 });
        let summary = format_clustering_only_summary(clustering, 0.6);
        assert_eq!(summary, "152/187 faces clustered into 14 people (eps=0.60)");
    }

    #[test]
    fn format_clustering_only_summary_none() {
        let summary = format_clustering_only_summary(None, 0.6);
        assert_eq!(summary, "no faces in database to cluster (eps=0.60)");
    }
}
