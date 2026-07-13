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
        dupe_core::location::ensure_location_column(&conn);
        if args.faces {
            run_faces_stage(args, &conn)?;
        }
        if args.heic {
            run_heic_stage(args, &conn)?;
        }
        if args.location {
            run_location_stage(args, &conn)?;
        }
    }
    Ok(())
}

/// Queries (path, hash) pairs from file_hashes matching a SQL WHERE clause,
/// deduped to one representative path per hash.
fn dedup_paths_by_hash(conn: &rusqlite::Connection, where_clause: &str) -> Result<Vec<(String, String)>> {
    let sql = format!("SELECT path, hash FROM file_hashes WHERE {where_clause}");
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seen = std::collections::HashSet::new();
    Ok(rows.into_iter().filter(|(_, hash)| seen.insert(hash.clone())).collect())
}

fn run_faces_stage(args: &Args, conn: &rusqlite::Connection) -> Result<()> {
    let all_paths = dedup_paths_by_hash(
        conn,
        "ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')",
    )?;
    let skip_hashes: std::collections::HashSet<String> =
        face_db::hashes_with_faces(conn)?.into_iter().collect();
    let to_process: Vec<(String, String)> = all_paths
        .into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash))
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
    let heic_paths = dedup_paths_by_hash(conn, "ext = 'heic'")?;
    let mut converted = 0usize;
    let mut failed = 0usize;
    for (path, hash) in heic_paths {
        let need_240 = !dupe_core::thumb_cache::thumb_exists(&hash, 240);
        let need_1200 = !dupe_core::thumb_cache::thumb_exists(&hash, 1200);
        if !need_240 && !need_1200 {
            continue;
        }
        std::fs::create_dir_all(dupe_core::thumb_cache::cache_dir()).ok();
        // Convert once, then downscale the same in-memory image for each
        // missing size (largest first) instead of re-running QuickLook per size.
        match dupe_core::heic::heic_via_quicklook(&path, "watch") {
            Some(img) => {
                for size in [1200u32, 240] {
                    let need = if size == 240 { need_240 } else { need_1200 };
                    if !need {
                        continue;
                    }
                    let resized = if img.width() > size || img.height() > size {
                        img.resize(size, size, image::imageops::FilterType::Triangle)
                    } else {
                        img.clone()
                    };
                    if resized.save(dupe_core::thumb_cache::thumb_path(&hash, size)).is_ok() {
                        converted += 1;
                    } else {
                        failed += 1;
                    }
                }
            }
            None => {
                if need_240 {
                    failed += 1;
                }
                if need_1200 {
                    failed += 1;
                }
            }
        }
    }
    if !args.silent && (converted > 0 || failed > 0) {
        if failed > 0 {
            eprintln!("dupe-watch: heic stage cached {converted} thumbnail(s), {failed} failed");
        } else {
            eprintln!("dupe-watch: heic stage cached {converted} thumbnail(s)");
        }
    }
    Ok(())
}

fn run_location_stage(args: &Args, conn: &rusqlite::Connection) -> Result<()> {
    let unresolved: Vec<(f64, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT gps_lat, gps_lon FROM file_hashes \
             WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL AND location_name IS NULL"
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    let mut resolved = 0usize;
    for (lat, lon) in unresolved {
        if let Some(name) = dupe_core::location::location_name(lat, lon) {
            conn.execute(
                "UPDATE file_hashes SET location_name = ?1 \
                 WHERE ROUND(gps_lat, 6) = ROUND(?2, 6) AND ROUND(gps_lon, 6) = ROUND(?3, 6)",
                rusqlite::params![name, lat, lon],
            )?;
            resolved += 1;
        }
    }
    if !args.silent && resolved > 0 {
        eprintln!("dupe-watch: location stage resolved {resolved} coordinate(s)");
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
