use videre::{
    hasher, output, scanner, sqlite_output,
    types::{self, DedupeJson, DupGroupJson, ErrorJson, SimilarGroupJson, SCHEMA_VERSION},
};
use rayon::prelude::*;
use std::path::PathBuf;
use std::process;

#[derive(clap::Args)]
pub struct DedupeArgs {
    /// Directory to scan recursively (default: 'path' from videre config)
    directory: Option<PathBuf>,

    /// JSONL output file (appended). Bare --output targets ~/.videre/hashes.jsonl.
    /// Note: place a bare --output AFTER the directory. Cannot be used with --output-sqlite
    #[arg(long, num_args = 0..=1, conflicts_with = "output_sqlite")]
    output: Option<Option<PathBuf>>,

    /// SQLite output file (upserted by path). When neither --output nor
    /// --output-sqlite is given, records go to the resolved default db
    #[arg(long)]
    output_sqlite: Option<PathBuf>,

    /// Also find visually similar images via perceptual hash
    #[arg(long)]
    similar: bool,

    /// Suppress progress output on stderr (duplicate paths are always written to stdout)
    #[arg(long)]
    silent: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

pub fn run(args: DedupeArgs) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                // stdout must always carry exactly one valid JSON object; the
                // error goes here (not stderr) and we exit before main's eprintln.
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                process::exit(1);
            }
        }
    } else {
        run_text(args)
    }
}

/// Scan, hash (in parallel), and optionally phash. Shared by both output modes;
/// contains no exit calls so the JSON path can also use it. Progress and
/// warnings go to stderr, gated by --silent, exactly as before.
fn gather_records(args: &DedupeArgs, directory: &std::path::Path) -> Vec<types::FileRecord> {
    if !args.silent {
        eprintln!("Scanning {:?}...", directory);
    }

    let paths = scanner::scan(directory);

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

    if args.similar {
        records
            .into_iter()
            .map(|mut r| {
                r.phash = hasher::compute_dhash(std::path::Path::new(&r.path));
                r
            })
            .collect()
    } else {
        records
    }
}

enum OutputTarget {
    Sqlite(PathBuf),
    Jsonl(PathBuf),
}

/// Where records go. Explicit flags behave exactly as before; the bare default
/// is SQLite at the resolved db, and a bare --output is JSONL at the default
/// jsonl path. Defaulted destinations get their parent dir created (that is
/// how ~/.videre comes into existence on first use).
fn output_target(args: &DedupeArgs) -> anyhow::Result<OutputTarget> {
    if let Some(ref db) = args.output_sqlite {
        return Ok(OutputTarget::Sqlite(db.clone()));
    }
    match &args.output {
        Some(Some(path)) => Ok(OutputTarget::Jsonl(path.clone())),
        Some(None) => {
            let path = videre_core::home::default_jsonl()?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(OutputTarget::Jsonl(path))
        }
        None => {
            let db = videre_core::home::resolve_db(None)?;
            if let Some(parent) = db.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(OutputTarget::Sqlite(db))
        }
    }
}

/// The pre-existing text mode, byte-identical: same stderr text, same
/// process::exit(1) sites, same stdout lines.
fn run_text(args: DedupeArgs) -> anyhow::Result<()> {
    let directory = match super::resolve_directory(args.directory.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };
    if !directory.exists() {
        eprintln!("Error: directory {:?} does not exist", directory);
        process::exit(1);
    }

    let records = gather_records(&args, &directory);

    match output_target(&args) {
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
        Ok(OutputTarget::Sqlite(db_path)) => {
            if let Err(e) = sqlite_output::write_records(&records, &db_path) {
                eprintln!("Error writing to {:?}: {}", db_path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
            }
        }
        Ok(OutputTarget::Jsonl(path)) => {
            if let Err(e) = output::append_records(&records, &path) {
                eprintln!("Error writing to {:?}: {}", path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), path);
            }
        }
    }

    // Exact duplicates: print REMOVE candidates to stdout (one path per line)
    let groups = output::find_duplicate_groups(&records);
    if !args.silent {
        if groups.is_empty() {
            eprintln!("No exact duplicates found.");
        } else {
            eprintln!("{} duplicate group(s), {} file(s) to remove.",
                groups.len(),
                groups.iter().map(|g| g.files.len() - 1).sum::<usize>()
            );
        }
    }
    output::print_losers(&groups);

    // Similar groups: informational only: review via videre report before acting
    if args.similar {
        let similar = output::find_similar_groups(&records, 10);
        if !args.silent {
            if similar.is_empty() {
                eprintln!("No visually similar images found.");
            } else {
                eprintln!("{} visually similar group(s) found: review with videre report before deleting.", similar.len());
            }
        }
    }

    Ok(())
}

/// JSON mode: identical pipeline, but every failure becomes Err so run() can
/// emit the error JSON document (text mode's process::exit paths would
/// otherwise kill the process with empty stdout).
fn run_json(args: &DedupeArgs) -> anyhow::Result<DedupeJson> {
    let directory = super::resolve_directory(args.directory.clone())?;
    anyhow::ensure!(
        directory.exists(),
        "directory {:?} does not exist",
        directory
    );

    let records = gather_records(args, &directory);

    match output_target(args)? {
        OutputTarget::Sqlite(db_path) => {
            sqlite_output::write_records(&records, &db_path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))?;
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
            }
        }
        OutputTarget::Jsonl(path) => {
            output::append_records(&records, &path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", path, e))?;
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), path);
            }
        }
    }

    // find_duplicate_groups / find_similar_groups only ever return groups with
    // >= 2 files, which DupGroupJson::from relies on (keep = files[0]).
    let scanned = records.len();
    let duplicate_groups = output::find_duplicate_groups(&records)
        .into_iter()
        .map(DupGroupJson::from)
        .collect();
    let similar_groups = args.similar.then(|| {
        output::find_similar_groups(&records, 10)
            .into_iter()
            .map(SimilarGroupJson::from)
            .collect()
    });

    Ok(DedupeJson {
        schema_version: SCHEMA_VERSION,
        scanned,
        duplicate_groups,
        similar_groups,
    })
}
