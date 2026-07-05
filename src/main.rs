mod hasher;
mod output;
mod scanner;
mod sqlite_output;
mod types;

use clap::Parser;
use rayon::prelude::*;
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(name = "dupe", version, about = "Find duplicate images in a directory")]
struct Args {
    /// Directory to scan recursively
    directory: PathBuf,

    /// JSONL output file (appended); cannot be used with --output-sqlite
    #[arg(long, default_value = "/tmp/hashes", conflicts_with = "output_sqlite")]
    output: PathBuf,

    /// SQLite output file (upserted by path); cannot be used with --output
    #[arg(long)]
    output_sqlite: Option<PathBuf>,

    /// Also find visually similar images via perceptual hash
    #[arg(long)]
    similar: bool,

    /// Suppress console output
    #[arg(long)]
    silent: bool,
}

fn main() {
    let args = Args::parse();

    if !args.directory.exists() {
        eprintln!("Error: directory {:?} does not exist", args.directory);
        process::exit(1);
    }

    if !args.silent {
        eprintln!("Scanning {:?}...", args.directory);
    }

    let paths = scanner::scan(&args.directory);

    if !args.silent {
        eprintln!("Found {} file(s) to process", paths.len());
    }

    let silent = args.silent;
    let records: Vec<_> = paths
        .par_iter()
        .filter_map(|path| {
            hasher::hash_file(path)
                .map_err(|e| {
                    if !silent {
                        eprintln!("Warning: skipping {:?}: {}", path, e);
                    }
                })
                .ok()
        })
        .collect();

    // Compute pHash for each file if --similar requested
    let records: Vec<types::FileRecord> = if args.similar {
        records
            .into_iter()
            .map(|mut r| {
                r.phash = hasher::compute_dhash(std::path::Path::new(&r.path));
                r
            })
            .collect()
    } else {
        records
    };

    if let Some(ref db_path) = args.output_sqlite {
        if let Err(e) = sqlite_output::write_records(&records, db_path) {
            eprintln!("Error writing to {:?}: {}", db_path, e);
            process::exit(1);
        }
        if !args.silent {
            eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
        }
    } else {
        if let Err(e) = output::append_records(&records, &args.output) {
            eprintln!("Error writing to {:?}: {}", args.output, e);
            process::exit(1);
        }
        if !args.silent {
            eprintln!("Wrote {} record(s) to {:?}", records.len(), args.output);
        }
    }

    if !args.silent {
        let groups = output::find_duplicate_groups(&records);
        if groups.is_empty() {
            eprintln!("No exact duplicates found.");
        } else {
            output::print_duplicate_groups(&groups);
        }

        if args.similar {
            let similar = output::find_similar_groups(&records, 10);
            if similar.is_empty() {
                eprintln!("No visually similar images found.");
            } else {
                output::print_similar_groups(&similar);
            }
        }
    }
}
