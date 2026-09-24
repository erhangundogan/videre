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

impl FixDatesArgs {
    /// Pipeline-stage defaults: non-interactive (`--yes`, since the pipeline
    /// cannot answer a per-file prompt), clap defaults otherwise, silence
    /// controlled by the pipeline.
    pub(crate) fn for_pipeline(silent: bool) -> Self {
        #[derive(clap::Parser)]
        struct P {
            #[command(flatten)]
            a: FixDatesArgs,
        }
        let argv: &[&str] = if silent {
            &["fix-dates", "--yes", "--silent"]
        } else {
            &["fix-dates", "--yes"]
        };
        <P as clap::Parser>::parse_from(argv).a
    }
}

pub fn run(args: FixDatesArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // Ordinary writer: coexists with readers and other ordinary work, excluded
    // only by exclusive maintenance.
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;

    if args.dry_run && !args.silent {
        tracing::info!("Dry run: no files will be modified.");
    }

    let guard = videre_core::library_locks::try_command(&ctx.library, "fix-dates")?;
    let errors =
        videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "fix-dates", || {
            run_fix_dates(&args, ctx, &conn)
        })?;

    if errors > 0 {
        return Err(crate::exit::Exit::code(1).into());
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
            "SELECT path, exif_date, modified_at FROM file_hashes \
             WHERE exif_date IS NOT NULL \
             ORDER BY path",
        )
        .expect("failed to prepare query");

    let all: Vec<(String, String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("failed to execute query")
        .filter_map(|r| r.ok())
        .collect();

    let total = all.len();
    // A row whose modified time already equals its camera date is left alone:
    // rewriting it changes nothing but still touches the file and reported it
    // as updated, so a status suggesting 8 files became a run "updating" 876.
    // The same test `videre status` counts with, so the two always agree.
    let (current, rows): (Vec<_>, Vec<_>) = all.into_iter().partition(|(_, exif, modified)| {
        videre_core::fix_dates_target::is_current(exif, modified.as_deref()) == Some(true)
    });
    let already_correct = current.len();
    let rows: Vec<(String, String)> = rows.into_iter().map(|(p, e, _)| (p, e)).collect();
    let to_change = rows.len();

    if !args.dry_run && to_change > 0 && !args.yes {
        let proceed = confirm(&format!(
            "This will set the modified time on {to_change} file(s) from their exif_date. Continue?"
        ))?;
        if !proceed {
            tracing::info!("Aborted; no files modified.");
            return Ok(0);
        }
    }

    let mut changed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for (path, exif_date) in &rows {
        // The target mtime comes from the one shared definition of the
        // exif_date -> timestamp transform, so status's "N rows would change"
        // count and this command can never disagree about what changes.
        let Some(local_dt) = (|| {
            let s = videre_core::fix_dates_target::target_modified_at(exif_date)?;
            chrono::DateTime::parse_from_rfc3339(&s).ok()
        })() else {
            let ndt_ok =
                chrono::NaiveDateTime::parse_from_str(exif_date, "%Y-%m-%dT%H:%M:%S").is_ok();
            if ndt_ok {
                tracing::error!("{path}: ambiguous local time for {exif_date}");
            } else {
                tracing::error!("{path}: bad exif_date {exif_date:?}");
            }
            errors += 1;
            continue;
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
                        tracing::error!("{path}: {e}");
                        errors += 1;
                        continue;
                    }
                    // Keep the stored modified_at in step with the mtime just
                    // written, so an incremental scan does not treat every
                    // fix-dated file as changed and re-hash the whole library.
                    // Re-read the handle rather than reusing `ft`, so the stored
                    // value is exactly what a later scan will read (a filesystem
                    // that rounds the mtime then still compares equal).
                    if let Ok(mtime) = file.metadata().and_then(|m| m.modified()) {
                        let _ = conn.execute(
                            "UPDATE file_hashes SET modified_at = ?1 WHERE path = ?2",
                            rusqlite::params![videre_core::db::mtime_iso(mtime), path],
                        );
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
                    tracing::error!("{path}: {e:#}");
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
        tracing::info!(
            "{} file(s) with exif_date, {} {}, {} already correct, {} error(s){}.",
            total,
            changed,
            if args.dry_run {
                "would be updated"
            } else {
                "updated"
            },
            already_correct,
            errors,
            skipped_note,
        );
    }

    Ok(errors)
}
