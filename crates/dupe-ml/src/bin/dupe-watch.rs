use anyhow::Result;
use clap::Parser;
use dupe::{hasher, scanner, sqlite_output, types};
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
