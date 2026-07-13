use anyhow::Result;
use clap::Parser;
use dupe::{hasher, scanner, sqlite_output, types};
use dupe_core::{db, face_db};
use dupe_ml::pipeline::{run_clustering, run_face_pipeline};
use rayon::prelude::*;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "dupe-watch", about = "Periodically populate the scan/faces/HEIC-cache/location pipeline in the background")]
struct Args {
    /// Directory to scan recursively
    directory: PathBuf,

    /// SQLite database to populate (same file dupe-report reads)
    #[arg(long)]
    output_sqlite: PathBuf,

    /// Re-run the scan/hash/EXIF pipeline each cycle
    #[arg(long)]
    scan: bool,
    /// Run incremental face detection each cycle
    #[arg(long)]
    faces: bool,
    /// Pre-convert and cache HEIC thumbnails each cycle
    #[arg(long)]
    heic: bool,
    /// Pre-resolve reverse-geocoded location names each cycle
    #[arg(long)]
    location: bool,

    /// Seconds between cycles
    #[arg(long, default_value = "300")]
    interval: u64,

    #[arg(long)]
    silent: bool,
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    if !args.directory.exists() {
        anyhow::bail!("{:?} does not exist", args.directory);
    }
    // If no stage flags were passed, run all four - the common case is
    // "just keep everything up to date", not memorizing four flags.
    if !(args.scan || args.faces || args.heic || args.location) {
        args.scan = true;
        args.faces = true;
        args.heic = true;
        args.location = true;
    }

    loop {
        if !args.silent {
            eprintln!("dupe-watch: cycle starting ({})", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"));
        }
        if let Err(e) = run_cycle(&args) {
            eprintln!("dupe-watch: cycle error: {e}");
        }
        if !args.silent {
            eprintln!("dupe-watch: sleeping {}s", args.interval);
        }
        std::thread::sleep(Duration::from_secs(args.interval));
    }
}

fn run_cycle(args: &Args) -> Result<()> {
    if args.scan {
        run_scan_stage(args)?;
    }
    if args.faces || args.heic || args.location {
        // These three stages all read file_hashes; open once and reuse.
        let conn = db::open_wal(&args.output_sqlite)?;
        face_db::create_faces_table(&conn)?;
        if args.faces {
            run_faces_stage(args, &conn)?;
        }
        if args.heic {
            run_heic_stage(args, &conn)?;
        }
        // location stage added in Task 10
    }
    Ok(())
}

fn run_faces_stage(args: &Args, conn: &rusqlite::Connection) -> Result<()> {
    let all_paths: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT path, hash FROM file_hashes WHERE ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')"
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let skip_hashes: std::collections::HashSet<String> =
        face_db::hashes_with_faces(conn)?.into_iter().collect();
    let mut seen_hashes = std::collections::HashSet::new();
    let to_process: Vec<(String, String)> = all_paths
        .into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash) && seen_hashes.insert(hash.clone()))
        .collect();

    if !to_process.is_empty() {
        let result = run_face_pipeline(conn, &to_process, 8, false, args.silent)?;
        if !args.silent {
            eprintln!(
                "dupe-watch: faces stage processed {} new hash(es), {} face(s)",
                to_process.len(),
                result.total_faces
            );
        }
    }
    run_clustering(conn, 0.6, 3, args.silent)?;
    Ok(())
}

fn run_heic_stage(args: &Args, conn: &rusqlite::Connection) -> Result<()> {
    let heic_paths: Vec<(String, String)> = {
        let mut stmt = conn.prepare("SELECT path, hash FROM file_hashes WHERE ext = 'heic'")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut converted = 0usize;
    let mut seen = std::collections::HashSet::new();
    for (path, hash) in heic_paths {
        if !seen.insert(hash.clone()) { continue; } // one representative path per hash
        if dupe_core::thumb_cache::thumb_exists(&hash, 240) && dupe_core::thumb_cache::thumb_exists(&hash, 1200) {
            continue;
        }
        std::fs::create_dir_all(dupe_core::thumb_cache::cache_dir()).ok();
        for size in [240u32, 1200] {
            if dupe_core::thumb_cache::thumb_exists(&hash, size) { continue; }
            if let Some(img) = dupe_core::heic::heic_via_quicklook(&path, &format!("watch{size}")) {
                let img = if img.width() > size || img.height() > size {
                    img.resize(size, size, image::imageops::FilterType::Triangle)
                } else {
                    img
                };
                if img.save(dupe_core::thumb_cache::thumb_path(&hash, size)).is_ok() {
                    converted += 1;
                }
            }
        }
    }
    if !args.silent && converted > 0 {
        eprintln!("dupe-watch: heic stage cached {converted} thumbnail(s)");
    }
    Ok(())
}

fn run_scan_stage(args: &Args) -> Result<()> {
    let paths = scanner::scan(&args.directory);
    let records: Vec<types::FileRecord> = paths
        .par_iter()
        .filter_map(|path| hasher::hash_file(path).ok())
        .collect();
    sqlite_output::write_records(&records, &args.output_sqlite)?;
    if !args.silent {
        eprintln!("dupe-watch: scan stage wrote {} record(s)", records.len());
    }
    Ok(())
}
