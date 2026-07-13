use anyhow::Result;
use clap::Parser;
use videre_core::face_db;
use videre_ml::pipeline::{run_clustering, run_face_pipeline};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dupe-faces", about = "Detect, embed, and cluster faces in a dupe SQLite database.")]
struct Args {
    db: PathBuf,
    #[arg(long)] reprocess: bool,
    /// Skip detection; just re-run clustering on existing embeddings
    #[arg(long)] recluster: bool,
    #[arg(long, default_value = "8")] batch: usize,
    #[arg(long)] dry_run: bool,
    #[arg(long)] silent: bool,
    /// DBSCAN cosine-distance radius (0 = identical, 2 = opposite). Default 0.6.
    #[arg(long, default_value = "0.6")] eps: f32,
    /// Minimum faces per cluster (below this, faces are left as singletons). Default 3.
    #[arg(long, default_value = "3")] min_cluster_size: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if !args.db.exists() {
        anyhow::bail!("{:?} does not exist", args.db);
    }
    let conn = videre_core::db::open_wal(&args.db)?;
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

    let skip_hashes: std::collections::HashSet<String> = if args.reprocess {
        std::collections::HashSet::new()
    } else {
        face_db::hashes_with_faces(&conn)?.into_iter().collect()
    };

    // Gap 1: Deduplicate by hash - one representative path per hash
    let mut seen_hashes = std::collections::HashSet::new();
    let to_process: Vec<(String, String)> = all_paths.into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash) && seen_hashes.insert(hash.clone()))
        .collect();

    if args.recluster || to_process.is_empty() {
        if !args.silent && to_process.is_empty() && !args.recluster {
            eprintln!("All hashes already processed.");
        }
        // Skip detection; jump straight to clustering
        if !args.dry_run {
            run_clustering(&conn, args.eps, args.min_cluster_size, args.silent)?;
        }
        return Ok(());
    }

    let result = run_face_pipeline(&conn, &to_process, args.batch, args.dry_run, args.silent)?;

    // Cluster whenever there are faces in the DB, not only when new faces were found
    if !args.dry_run {
        run_clustering(&conn, args.eps, args.min_cluster_size, args.silent)?;
    }

    if !args.silent { eprintln!("Done: {} new face(s) detected.", result.total_faces); }
    if result.write_errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}
