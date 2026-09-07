use crate::command_context::CommandContext;
use filetime::FileTime;

#[derive(clap::Args)]
pub struct FixDatesArgs {
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

use super::confirm;

pub fn run(args: FixDatesArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // Ordinary writer: coexists with readers and other ordinary work, excluded
    // only by exclusive maintenance.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;

    if args.dry_run && !args.silent {
        eprintln!("Dry run: no files will be modified.");
    }

    let guard = videre_core::library_locks::try_command(&ctx.library, "fix-dates")?;
    let errors =
        videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "fix-dates", || {
            run_fix_dates(&args, ctx, &conn)
        })?;

    if errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// The actual fix-dates work, wrapped by `track()` above. Returns the error
/// count so the caller can decide the exit code after tracking has already
/// finalized the run.
fn run_fix_dates(
    args: &FixDatesArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> anyhow::Result<usize> {
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
            // The row's path is already confined to the selected root by
            // open_existing's containment check; open it through the confined
            // writer and set the time on the handle, so a symlink swapped in
            // after the check is not followed.
            match videre_core::library_io::open_media(&ctx.library, std::path::Path::new(path)) {
                Ok(file) => {
                    if let Err(e) = filetime::set_file_handle_times(&file, None, Some(ft)) {
                        eprintln!("Error: {path}: {e}");
                        errors += 1;
                        continue;
                    }
                }
                Err(e) => {
                    // A missing or offline original is not this run's problem;
                    // skip it silently as before.
                    if let Some(io) = e.downcast_ref::<std::io::Error>() {
                        if io.kind() == std::io::ErrorKind::NotFound {
                            skipped += 1;
                            continue;
                        }
                    }
                    eprintln!("Error: {path}: {e:#}");
                    errors += 1;
                    continue;
                }
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
