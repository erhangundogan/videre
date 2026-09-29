use crate::command_context::CommandContext;
use videre::types::ErrorJson;

#[derive(clap::Args)]
pub struct DedupeArgs {
    /// Also report perceptual-hash near-duplicate clusters (review-only)
    #[arg(long)]
    similar: bool,

    /// Suppress progress output on stderr (duplicate paths are always written to stdout)
    #[arg(long)]
    silent: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,

    /// Also write the duplicate groups to a browsable HTML page.
    /// Bare --html targets <db>_duplicates.html.
    #[arg(long, num_args = 0..=1)]
    html: Option<Option<std::path::PathBuf>>,

    /// Move the duplicate copies to the system trash (recoverable), instead of
    /// only listing them. videre deletes them itself, so no shell pipeline and
    /// no word-splitting on paths that contain spaces.
    #[arg(long)]
    remove: bool,

    /// With --remove, list what would be removed and delete nothing.
    #[arg(long)]
    dry_run: bool,

    /// With --remove, skip the confirmation prompt.
    #[arg(long)]
    yes: bool,

    /// With --remove, proceed even when the count trips the bulk-deletion guard.
    #[arg(long)]
    force: bool,

    /// Also pair Google Takeout edits with their originals (`a-edited.jpg`
    /// beside `a.jpg` in one folder). With --remove, the edits go to the
    /// trash and the originals stay.
    #[arg(long)]
    edited: bool,

    /// Print duplicate paths NUL-delimited instead of newline-delimited, so
    /// `videre dedupe --print0 | xargs -0 trash` is safe for paths with spaces.
    #[arg(long)]
    print0: bool,
}

pub fn run(args: DedupeArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    // --remove is the human deletion path; --json and --similar are report
    // surfaces. Combining them is ambiguous (delete the JSON? delete the
    // review-only near-duplicates?), so reject rather than guess.
    if args.remove && args.json {
        anyhow::bail!("--remove cannot be combined with --json; --json only reports");
    }
    if args.remove && args.similar {
        anyhow::bail!(
            "--remove only removes exact duplicates; --similar groups are review-only \
             (use 'videre dedupe --similar --html' to review them)"
        );
    }
    if args.remove && args.html.is_some() {
        anyhow::bail!(
            "--remove cannot be combined with --html; --html writes a review page. \
             Review first, then run 'videre dedupe --remove'"
        );
    }
    if args.json {
        match run_json(&args, ctx) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                Err(crate::exit::Exit::shown(e).into())
            }
        }
    } else {
        run_text(args, ctx)
    }
}

/// `--html`: the same duplicate groups, as a page you can keep.
///
/// Static on purpose. `videre gallery` is for browsing a library and writes
/// nothing; this renders the set the command just produced, so it survives the
/// process and can be archived or opened later.
fn write_html(
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    arg: Option<&std::path::Path>,
    edited: bool,
) -> anyhow::Result<()> {
    let db = &ctx.library.paths.db;
    // A bare --html targets a page beside the selected database; an explicit
    // relative path is an operand, resolved against the launch directory.
    let output = match arg {
        Some(p) => ctx.operand(p),
        None => {
            let mut p = db.clone();
            let stem = db
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            p.set_file_name(format!("{stem}_duplicates.html"));
            p
        }
    };
    let groups = crate::render::query_groups(conn);
    let edited_groups = if edited {
        crate::render::query_edited_groups(conn)
    } else {
        Vec::new()
    };
    crate::render::write_static_page(conn, &output, &groups, &edited_groups, None)
}

fn run_text(args: DedupeArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "dedupe")?;

    let moved =
        videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "dedupe", || {
            if args.remove {
                run_remove(&args, ctx, &conn)
            } else {
                run_dedupe_text(&args, &conn).map(|_| 0usize)
            }
        })?;

    // `--remove` trashed duplicate copies, and their database rows now
    // describe files that no longer exist: run the same cleanup `videre
    // prune` would, so the library never shows a ghost. The prune pass needs
    // the exclusive activity lease, which conflicts with the shared one this
    // command holds, so the shared lease is released first. Best effort: the
    // removal itself already succeeded, and a user can always run
    // `videre prune` by hand.
    if args.remove && !args.dry_run && moved > 0 {
        drop(activity);
        let prune_args = super::prune::PruneArgs::for_watch_stage(args.silent);
        match crate::command_context::with_tracked_command(
            ctx,
            "prune",
            videre_core::library_locks::ActivityMode::Exclusive,
            |conn| super::prune::run_prune(&prune_args, &ctx.library, conn),
        ) {
            Ok(0) => {}
            Ok(errors) => tracing::warn!("prune finished with {errors} error(s)."),
            Err(e) => {
                tracing::warn!("automatic prune failed: {e:#}; run 'videre prune' to clean up.")
            }
        }
    }

    if let Some(arg) = args.html.as_ref() {
        write_html(ctx, &conn, arg.as_deref(), args.edited)?;
    }
    Ok(())
}

/// Write the removable loser paths to stdout, NUL-delimited when `print0` so a
/// `| xargs -0` consumer is safe for paths containing spaces, else one per line.
fn print_paths_delimited(paths: &[std::path::PathBuf], print0: bool) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for path in paths {
        let _ = if print0 {
            write!(out, "{}\0", path.display())
        } else {
            writeln!(out, "{}", path.display())
        };
    }
}

/// What a run removes: the exact-duplicate losers, then, with `--edited`,
/// each Google Takeout edit not already among them. Kept apart because only
/// the exact losers count toward the bulk-deletion guard: an edit is paired
/// only when its original is in the library too, so a large count is what a
/// Takeout export looks like, not a sign of a mistake.
struct Removals {
    exact: Vec<std::path::PathBuf>,
    edits: Vec<std::path::PathBuf>,
}

impl Removals {
    /// The two decisions are made together, not side by side:
    ///
    /// - An edit goes only while its original is still on disk. The pairing
    ///   comes from rows, and an original deleted since the last scan would
    ///   otherwise leave the edit as the only file, then take that too.
    /// - An exact group keeps its first file that is not itself an edit being
    ///   removed. Exact dedupe alone may keep an edit (the oldest copy), and
    ///   removing that edit as well would leave its content with no copy.
    fn collect(records: &[videre::types::FileRecord], edited: bool) -> Removals {
        use std::path::{Path, PathBuf};
        let edits: Vec<PathBuf> = if edited {
            videre::output::edited_losers(records)
                .into_iter()
                .filter(|(original, _)| Path::new(original).exists())
                .map(|(_, edit)| PathBuf::from(edit))
                .collect()
        } else {
            Vec::new()
        };
        let going: std::collections::HashSet<&Path> = edits.iter().map(PathBuf::as_path).collect();
        let mut exact = Vec::new();
        for group in videre::output::find_duplicate_groups(records) {
            let paths: Vec<&Path> = group.files.iter().map(|f| Path::new(&f.path)).collect();
            // Every copy is an edit whose original stays: all of them go.
            let keeper = paths.iter().position(|p| !going.contains(p));
            for (i, path) in paths.into_iter().enumerate() {
                if Some(i) != keeper && !going.contains(path) {
                    exact.push(path.to_path_buf());
                }
            }
        }
        Removals { exact, edits }
    }

    fn all(&self) -> Vec<std::path::PathBuf> {
        self.exact.iter().chain(&self.edits).cloned().collect()
    }

    /// `3 exact duplicate(s) and 2 Google Photos edit(s)`, leaving out a zero part.
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.exact.is_empty() {
            parts.push(format!("{} exact duplicate(s)", self.exact.len()));
        }
        if !self.edits.is_empty() {
            parts.push(format!("{} Google Photos edit(s)", self.edits.len()));
        }
        parts.join(" and ")
    }
}

/// `--remove`: videre moves the duplicate copies to the system trash itself.
/// Safe by default: refuses on a missing library volume, refuses an implausibly
/// large deletion without --force, confirms unless --yes, and `--dry-run` only
/// previews. Operates only on what `Removals` computes, which is also what
/// the report prints: an exact group's kept copy is never removed, and an edit
/// goes only while its original is on disk.
fn run_remove(
    args: &DedupeArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> anyhow::Result<usize> {
    let records = videre::sqlite_output::load_records_from(conn)
        .map_err(|e| anyhow::anyhow!("reading the library database: {e}"))?;
    let total = records.len();
    let volume_missing = || {
        anyhow::anyhow!(
            "library root {:?} is not available (is the drive connected?); removed nothing",
            ctx.library.paths.root
        )
    };
    // Missing-volume refusal: never read "the files are gone" as "everything is
    // a duplicate to remove". With --edited it comes before anything is
    // counted, because pairing checks each original on disk: a detached drive
    // would otherwise empty the set and report "Nothing to remove."
    if args.edited && !ctx.library.paths.root.is_dir() {
        return Err(volume_missing());
    }
    let removals = Removals::collect(&records, args.edited);
    let losers = removals.all();

    if losers.is_empty() {
        if !args.silent {
            if args.edited {
                tracing::info!("Nothing to remove.");
            } else {
                tracing::info!("No exact duplicates to remove.");
            }
        }
        return Ok(0);
    }

    // Check before the guard and before any deletion.
    if !ctx.library.paths.root.is_dir() {
        return Err(volume_missing());
    }

    // Bulk-deletion guard, shared with prune: an implausibly large share of the
    // library is more likely a wrong selection or a mounting accident.
    // Only exact duplicates count: see `Removals`.
    let exact = removals.exact.len();
    if !args.force && super::is_bulk_delete(exact, total) {
        tracing::warn!(
            "refusing to remove {} of {} file(s) ({:.0}% of the library): \
             that is more likely a mistake than real duplicates.",
            exact,
            total,
            (exact as f64 / total.max(1) as f64) * 100.0
        );
        tracing::warn!("  nothing was removed; re-run with --force if this is intended");
        return Ok(0);
    }

    if args.dry_run {
        print_paths_delimited(&losers, args.print0);
        if !args.silent {
            tracing::info!("{} would be moved to the trash.", removals.describe());
            // Stdout stays the media paths alone, for `--print0 | xargs -0`.
            let sidecars = crate::removal::sidecars_of(&losers);
            if !sidecars.is_empty() {
                tracing::info!(
                    "{} XMP sidecar(s) would go with them: {}",
                    sidecars.len(),
                    sidecars
                        .iter()
                        .map(|s| s.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        return Ok(0);
    }

    if !args.yes
        && !super::confirm(&format!(
            "This will move {} to the trash. Continue?",
            removals.describe()
        ))?
    {
        tracing::info!("Aborted; nothing was removed.");
        return Ok(0);
    }

    let results = crate::removal::trash_paths(&losers);
    let moved = results.iter().filter(|(_, r)| r.is_ok()).count();
    let skipped = results.len() - moved;
    for (path, r) in &results {
        if let Err(e) = r {
            tracing::warn!("could not trash {path:?}: {e}");
        }
    }
    if moved == 0 && skipped > 0 {
        anyhow::bail!(
            "could not move any of the {skipped} file(s) to the trash (is the drive connected?)"
        );
    }
    if !args.silent {
        if skipped > 0 {
            tracing::info!("Moved {moved} file(s) to the trash ({skipped} skipped).");
        } else {
            tracing::info!("Moved {moved} file(s) to the trash.");
        }
    }
    Ok(moved)
}

/// The actual dedupe-reporting work, wrapped by `track_in()` above.
fn run_dedupe_text(args: &DedupeArgs, conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let records = videre::sqlite_output::load_records_from(conn)
        .map_err(|e| anyhow::anyhow!("reading the library database: {e}"))?;

    let groups = videre::output::find_duplicate_groups(&records);
    let removals = Removals::collect(&records, args.edited);
    if !args.silent {
        if groups.is_empty() {
            tracing::info!("No exact duplicates found.");
        } else {
            tracing::info!(
                "{} duplicate group(s), {} file(s) to remove.",
                groups.len(),
                groups.iter().map(|g| g.files.len() - 1).sum::<usize>()
            );
        }
        if args.edited {
            tracing::info!(
                "{} edited pair(s), {} Google Photos edit(s) to remove.",
                videre::output::edited_losers(&records).len(),
                removals.edits.len()
            );
        }
    }
    print_paths_delimited(&removals.all(), args.print0);

    if args.similar {
        let similar = videre::output::find_similar_groups(&records, 10);
        if !args.silent && records.iter().all(|r| r.phash.is_none()) {
            tracing::info!("No near-duplicate fingerprints yet: run videre embed to compute them.");
        } else if !args.silent && !similar.is_empty() {
            tracing::info!(
                "{} visually similar group(s) found: review with videre dedupe --html before deleting.",
                similar.len()
            );
        }
    }

    Ok(())
}

fn run_json(
    args: &DedupeArgs,
    ctx: &CommandContext,
) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "dedupe")?;
    videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "dedupe", || {
        super::build_find_duplicates_from(&conn, args.similar, args.edited)
    })
}
