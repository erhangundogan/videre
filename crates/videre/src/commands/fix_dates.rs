use filetime::FileTime;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct FixDatesArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Preview changes without modifying any files
    #[arg(long)]
    dry_run: bool,

    /// Suppress per-file output (errors are always shown)
    #[arg(long)]
    silent: bool,

    /// Skip the confirmation prompt and proceed immediately
    #[arg(short = 'y', long = "yes")]
    yes: bool,
}

/// Prompts on stderr and reads a yes/no answer from stdin. Any input other
/// than "y"/"yes" (case-insensitive) is treated as "no", including EOF (e.g.
/// stdin piped from /dev/null in a non-interactive context), the safe
/// default for a prompt gating a file mutation.
fn confirm(prompt: &str) -> anyhow::Result<bool> {
    use std::io::Write;
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

pub fn run(args: FixDatesArgs) -> anyhow::Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;

    if !db.exists() {
        eprintln!("Error: {:?} does not exist", db);
        std::process::exit(1);
    }

    if args.dry_run && !args.silent {
        eprintln!("Dry run: no files will be modified.");
    }

    let conn = videre_core::db::open_wal(&db).expect("failed to open database");

    let errors =
        videre_core::pipeline_runs::track(&conn, &db, "fix-dates", || run_fix_dates(&args, &conn))?;

    if errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// The actual fix-dates work, wrapped by `track()` above. Returns the error
/// count so the caller can decide the exit code after tracking has already
/// finalized the run.
fn run_fix_dates(args: &FixDatesArgs, conn: &rusqlite::Connection) -> anyhow::Result<usize> {
    let mut stmt = conn
        .prepare(
            "SELECT path, exif_date FROM file_hashes \
             WHERE exif_date IS NOT NULL \
             ORDER BY path",
        )
        .expect("failed to prepare query");

    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("failed to execute query")
        .filter_map(|r| r.ok())
        .collect();

    let total = rows.len();

    if !args.dry_run && total > 0 && !args.yes {
        let proceed = confirm(&format!(
            "This will set the modified time on {total} file(s) from their exif_date. Continue?"
        ))?;
        if !proceed {
            eprintln!("Aborted; no files modified.");
            return Ok(0);
        }
    }

    let mut changed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for (path, exif_date) in &rows {
        // Parse exif_date: "YYYY-MM-DDTHH:MM:SS" camera-local, no timezone.
        // Treat as local time when converting to a UNIX timestamp.
        let ndt = match chrono::NaiveDateTime::parse_from_str(exif_date, "%Y-%m-%dT%H:%M:%S") {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error: {path}: bad exif_date {exif_date:?}: {e}");
                errors += 1;
                continue;
            }
        };

        use chrono::TimeZone;
        let local_dt = match chrono::Local.from_local_datetime(&ndt).single() {
            Some(d) => d,
            None => {
                eprintln!("Error: {path}: ambiguous local time for {exif_date}");
                errors += 1;
                continue;
            }
        };

        let ft = FileTime::from_unix_time(local_dt.timestamp(), 0);

        if !args.dry_run {
            if let Err(e) = filetime::set_file_mtime(path, ft) {
                if e.kind() == std::io::ErrorKind::NotFound {
                    // File was trashed or moved after the scan; skip silently.
                    skipped += 1;
                    continue;
                }
                eprintln!("Error: {path}: {e}");
                errors += 1;
                continue;
            }
        }

        if !args.silent {
            let prefix = if args.dry_run {
                "[dry-run]"
            } else {
                "[updated]"
            };
            println!("{prefix} {path}  →  {exif_date}");
        }
        changed += 1;
    }

    if !args.silent {
        let skipped_note = if skipped > 0 {
            format!(", {skipped} no longer on disk (skipped)")
        } else {
            String::new()
        };
        eprintln!(
            "{} file(s) with exif_date, {} {}, {} error(s){}.",
            total,
            changed,
            if args.dry_run {
                "would be updated"
            } else {
                "updated"
            },
            errors,
            skipped_note,
        );
    }

    Ok(errors)
}
