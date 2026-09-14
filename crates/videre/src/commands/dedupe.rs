use crate::command_context::CommandContext;
use std::process;
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
                process::exit(1);
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
    crate::render::write_static_page(conn, &output, &groups, None)
}

fn run_text(args: DedupeArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let conn = match videre_core::library_db::open_existing(&ctx.library) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };
    let _activity = match videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    ) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };
    let guard = match videre_core::library_locks::try_command(&ctx.library, "dedupe") {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };

    let result =
        videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "dedupe", || {
            if args.remove {
                run_remove(&args, ctx, &conn)
            } else {
                run_dedupe_text(&args, &conn)
            }
        });
    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        process::exit(1);
    }

    if let Some(arg) = args.html.as_ref() {
        if let Err(e) = write_html(ctx, &conn, arg.as_deref()) {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    }
    Ok(())
}

/// Write the removable loser paths to stdout, NUL-delimited when `print0` so a
/// `| xargs -0` consumer is safe for paths containing spaces, else one per line.
fn print_losers_delimited(groups: &[videre::types::DuplicateGroup], print0: bool) {
    use std::io::Write;
    if !print0 {
        videre::output::print_losers(groups);
        return;
    }
    let mut out = std::io::stdout().lock();
    for path in videre::output::loser_paths(groups) {
        let _ = write!(out, "{path}\0");
    }
}

/// `--remove`: videre moves the duplicate copies to the system trash itself.
/// Safe by default: refuses on a missing library volume, refuses an implausibly
/// large deletion without --force, confirms unless --yes, and `--dry-run` only
/// previews. Operates only on the losers `loser_paths` computes, so the kept
/// copy per group is never removed.
fn run_remove(
    args: &DedupeArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> anyhow::Result<()> {
    let records = videre::sqlite_output::load_records_from(conn)
        .map_err(|e| anyhow::anyhow!("reading the library database: {e}"))?;
    let total = records.len();
    let groups = videre::output::find_duplicate_groups(&records);
    let losers: Vec<std::path::PathBuf> = videre::output::loser_paths(&groups)
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect();

    if losers.is_empty() {
        if !args.silent {
            eprintln!("No exact duplicates to remove.");
        }
        return Ok(());
    }

    // Missing-volume refusal: never read "the files are gone" as "everything is
    // a duplicate to remove". Check before the guard and before any deletion.
    if !ctx.library.paths.root.is_dir() {
        anyhow::bail!(
            "library root {:?} is not available (is the drive connected?); removed nothing",
            ctx.library.paths.root
        );
    }

    // Bulk-deletion guard, shared with prune: an implausibly large share of the
    // library is more likely a wrong selection or a mounting accident.
    if !args.force && super::is_bulk_delete(losers.len(), total) {
        eprintln!(
            "refusing to remove {} of {} file(s) ({:.0}% of the library): \
             that is more likely a mistake than real duplicates.",
            losers.len(),
            total,
            (losers.len() as f64 / total.max(1) as f64) * 100.0
        );
        eprintln!("  nothing was removed; re-run with --force if this is intended");
        return Ok(());
    }

    if args.dry_run {
        print_losers_delimited(&groups, args.print0);
        if !args.silent {
            eprintln!("{} file(s) would be moved to the trash.", losers.len());
        }
        return Ok(());
    }

    if !args.yes
        && !super::confirm(&format!(
            "This will move {} duplicate file(s) to the trash. Continue?",
            losers.len()
        ))?
    {
        eprintln!("Aborted; nothing was removed.");
        return Ok(());
    }

    let results = crate::removal::trash_paths(&losers);
    let moved = results.iter().filter(|(_, r)| r.is_ok()).count();
    let skipped = results.len() - moved;
    for (path, r) in &results {
        if let Err(e) = r {
            eprintln!("Warning: could not trash {path:?}: {e}");
        }
    }
    if moved == 0 && skipped > 0 {
        anyhow::bail!(
            "could not move any of the {skipped} file(s) to the trash (is the drive connected?)"
        );
    }
    if !args.silent {
        if skipped > 0 {
            eprintln!("Moved {moved} file(s) to the trash ({skipped} skipped).");
        } else {
            eprintln!("Moved {moved} file(s) to the trash.");
        }
    }
    Ok(())
}

/// The actual dedupe-reporting work, wrapped by `track_in()` above.
fn run_dedupe_text(args: &DedupeArgs, conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let records = videre::sqlite_output::load_records_from(conn)
        .map_err(|e| anyhow::anyhow!("reading the library database: {e}"))?;

    let groups = videre::output::find_duplicate_groups(&records);
    if !args.silent {
        if groups.is_empty() {
            eprintln!("No exact duplicates found.");
        } else {
            eprintln!(
                "{} duplicate group(s), {} file(s) to remove.",
                groups.len(),
                groups.iter().map(|g| g.files.len() - 1).sum::<usize>()
            );
        }
    }
    print_losers_delimited(&groups, args.print0);

    if args.similar {
        let similar = videre::output::find_similar_groups(&records, 10);
        if !args.silent && !similar.is_empty() {
            eprintln!(
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
        super::build_find_duplicates_from(&conn, args.similar)
    })
}
