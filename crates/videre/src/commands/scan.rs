use videre::{
    hasher, output, scanner, sqlite_output,
    types::{ErrorJson, ScanJson, ScanOutputJson, SCHEMA_VERSION},
};
use rayon::prelude::*;
use std::path::PathBuf;
use std::process;

#[derive(clap::Args)]
pub struct ScanArgs {
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

    /// Also compute and store perceptual hashes for near-duplicate detection
    #[arg(long)]
    similar: bool,

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
fn gather_records(args: &ScanArgs, directory: &std::path::Path) -> (Vec<videre::types::FileRecord>, usize) {
    let paths = scanner::scan(directory);
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
                r.phash = hasher::compute_dhash(std::path::Path::new(&r.path));
                r
            })
            .collect()
    } else {
        records
    };

    (records, skipped)
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

    let (records, skipped) = gather_records(&args, &directory);

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
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", db_path)));
            }
        }
        Ok(OutputTarget::Jsonl(path)) => {
            if let Err(e) = output::append_records(&records, &path) {
                eprintln!("Error writing to {:?}: {}", path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", path)));
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

    let (records, skipped) = gather_records(args, &directory);

    let output = match output_target(args)? {
        OutputTarget::Sqlite(db_path) => {
            sqlite_output::write_records(&records, &db_path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))?;
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", db_path)));
            }
            ScanOutputJson { kind: "sqlite", path: db_path.display().to_string() }
        }
        OutputTarget::Jsonl(path) => {
            output::append_records(&records, &path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", path, e))?;
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", path)));
            }
            ScanOutputJson { kind: "jsonl", path: path.display().to_string() }
        }
    };

    Ok(ScanJson {
        schema_version: SCHEMA_VERSION,
        total_files: records.len(),
        output,
    })
}
