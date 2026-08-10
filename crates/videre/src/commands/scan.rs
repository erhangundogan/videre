use rayon::prelude::*;
use std::path::PathBuf;
use std::process;
use videre::{
    hasher, output, scanner, sqlite_output,
    types::{ErrorJson, ScanJson, ScanOutputJson, SCHEMA_VERSION},
};

#[derive(clap::Args)]
pub struct ScanArgs {
    /// Directory to scan recursively (default: 'path' from videre config)
    directory: Option<PathBuf>,

    /// JSONL output file (appended). Bare --output targets ~/.videre/hashes.jsonl.
    /// Note: place a bare --output AFTER the directory. Cannot be used with --db
    #[arg(long, num_args = 0..=1, conflicts_with = "db")]
    output: Option<Option<PathBuf>>,

    /// SQLite database to write (upserted by path). Without this, and without
    /// --output, records go to the resolved default db.
    ///
    /// `--output-sqlite` is the original name, kept working: this command
    /// predates the `--db` every reader uses, from when JSONL and SQLite were
    /// peer output *formats* rather than one destination and one opt-out.
    #[arg(long, alias = "output-sqlite")]
    db: Option<PathBuf>,

    /// Also compute and store perceptual hashes for near-duplicate detection
    #[arg(long)]
    similar: bool,

    /// Only process files no scan has finished: those with no database row, or
    /// a row with no recorded type. Needs a SQLite destination.
    #[arg(long, conflicts_with = "output")]
    retry_incomplete: bool,

    /// Suppress progress output on stderr
    #[arg(long)]
    silent: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

pub fn run(args: ScanArgs) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
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
/// warnings go to stderr, gated by --silent (except hash-failure warnings,
/// which always print via `Progress::println`). Returns the records plus the
/// count of files that were scanned but failed to hash.
/// The `--retry-incomplete` summary, printed before the write summary.
///
/// Three counts because the flag's value is knowing whether it worked: how
/// many were incomplete, how many got a type, and how many remain
/// unidentifiable. That last number now carries the sentinel, so a second run
/// reports 0 incomplete rather than repeating the work.
fn format_retry_summary(
    walked: usize,
    records: &[videre::types::FileRecord],
    skipped: usize,
) -> String {
    let unresolved = records
        .iter()
        .filter(|r| r.mime.as_deref() == Some(videre_core::mime_probe::UNKNOWN_MIME))
        .count();
    format!(
        "{walked} file(s) walked, {} incomplete; {} processed, {} identified, {unresolved} still unrecognised",
        records.len() + skipped,
        records.len(),
        records.len() - unresolved,
    )
}

/// Paths to skip under `--retry-incomplete`, empty when the flag is off.
///
/// Resolved before gathering because `gather_records` has no database handle;
/// the connection is opened later in `run`. A missing database is an empty
/// set: nothing has been scanned, so nothing is complete.
fn completed_paths(args: &ScanArgs) -> std::collections::HashSet<String> {
    if !args.retry_incomplete {
        return std::collections::HashSet::new();
    }
    // --retry-incomplete conflicts with --output, so the target is always
    // SQLite here, but fall back to an empty set rather than panicking if
    // that ever changes.
    let Ok(OutputTarget::Sqlite(db_path)) = output_target(args) else {
        return std::collections::HashSet::new();
    };
    if !db_path.exists() {
        return std::collections::HashSet::new();
    }
    videre_core::db::open_wal(&db_path)
        .ok()
        .and_then(|conn| videre_core::db::paths_with_known_mime(&conn).ok())
        .unwrap_or_default()
}

fn gather_records(
    args: &ScanArgs,
    directory: &std::path::Path,
    completed: &std::collections::HashSet<String>,
) -> (Vec<videre::types::FileRecord>, usize, usize) {
    let all_paths = scanner::scan(directory);
    let walked = all_paths.len();
    let paths: Vec<_> = all_paths
        .into_iter()
        .filter(|p| !completed.contains(&p.to_string_lossy().to_string()))
        .collect();
    let progress = videre_core::progress::Progress::new(paths.len() as u64, args.silent);

    let records: Vec<_> = paths
        .par_iter()
        .filter_map(|path| {
            let result = hasher::hash_file(path)
                .map_err(|e| {
                    progress.println(&format!("Warning: skipping {:?}: {}", path, e));
                })
                .ok();
            progress.tick();
            result
        })
        .collect();

    progress.finish();

    let skipped = paths.len() - records.len();

    let records = if args.similar {
        records
            .into_iter()
            .map(|mut r| {
                r.phash = hasher::compute_dhash(std::path::Path::new(&r.path), r.mime.as_deref());
                r
            })
            .collect()
    } else {
        records
    };

    (records, skipped, walked)
}

/// Formats the "Wrote N record(s) to <path>" summary line, with an
/// "(M skipped)" suffix when `skipped > 0`, omitted entirely when `skipped
/// == 0` (matching `videre embed`'s equivalent omit-when-zero precedent).
fn format_write_summary(written: usize, skipped: usize, dest: &str) -> String {
    if skipped > 0 {
        format!("Wrote {written} record(s) to {dest} ({skipped} skipped)")
    } else {
        format!("Wrote {written} record(s) to {dest}")
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
fn output_target(args: &ScanArgs) -> anyhow::Result<OutputTarget> {
    if let Some(ref db) = args.db {
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

/// Text mode: stdout is always empty (progress is on stderr; duplicate
/// reporting is `dedupe`'s job now, not scan's).
fn run_text(args: ScanArgs) -> anyhow::Result<()> {
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
    super::maybe_adopt_default_path(args.directory.as_deref(), args.silent);

    let completed = completed_paths(&args);
    let (records, skipped, walked) = gather_records(&args, &directory, &completed);

    if args.retry_incomplete && !args.silent {
        eprintln!("{}", format_retry_summary(walked, &records, skipped));
    }

    match output_target(&args) {
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
        Ok(OutputTarget::Sqlite(db_path)) => {
            let conn = match videre_core::db::open_wal(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error opening {:?}: {}", db_path, e);
                    process::exit(1);
                }
            };
            if let Err(e) = videre_core::pipeline_runs::install_sigint_handler(&db_path, "scan") {
                eprintln!("Warning: could not install interrupt handler: {e:#}");
            }
            let write_result = videre_core::pipeline_runs::track(&conn, &db_path, "scan", || {
                sqlite_output::write_records(&records, &db_path)
                    .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))
            });
            if let Err(e) = write_result {
                eprintln!("Error: {e:#}");
                process::exit(1);
            }
            if !args.silent {
                eprintln!(
                    "{}",
                    format_write_summary(records.len(), skipped, &format!("{:?}", db_path))
                );
            }
        }
        Ok(OutputTarget::Jsonl(path)) => {
            if let Err(e) = output::append_records(&records, &path) {
                eprintln!("Error writing to {:?}: {}", path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!(
                    "{}",
                    format_write_summary(records.len(), skipped, &format!("{:?}", path))
                );
            }
        }
    }

    Ok(())
}

/// JSON mode: identical pipeline, but every failure becomes Err so run() can
/// emit the error JSON document (text mode's process::exit paths would
/// otherwise kill the process with empty stdout).
fn run_json(args: &ScanArgs) -> anyhow::Result<ScanJson> {
    let directory = super::resolve_directory(args.directory.clone())?;
    anyhow::ensure!(
        directory.exists(),
        "directory {:?} does not exist",
        directory
    );
    super::maybe_adopt_default_path(args.directory.as_deref(), args.silent);

    let completed = completed_paths(args);
    let (records, skipped, walked) = gather_records(args, &directory, &completed);

    if args.retry_incomplete && !args.silent {
        eprintln!("{}", format_retry_summary(walked, &records, skipped));
    }

    let output = match output_target(args)? {
        OutputTarget::Sqlite(db_path) => {
            let conn = videre_core::db::open_wal(&db_path)
                .map_err(|e| anyhow::anyhow!("opening {:?}: {}", db_path, e))?;
            videre_core::pipeline_runs::track(&conn, &db_path, "scan", || {
                sqlite_output::write_records(&records, &db_path)
                    .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))
            })?;
            if !args.silent {
                eprintln!(
                    "{}",
                    format_write_summary(records.len(), skipped, &format!("{:?}", db_path))
                );
            }
            ScanOutputJson {
                kind: "sqlite",
                path: db_path.display().to_string(),
            }
        }
        OutputTarget::Jsonl(path) => {
            output::append_records(&records, &path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", path, e))?;
            if !args.silent {
                eprintln!(
                    "{}",
                    format_write_summary(records.len(), skipped, &format!("{:?}", path))
                );
            }
            ScanOutputJson {
                kind: "jsonl",
                path: path.display().to_string(),
            }
        }
    };

    Ok(ScanJson {
        schema_version: SCHEMA_VERSION,
        total_files: records.len(),
        output,
    })
}
