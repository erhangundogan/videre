//! `videre dedupe [list|review|trash|delete|undo]`: one action per
//! subcommand, over the kinds in `videre::duplicates`. Every action but
//! `undo` takes `--kind` and a scope (`--query`, `--path`, `--type`, `--ext`);
//! a group is selected when any member matches, and is then judged whole.

use crate::command_context::CommandContext;
use videre::duplicates::{Group, Kind};
use videre::types::ErrorJson;

#[derive(clap::Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct DedupeArgs {
    #[command(subcommand)]
    action: Option<Action>,

    /// `videre dedupe` alone is `videre dedupe list`.
    #[command(flatten)]
    list: ListArgs,
}

#[derive(clap::Subcommand)]
enum Action {
    /// List the copies each group would remove (the default action)
    List(ListArgs),
    /// Write the groups to a review page you can keep and open later
    Review(ReviewArgs),
    /// Move the copies to the system trash, keeping each group's keeper.
    /// Recoverable with videre dedupe undo
    Trash(RemoveArgs),
    /// Delete the copies permanently, keeping each group's keeper. Cannot be
    /// undone
    Delete(RemoveArgs),
    /// Put back the files the most recent trash run moved to the trash
    Undo(UndoArgs),
}

/// Which duplicates: their kinds, and the files a group must touch.
#[derive(clap::Args, Clone)]
struct ScopeArgs {
    /// Kinds of duplicate: exact (identical content, the oldest kept),
    /// resized (the same picture at another pixel size, the largest kept),
    /// creation (a Google Photos -edited, -EFFECTS or -SMILE file beside its
    /// original, the original kept), similar (near pictures, review only).
    /// Repeatable, or comma-separated
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        value_name = "KIND",
        default_value = "exact"
    )]
    kind: Vec<Kind>,

    #[command(flatten)]
    query: super::selection_args::QueryArg,
    #[command(flatten)]
    paths: super::selection_args::PathArgs,
    #[command(flatten)]
    media: super::selection_args::MediaArgs,

    /// Suppress progress and summaries on stderr (paths still go to stdout)
    #[arg(long)]
    silent: bool,
}

#[derive(clap::Args, Clone)]
struct ListArgs {
    #[command(flatten)]
    scope: ScopeArgs,

    /// Print one JSON document on stdout instead of the copies' paths
    #[arg(long, conflicts_with = "print0")]
    json: bool,

    /// Print the paths NUL-delimited, safe for `| xargs -0` with spaces in
    /// paths. To remove the copies, use videre dedupe trash instead
    #[arg(long)]
    print0: bool,
}

#[derive(clap::Args)]
struct ReviewArgs {
    /// Where to write the page. Default: hashes_duplicates.html beside the
    /// library database
    #[arg(value_hint = clap::ValueHint::FilePath)]
    page: Option<std::path::PathBuf>,

    #[command(flatten)]
    scope: ScopeArgs,
}

#[derive(clap::Args)]
struct RemoveArgs {
    #[command(flatten)]
    scope: ScopeArgs,

    /// List what would be removed and remove nothing
    #[arg(long)]
    dry_run: bool,

    /// Skip the confirmation prompt
    #[arg(long)]
    yes: bool,

    /// Proceed even when the count trips the bulk-deletion guard
    #[arg(long)]
    force: bool,

    /// Print one JSON document on stdout: what was (or would be) removed
    #[arg(long, conflicts_with = "print0")]
    json: bool,

    /// With --dry-run, print the paths NUL-delimited
    #[arg(long)]
    print0: bool,
}

#[derive(clap::Args)]
struct UndoArgs {
    /// List what would be restored and restore nothing
    #[arg(long)]
    dry_run: bool,

    /// Skip the confirmation prompt
    #[arg(long)]
    yes: bool,

    /// Print the restore report as JSON
    #[arg(long)]
    json: bool,

    /// Suppress progress and summaries on stderr
    #[arg(long)]
    silent: bool,
}

pub fn run(args: DedupeArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    match args.action {
        None => run_list(&args.list, ctx),
        Some(Action::List(list)) => run_list(&list, ctx),
        Some(Action::Review(review)) => run_review(&review, ctx),
        Some(Action::Trash(remove)) => run_remove(&remove, crate::removal::Method::Trash, ctx),
        Some(Action::Delete(remove)) => run_remove(&remove, crate::removal::Method::Delete, ctx),
        Some(Action::Undo(undo)) => run_undo(&undo, ctx),
    }
}

/// The groups `scope` selects, in a library already open and locked.
/// `decode` lets the resized check take the signatures it is missing.
fn selected_groups(
    scope: &ScopeArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    decode: bool,
) -> anyhow::Result<videre::duplicates::Found> {
    use videre_core::selection::SelectionCtx;
    // A typo in the query fails before anything is decoded.
    scope.query.compile()?;
    videre_core::library_guard::validate_paths(&ctx.library, &scope.paths.path)?;
    let mut found = videre::duplicates::find(conn, &scope.kind, decode, scope.silent)?;
    let sel = super::selection_args::row_selection(
        Some(&scope.media),
        None,
        None,
        None,
        None,
        Some(&scope.paths),
        None,
        None,
    )?;
    let sel = super::selection_args::with_query(
        sel,
        &scope.query,
        conn,
        &SelectionCtx::default(),
        &ctx.library,
    )?;
    let resolved = sel.resolve_in(conn, &SelectionCtx::default(), &ctx.library)?;
    if let Some(hashes) = resolved.hashes {
        let before = found.groups.len();
        found.groups =
            videre::duplicates::filter_groups(found.groups, &|r| hashes.contains(&r.hash));
        if !scope.silent {
            tracing::info!("{} of {before} group(s) match.", found.groups.len());
        }
    }
    Ok(found)
}

/// Open the library for a dedupe action: shared activity, the dedupe lock,
/// and the run recorded under `dedupe` whichever action it is.
fn with_library<T>(
    ctx: &CommandContext,
    f: impl FnOnce(&rusqlite::Connection) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "dedupe")?;
    videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "dedupe", || f(&conn))
}

/// A JSON-printing action: the document on success, an error document (and
/// the failure, once) otherwise.
fn print_json<T: serde::Serialize>(result: anyhow::Result<T>) -> anyhow::Result<()> {
    match result {
        Ok(doc) => {
            println!("{}", serde_json::to_string(&doc)?);
            Ok(())
        }
        Err(e) => {
            println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
            Err(crate::exit::Exit::shown(e).into())
        }
    }
}

fn run_list(args: &ListArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    let scope = &args.scope;
    let found = with_library(ctx, |conn| {
        let found = selected_groups(scope, ctx, conn, true)?;
        if args.json {
            return super::duplicates_document(conn, &scope.kind, found).map(Listed::Json);
        }
        Ok(Listed::Groups(found))
    });
    let found = match found {
        Ok(Listed::Groups(found)) => found,
        Ok(Listed::Json(doc)) => return print_json(Ok(doc)),
        Err(e) if args.json => return print_json::<()>(Err(e)),
        Err(e) => return Err(e),
    };
    let removals = Removals::collect(&found.groups, false);
    if !scope.silent {
        summarize(&found, &removals);
    }
    print_paths_delimited(&removals.all(), args.print0);
    Ok(())
}

enum Listed {
    Groups(videre::duplicates::Found),
    Json(videre::types::FindDuplicatesJson),
}

fn kinds_need_fingerprints(found: &videre::duplicates::Found) -> bool {
    found
        .wanted
        .iter()
        .any(|k| matches!(k, Kind::Resized | Kind::Similar))
}

/// One line per kind on stderr: groups and copies, or what review shows.
fn summarize(found: &videre::duplicates::Found, removals: &Removals) {
    for &kind in &found.wanted {
        let groups = found.groups.iter().filter(|g| g.kind == kind).count();
        if kind.removable() {
            tracing::info!(
                "{groups} {} group(s), {} file(s) to remove.",
                kind.name(),
                removals.count(kind)
            );
        } else {
            tracing::info!(
                "{groups} {} group(s), review only: see them with videre dedupe review --kind {}.",
                kind.name(),
                kind.name()
            );
        }
    }
    if found.fingerprinted == 0 && kinds_need_fingerprints(found) {
        tracing::info!(
            "No fingerprints yet, so no resized or similar groups: run videre embed to compute them."
        );
    }
    if found.unchecked > 0 {
        tracing::info!(
            "{} possible resized copies could not be checked.",
            found.unchecked
        );
    }
}

/// `review`: the groups as a page you can keep. Static on purpose: `videre
/// gallery` is the live review; this survives the process and can be
/// archived or opened later.
fn run_review(args: &ReviewArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    // A bare `review` targets a page beside the selected database; an
    // explicit relative path is an operand, resolved against the launch
    // directory.
    let output = match &args.page {
        Some(p) => ctx.operand(p),
        None => {
            let db = &ctx.library.paths.db;
            let stem = db
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            db.with_file_name(format!("{stem}_duplicates.html"))
        }
    };
    with_library(ctx, |conn| {
        let found = selected_groups(&args.scope, ctx, conn, true)?;
        let bytes = crate::render::write_review_page(conn, &output, &found)?;
        if !args.scope.silent {
            tracing::info!(
                "Wrote {} group(s) to {} ({} KB)",
                found.groups.len(),
                output.display(),
                bytes / 1024
            );
        }
        Ok(())
    })
}

/// Write the removable copy paths to stdout, NUL-delimited when `print0` so a
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

/// What a run removes (`videre::duplicates::removals`), with the counts and
/// wording dedupe reports.
struct Removals {
    by_kind: Vec<(Kind, std::path::PathBuf)>,
}

impl Removals {
    fn collect(groups: &[Group], check_originals: bool) -> Removals {
        Removals {
            by_kind: videre::duplicates::removals(groups, check_originals),
        }
    }

    fn all(&self) -> Vec<std::path::PathBuf> {
        self.by_kind.iter().map(|(_, p)| p.clone()).collect()
    }

    fn count(&self, kind: Kind) -> usize {
        self.by_kind.iter().filter(|(k, _)| *k == kind).count()
    }

    /// What counts toward the bulk-deletion guard. Not creations: one is
    /// paired only when its original is in the library too, so a large count
    /// is what a Takeout export looks like, not a sign of a mistake.
    fn guarded(&self) -> usize {
        self.by_kind
            .iter()
            .filter(|(k, _)| *k != Kind::Creation)
            .count()
    }

    /// `3 exact copies and 2 Google Photos creations`, leaving out zeros.
    fn describe(&self) -> String {
        let parts: Vec<String> = [
            (Kind::Exact, "exact duplicate(s)"),
            (Kind::Resized, "resized copies"),
            (Kind::Creation, "Google Photos creation(s)"),
        ]
        .into_iter()
        .filter_map(|(kind, noun)| {
            let n = self.count(kind);
            (n > 0).then(|| format!("{n} {noun}"))
        })
        .collect();
        parts.join(" and ")
    }
}

/// `trash --json` / `delete --json`.
#[derive(serde::Serialize)]
struct RemoveJson {
    action: &'static str,
    dry_run: bool,
    removed: Vec<std::path::PathBuf>,
    already_gone: Vec<std::path::PathBuf>,
    failed: Vec<std::path::PathBuf>,
}

/// `trash` / `delete`: videre removes the copies itself, to the system trash
/// or permanently. Safe by default: refuses a review-only kind, refuses on a
/// missing library volume, refuses an implausibly large removal without
/// --force, confirms unless --yes, and `--dry-run` only previews. Operates
/// only on what `Removals` computes, which is also what `list` prints: a
/// group's keeper is never removed.
fn run_remove(
    args: &RemoveArgs,
    method: crate::removal::Method,
    ctx: &CommandContext,
) -> anyhow::Result<()> {
    if let Some(kind) = args.scope.kind.iter().find(|k| !k.removable()) {
        anyhow::bail!(
            "`{0}` is review-only: see it with videre dedupe review --kind {0}",
            kind.name()
        );
    }
    let silent = args.scope.silent || args.json;
    let outcome = with_library(ctx, |conn| remove_copies(args, method, ctx, conn, silent));
    if args.json {
        return print_json(outcome);
    }
    let doc = outcome?;
    let moved = doc.removed.len() + doc.already_gone.len();
    if !args.dry_run && moved > 0 {
        prune_after(ctx, args.scope.silent);
    }
    Ok(())
}

fn remove_copies(
    args: &RemoveArgs,
    method: crate::removal::Method,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    silent: bool,
) -> anyhow::Result<RemoveJson> {
    use crate::removal::{Method, Outcome};
    let (action, fate) = match method {
        Method::Trash => ("trash", "moved to the trash"),
        Method::Delete => ("delete", "permanently deleted"),
    };
    let mut doc = RemoveJson {
        action,
        dry_run: args.dry_run,
        removed: Vec::new(),
        already_gone: Vec::new(),
        failed: Vec::new(),
    };
    let volume_missing = || {
        anyhow::anyhow!(
            "library root {:?} is not available (is the drive connected?); removed nothing",
            ctx.library.paths.root
        )
    };
    // Missing-volume refusal: never read "the files are gone" as "everything
    // is a duplicate to remove". Before anything is counted, because creation
    // pairing checks each original on disk: a detached drive would otherwise
    // empty the set and report "Nothing to remove."
    if !ctx.library.paths.root.is_dir() {
        return Err(volume_missing());
    }
    let mut scope = args.scope.clone();
    scope.silent = silent;
    let found = selected_groups(&scope, ctx, conn, true)?;
    let removals = Removals::collect(&found.groups, true);
    let losers = removals.all();
    if losers.is_empty() {
        if !silent {
            tracing::info!("Nothing to remove.");
        }
        return Ok(doc);
    }

    // Bulk-deletion guard, shared with prune: an implausibly large share of
    // the library is more likely a wrong selection or a mounting accident.
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    let total = total.max(0) as usize;
    let guarded = removals.guarded();
    if !args.force && super::is_bulk_delete(guarded, total) {
        anyhow::bail!(
            "refusing to remove {guarded} of {total} file(s) ({:.0}% of the library): that is \
             more likely a mistake than real duplicates. Nothing was removed; re-run with \
             --force if this is intended",
            (guarded as f64 / total.max(1) as f64) * 100.0
        );
    }

    if args.dry_run {
        if !args.json {
            print_paths_delimited(&losers, args.print0);
        }
        if !silent {
            tracing::info!("{} would be {fate}.", removals.describe());
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
        doc.removed = losers;
        return Ok(doc);
    }

    let question = match method {
        Method::Trash => format!(
            "This will move {} to the trash. Continue?",
            removals.describe()
        ),
        Method::Delete => format!(
            "This will permanently delete {}. This cannot be undone. Continue?",
            removals.describe()
        ),
    };
    if !args.yes && !super::confirm(&question)? {
        tracing::info!("Aborted; nothing was removed.");
        return Ok(doc);
    }

    // A bar on a terminal, a line every half minute otherwise, and each
    // failure as it happens: a run over thousands of files never looks hung.
    // Rows go as each file does, so a run stopped halfway leaves the library
    // listing exactly what is still on disk.
    let progress =
        videre_core::progress::Progress::new_counting(losers.len() as u64, silent, "files");
    // A trash run records what it moved, so `undo` can put it back.
    let mut manifest = (method == Method::Trash)
        .then(|| crate::trash_stack::Writer::new(&ctx.library.paths.state, "dedupe"));
    let results = crate::removal::remove_and_forget(
        Some(conn),
        &losers,
        method,
        &progress,
        manifest.as_mut(),
    );
    progress.finish();
    for (path, outcome) in results {
        match outcome {
            Outcome::Removed => doc.removed.push(path),
            Outcome::AlreadyGone => doc.already_gone.push(path),
            _ => doc.failed.push(path),
        }
    }
    let (removed, gone, failed) = (doc.removed.len(), doc.already_gone.len(), doc.failed.len());
    if removed == 0 && gone == 0 && failed > 0 {
        anyhow::bail!("could not remove any of the {failed} file(s) (is the drive connected?)");
    }
    if !silent {
        let done = match method {
            Method::Trash => format!("Moved {removed} file(s) to the trash"),
            Method::Delete => format!("Deleted {removed} file(s)"),
        };
        let mut notes = Vec::new();
        if gone > 0 {
            notes.push(format!("{gone} already gone"));
        }
        if failed > 0 {
            notes.push(format!("{failed} could not be removed"));
        }
        if notes.is_empty() {
            tracing::info!("{done}.");
        } else {
            tracing::info!("{done} ({}).", notes.join(", "));
        }
    }
    Ok(doc)
}

/// Each removed copy's row was forgotten as it went. What hung off those rows
/// by hash (marks, tags, faces, embeddings, signatures of content no copy
/// holds any more) is `videre prune`'s cleanup, run here so the library never
/// shows a ghost. Best effort: the removal itself already succeeded, and a
/// user can always run `videre prune` by hand.
fn prune_after(ctx: &CommandContext, silent: bool) {
    let prune_args = super::prune::PruneArgs::for_watch_stage(silent);
    // Named, so the summary line that follows is not a puzzle, and the
    // command it stands for is learned.
    if !silent {
        tracing::info!("Cleaning up the library, as videre prune does:");
    }
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

/// `undo`: put back the newest recorded trash run (dedupe or gallery
/// Delete), one run per call.
fn run_undo(args: &UndoArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    use crate::trash_stack::{self, Step};
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let guard = videre_core::library_locks::try_command(&ctx.library, "dedupe")?;
    let locate = trash_stack::system();

    let Some((run, steps)) = trash_stack::plan_latest(&ctx.library.paths.state, locate.as_ref())?
    else {
        if args.json {
            println!("{}", serde_json::json!({ "nothing_to_undo": true }));
        } else {
            tracing::info!("Nothing to undo: no trash run is recorded in this library.");
        }
        return Ok(());
    };
    let restorable = steps
        .iter()
        .filter(|s| matches!(s, Step::Restore(_)))
        .count();

    if args.dry_run {
        for (entry, step) in run.entries.iter().zip(&steps) {
            let path = entry.file.path.display();
            match step {
                Step::Restore(c) => println!("[would restore] {path}  <-  {}", c.file.display()),
                Step::Occupied => println!("[skipped, a file is already there] {path}"),
                Step::NotInTrash => println!("[not in the trash] {path}"),
            }
        }
        if !args.silent {
            tracing::info!(
                "Dry run: {restorable} of {} file(s) trashed on {} would be restored.",
                run.entries.len(),
                run.when
            );
        }
        return Ok(());
    }

    if restorable > 0
        && !args.yes
        && !super::confirm(&format!(
            "Restore {restorable} file(s) trashed on {}?",
            run.when
        ))?
    {
        tracing::info!("Aborted; nothing was restored.");
        return Ok(());
    }

    let progress = videre_core::progress::Progress::new_counting(
        run.entries.len() as u64,
        args.silent || args.json,
        "files",
    );
    let report =
        videre_core::pipeline_runs::track_in(&conn, &ctx.library, &guard, "dedupe", || {
            trash_stack::undo_latest(&conn, &ctx.library, locate.as_ref(), &progress)
        })?
        .expect("planned above");
    progress.finish();

    if args.json {
        println!("{}", serde_json::to_string(&report)?);
    } else if !args.silent {
        let mut notes = Vec::new();
        if !report.not_in_trash.is_empty() {
            notes.push(format!("{} not in the trash", report.not_in_trash.len()));
        }
        if !report.occupied.is_empty() {
            notes.push(format!(
                "{} skipped: a file is already there",
                report.occupied.len()
            ));
        }
        if !report.failed.is_empty() {
            notes.push(format!("{} could not be moved", report.failed.len()));
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        };
        tracing::info!(
            "Restored {} file(s) from the run of {}{notes}. {} earlier run(s) can still be undone.",
            report.restored.len(),
            report.when,
            report.runs_left
        );
        if !report.restored.is_empty() {
            tracing::info!(
                "Their embeddings and faces are rebuilt by videre embed and videre faces, or a running videre watch; videre status lists them until then."
            );
        }
    }
    if report.restored.is_empty() && !report.failed.is_empty() {
        return Err(crate::exit::Exit::code(1).into());
    }
    Ok(())
}
