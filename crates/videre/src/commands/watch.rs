use super::faces::format_clustering_only_summary;
use crate::command_context::CommandContext;
use anyhow::Result;

use std::time::{Duration, Instant};
use videre::scanner;
use videre_core::{decode_failures, face_db};
use videre_ml::pipeline::{run_clustering, run_face_pipeline_in};

#[derive(clap::Args)]
pub struct WatchArgs {
    /// Narrow the walk, exactly as on `videre scan`. Path-side only.
    #[command(flatten)]
    media: super::selection_args::MediaArgs,

    /// Re-run the scan/hash/EXIF pipeline each cycle
    #[arg(long)]
    scan: bool,
    /// Run incremental face detection each cycle
    #[arg(long)]
    faces: bool,
    /// Pre-convert and cache HEIC thumbnails each cycle
    #[arg(long)]
    heic: bool,
    /// Pre-resolve reverse-geocoded location names each cycle
    #[arg(long)]
    location: bool,
    /// Embed and classify new files each cycle, with the library's model. Never
    /// downloads it: until `videre embed` has fetched the model once, the stage
    /// says so and the files stay outstanding.
    #[arg(long)]
    embed: bool,
    /// Sync stale rows/cache and clean orphans each cycle (same cleanup as
    /// `videre prune`). Opt-in: unlike the other stages, it does not run
    /// when no stage flags are given. Never deletes real files, only stale
    /// db rows and cache entries for files already gone from disk.
    #[arg(long)]
    prune: bool,
    /// Write XMP sidecars for updated labels each cycle (opt-in). Also enabled
    /// by `videre config set export-xmp-on-watch true`.
    #[arg(long)]
    export_xmp: bool,

    #[command(flatten)]
    xmp: crate::xmp::XmpArg,

    /// Suppress per-cycle progress output (errors always shown)
    #[arg(long)]
    silent: bool,

    /// Stop once process <PID> is no longer this watch's parent. Set by the
    /// gallery on the watch it starts, so the watch cannot outlive it however
    /// it exits. Not for users: hidden from help.
    #[arg(long, value_name = "PID", hide = true)]
    exit_with: Option<u32>,
}

/// How often a watch serving a parent (`--exit-with`) checks that the parent
/// is still there. It bounds how long an orphaned watch lingers; the work it
/// is doing between checks is resumable.
const PARENT_CHECK: Duration = Duration::from_secs(1);

/// True when this watch serves a parent (`--exit-with`) that is gone: the
/// process has been re-parented, so its parent id is no longer the one it was
/// started for.
fn orphaned(args: &WatchArgs) -> bool {
    let gone = parent_gone(args.exit_with, std::os::unix::process::parent_id());
    if gone && !args.silent {
        tracing::info!("videre watch: the process that started it has exited; stopping");
    }
    gone
}

/// Pure half of [`orphaned`], for tests.
fn parent_gone(expected: Option<u32>, actual: u32) -> bool {
    expected.is_some_and(|pid| pid != actual)
}

/// Sleep for `total`, in [`PARENT_CHECK`] steps while serving a parent.
/// Returns false as soon as the parent is gone.
fn sleep_while_served(args: &WatchArgs, total: Duration) -> bool {
    if args.exit_with.is_none() {
        std::thread::sleep(total);
        return true;
    }
    let until = Instant::now() + total;
    loop {
        if orphaned(args) {
            return false;
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(PARENT_CHECK));
    }
}

/// Degraded-mode rescan cadence, in seconds. Internal, not a flag: this
/// runs only where live events cannot be delivered at all (see run()).
const FALLBACK_RESCAN_SECS: u64 = 300;

/// How long a busy event batch waits before its retry attempt.
/// Internal; used by the event loop.
const PENDING_RETRY_BACKOFF: Duration = Duration::from_secs(2);

pub fn run(mut args: WatchArgs, ctx: &CommandContext) -> Result<()> {
    // If NO stage flag at all was passed (including --prune), run the
    // original all-four default: the common case is "just keep everything up
    // to date". An explicit --prune is a stage selection and does not turn the
    // others on.
    if !(args.scan || args.faces || args.heic || args.location || args.embed || args.prune) {
        args.scan = true;
        args.faces = true;
        args.heic = true;
        args.location = true;
        args.embed = true;
    }

    // The XMP export stage is opt-in: the flag, or the library config default.
    if ctx.library.settings.export_xmp_on_watch {
        args.export_xmp = true;
    }

    // Watch is a writer: initialize the selected library so its database and
    // lock directory exist before the lifetime lock is taken.
    videre_core::library_db::initialize(&ctx.library)?;

    // Held for the entire life of this process (released even on kill), so a
    // second `videre watch` against this library is refused rather than racing.
    // No pipeline_runs row: watch has no "finished" moment, only running or not.
    let _watch_lock = videre_core::library_locks::try_command(&ctx.library, "watch")?;
    // Which stages this watcher runs, for the gallery's "still processing"
    // note: work a watcher never does must not read as in progress.
    let runs: Vec<&str> = [
        ("scan", args.scan),
        ("faces", args.faces),
        ("heic", args.heic),
        ("location", args.location),
        ("embed", args.embed),
    ]
    .into_iter()
    .filter_map(|(name, on)| on.then_some(name))
    .collect();
    videre_core::library_state::set_string(
        &videre_core::library_db::open_existing(&ctx.library)?,
        videre_core::status_report::WATCH_STAGES,
        &runs.join(","),
    )?;

    // The watcher registers BEFORE the startup reconcile: anything that lands
    // while the startup scan walks queues in the event channel and drains
    // right after, so the gap between "walked this directory" and "watching
    // this directory" cannot swallow a file. Registration failure (no event
    // backend on this mount) reaches the degraded arm, which runs the same
    // startup scan before falling back to the internal rescan.
    match event_loop(&args, ctx) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::warn!(
                "videre watch: live events unavailable ({e:#}); rescanning every \
                 {FALLBACK_RESCAN_SECS}s. On Linux, raising fs.inotify.max_user_watches \
                 may restore live watching."
            );
            if let Err(e) = reconcile(&args, ctx) {
                failed("startup scan", e);
            }
            // Test-only bounded exit still honors the degraded arm's startup
            // pass, so the once-hook never hangs on the fallback loop.
            if std::env::var_os("VIDERE_WATCH_ONCE").is_some() {
                return Ok(());
            }
            degraded_rescan_loop(&args, ctx)
        }
    }
}

/// Fallback only: no live events, so keep the library current with a slow
/// incremental rescan. Reached only when event registration fails (run()'s
/// error arm); the event loop sits in front of it when watching works.
fn degraded_rescan_loop(args: &WatchArgs, ctx: &CommandContext) -> Result<()> {
    loop {
        if !sleep_while_served(args, Duration::from_secs(FALLBACK_RESCAN_SECS)) {
            return Ok(());
        }
        if let Err(e) = reconcile(args, ctx) {
            failed("rescan", e);
        }
    }
}

/// The maintenance reconcile cadence, in seconds. Internal, not a flag: in
/// steady state this is what runs the opt-in prune/export stages and the
/// gated recluster, and it is the safety net for mounts that lose events
/// silently. Overridable only for tests (VIDERE_WATCH_TEST_MAINTENANCE_SECS,
/// same test-only category as VIDERE_WATCH_ONCE).
const MAINTENANCE_RECONCILE_SECS: u64 = 3600;

/// How long the loop may block before its next scheduled wake: the retry
/// backoff while work is pending, otherwise the time left to the
/// maintenance deadline (zero when it is due now). Pure so the wake rules
/// are unit-testable without a real watcher.
fn block_timeout(
    pending_empty: bool,
    since_maintenance: Duration,
    maintenance: Duration,
    backoff: Duration,
) -> Duration {
    let until_maintenance = maintenance.saturating_sub(since_maintenance);
    if pending_empty {
        until_maintenance
    } else {
        backoff.min(until_maintenance)
    }
}

/// What one drain does with the pending set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DrainMode {
    /// Every stage, for the batch.
    Normal,
    /// A bulk import is still arriving: scan only, so files show up at once,
    /// and keep them pending for the finish.
    ScanOnly,
    /// The bulk import went quiet: every stage once over everything collected,
    /// each model loaded once, with the full place recompute and face regroup.
    BulkFinish,
}

/// The mode for the next drain, and whether bulk mode is on after it. Bulk
/// starts when `threshold` files are waiting and holds, scanning only, until no
/// event has arrived for `quiet`; then one finish runs everything. Pure so the
/// rules are unit-testable.
fn next_mode(
    pending: usize,
    bulk: bool,
    since_event: Duration,
    threshold: usize,
    quiet: Duration,
) -> (DrainMode, bool) {
    if !bulk && pending < threshold {
        return (DrainMode::Normal, false);
    }
    if since_event >= quiet {
        (DrainMode::BulkFinish, false)
    } else {
        (DrainMode::ScanOnly, true)
    }
}

/// While bulk mode is on, the loop must wake at the quiet deadline even with no
/// event, or the finish would wait for the next one.
fn bulk_wake(bulk: bool, since_event: Duration, quiet: Duration) -> Option<Duration> {
    bulk.then(|| quiet.saturating_sub(since_event))
}

/// The backoff between wakes after a cycle hit a systemic disk error:
/// start at the pending-retry cadence, double, and never exceed the
/// maintenance cadence - the failing volume is retried hourly, not
/// hammered every 2 s. Pure so the rule is unit-testable.
fn next_io_backoff(current: Duration) -> Duration {
    if current.is_zero() {
        PENDING_RETRY_BACKOFF
    } else {
        current
            .saturating_mul(2)
            .min(Duration::from_secs(MAINTENANCE_RECONCILE_SECS))
    }
}

/// Enter the backoff after a fatal-IO error, warning once on the
/// transition in; the daemon stays up either way.
fn enter_io_backoff(io_backoff: &mut Duration) {
    let was_zero = io_backoff.is_zero();
    *io_backoff = next_io_backoff(*io_backoff);
    if was_zero {
        tracing::warn!("library volume is failing (disk I/O error); backing off before retrying");
    }
}

/// Fold one reconcile's result into the loop's io backoff: a fatal-IO
/// cycle enters the backoff, a cycle without one resets it (the volume
/// recovered).
fn fold_io_backoff(io_backoff: &mut Duration, result: &Result<ReconcileOutcome>) {
    let cycle_fatal = match result {
        Ok(outcome) => outcome.fatal_io,
        Err(e) => videre_core::error_kind::is_fatal_io_error(e),
    };
    if cycle_fatal {
        enter_io_backoff(io_backoff);
    } else {
        *io_backoff = Duration::ZERO;
    }
}

#[cfg(test)]
mod scheduler_tests {
    use super::*;

    const THRESHOLD: usize = 1000;
    const QUIET: Duration = Duration::from_secs(30);

    #[test]
    fn only_a_watch_serving_a_parent_can_be_orphaned() {
        assert!(
            !parent_gone(None, 1),
            "a standalone watch never stops itself"
        );
        assert!(!parent_gone(Some(4242), 4242), "its parent is still there");
        assert!(
            parent_gone(Some(4242), 1),
            "re-parented: the parent is gone"
        );
    }

    #[test]
    fn a_small_batch_drains_normally() {
        assert_eq!(
            next_mode(10, false, Duration::ZERO, THRESHOLD, QUIET),
            (DrainMode::Normal, false)
        );
    }

    #[test]
    fn a_large_batch_enters_bulk_and_only_scans() {
        assert_eq!(
            next_mode(1000, false, Duration::ZERO, THRESHOLD, QUIET),
            (DrainMode::ScanOnly, true)
        );
    }

    #[test]
    fn bulk_keeps_scanning_while_files_arrive() {
        assert_eq!(
            next_mode(40, true, Duration::from_secs(5), THRESHOLD, QUIET),
            (DrainMode::ScanOnly, true),
            "bulk holds even after the pending set shrank below the threshold"
        );
    }

    #[test]
    fn a_quiet_folder_finishes_the_bulk_import_in_one_pass() {
        assert_eq!(
            next_mode(2000, true, Duration::from_secs(31), THRESHOLD, QUIET),
            (DrainMode::BulkFinish, false)
        );
    }

    #[test]
    fn bulk_wakes_the_loop_at_the_quiet_deadline() {
        assert_eq!(
            bulk_wake(true, Duration::from_secs(10), QUIET),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            bulk_wake(true, Duration::from_secs(40), QUIET),
            Some(Duration::ZERO)
        );
        assert_eq!(bulk_wake(false, Duration::ZERO, QUIET), None);
    }

    #[test]
    fn a_complete_rescan_supersedes_the_pending_batch() {
        let mut pending: std::collections::BTreeSet<std::path::PathBuf> =
            std::collections::BTreeSet::new();
        pending.insert(std::path::PathBuf::from("/lib/a.jpg"));
        apply_rescan_outcome(
            &mut pending,
            Some(ReconcileOutcome {
                hashed: vec![],
                complete: true,
                fatal_io: false,
            }),
        );
        assert!(
            pending.is_empty(),
            "a complete reconcile supersedes pending"
        );
    }

    #[test]
    fn an_incomplete_rescan_seeds_its_work_and_keeps_the_batch() {
        let mut pending: std::collections::BTreeSet<std::path::PathBuf> =
            std::collections::BTreeSet::new();
        pending.insert(std::path::PathBuf::from("/lib/a.jpg"));
        let complete = apply_rescan_outcome(
            &mut pending,
            Some(ReconcileOutcome {
                hashed: vec![std::path::PathBuf::from("/lib/b.jpg")],
                complete: false,
                fatal_io: false,
            }),
        );
        assert!(!complete, "an incomplete reconcile is owed");
        assert!(
            pending.contains(std::path::Path::new("/lib/a.jpg"))
                && pending.contains(std::path::Path::new("/lib/b.jpg")),
            "an incomplete reconcile must retain the batch and seed its own scan output"
        );
    }

    #[test]
    fn a_failed_rescan_leaves_the_batch_alone() {
        let mut pending: std::collections::BTreeSet<std::path::PathBuf> =
            std::collections::BTreeSet::new();
        pending.insert(std::path::PathBuf::from("/lib/a.jpg"));
        let complete = apply_rescan_outcome(&mut pending, None);
        assert!(!complete, "a failed reconcile is owed");
        assert!(
            pending.contains(std::path::Path::new("/lib/a.jpg")),
            "a failed reconcile must not discard work"
        );
    }

    #[test]
    fn directory_candidates_expand_to_their_files() {
        let dir = tempfile::tempdir().unwrap();
        let moved = dir.path().join("moved-in");
        std::fs::create_dir_all(moved.join("nested")).unwrap();
        std::fs::write(moved.join("inner.jpg"), b"one").unwrap();
        std::fs::write(moved.join("nested/deep.jpg"), b"two").unwrap();
        std::fs::write(dir.path().join("loose.jpg"), b"three").unwrap();
        std::fs::create_dir_all(moved.join(".videre")).unwrap();
        std::fs::write(moved.join(".videre/state.db"), b"state").unwrap();

        let got = expand_candidates(&[
            moved.clone(),
            dir.path().join("loose.jpg"),
            // A file candidate that no longer exists passes through: the
            // scoped scan's needs_processing drop is the next stage's job,
            // not the expansion's.
            dir.path().join("does-not-exist.jpg"),
        ]);
        let names: Vec<String> = got
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert_eq!(
            names.len(),
            4,
            "2 in the tree + 1 loose + 1 pass-through: {names:?}"
        );
        assert!(names.iter().any(|n| n.ends_with("inner.jpg")));
        assert!(names.iter().any(|n| n.ends_with("deep.jpg")));
        assert!(names.iter().any(|n| n.ends_with("loose.jpg")));
        assert!(
            names.iter().all(|n| !n.contains(".videre")),
            "state files stay excluded: {names:?}"
        );
    }

    #[test]
    fn quiet_and_not_due_blocks_until_the_maintenance_deadline() {
        let t = block_timeout(
            true,
            Duration::from_secs(60),
            Duration::from_secs(3600),
            Duration::from_secs(2),
        );
        assert_eq!(t, Duration::from_secs(3540));
    }

    #[test]
    fn quiet_and_due_wakes_now() {
        let t = block_timeout(
            true,
            Duration::from_secs(3600),
            Duration::from_secs(3600),
            Duration::from_secs(2),
        );
        assert_eq!(t, Duration::ZERO);
    }

    #[test]
    fn pending_work_wakes_on_the_backoff() {
        let t = block_timeout(
            false,
            Duration::ZERO,
            Duration::from_secs(3600),
            Duration::from_secs(2),
        );
        assert_eq!(t, Duration::from_secs(2));
    }

    #[test]
    fn pending_work_never_overshoots_a_due_maintenance() {
        let t = block_timeout(
            false,
            Duration::from_secs(3601),
            Duration::from_secs(3600),
            Duration::from_secs(2),
        );
        assert_eq!(t, Duration::ZERO);
    }

    #[test]
    fn the_io_backoff_doubles_to_the_maintenance_cadence_then_stays() {
        let mut b = Duration::ZERO;
        b = next_io_backoff(b);
        assert_eq!(b, PENDING_RETRY_BACKOFF);
        b = next_io_backoff(b);
        assert_eq!(b, Duration::from_secs(4));
        b = next_io_backoff(b);
        assert_eq!(b, Duration::from_secs(8));
        let capped = next_io_backoff(Duration::from_secs(MAINTENANCE_RECONCILE_SECS));
        assert_eq!(capped, Duration::from_secs(MAINTENANCE_RECONCILE_SECS));
    }

    #[test]
    fn a_drain_open_failure_enters_the_backoff_only_for_disk_errors() {
        let cantopen = anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            None,
        ));
        let mut io_backoff = Duration::ZERO;
        note_drain_open_failure(&mut io_backoff, &cantopen);
        assert_eq!(io_backoff, PENDING_RETRY_BACKOFF);

        let busy = anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        ));
        let mut io_backoff = Duration::ZERO;
        note_drain_open_failure(&mut io_backoff, &busy);
        assert_eq!(io_backoff, Duration::ZERO);
    }
}

/// The live event path: one recursive debounced watch on the root, blocked
/// on until a batch settles, the OS signals dropped events, the retry
/// backoff fires, or the maintenance deadline comes due. Registering the
/// watcher is the only fallible step; after that, errors are logged and
/// watched over.
fn event_loop(args: &WatchArgs, ctx: &CommandContext) -> Result<()> {
    use notify::RecursiveMode;
    use notify_debouncer_full::new_debouncer;
    use std::collections::BTreeSet;
    use std::sync::mpsc::channel;

    // Test-only injection: fail registration so run() exercises the
    // degraded fallback. Never documented as a knob.
    if std::env::var_os("VIDERE_WATCH_TEST_FAIL_EVENTS").is_some() {
        anyhow::bail!("event registration failed (test injection)");
    }

    let debounce = Duration::from_millis(
        ctx.library
            .settings
            .watch_debounce_ms
            .unwrap_or(videre_core::library_config::WATCH_DEBOUNCE_MS_DEFAULT),
    );
    let maintenance = Duration::from_secs(
        std::env::var("VIDERE_WATCH_TEST_MAINTENANCE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(MAINTENANCE_RECONCILE_SECS),
    );
    let (tx, rx) = channel();
    let mut debouncer =
        new_debouncer(debounce, None, tx).map_err(|e| anyhow::anyhow!("debouncer: {e}"))?;
    debouncer
        .watch(&ctx.library.paths.root, RecursiveMode::Recursive)
        .map_err(|e| anyhow::anyhow!("watch registration: {e}"))?;
    if !args.silent {
        tracing::info!(
            "videre watch: watching live (debounce {}ms)",
            debounce.as_millis()
        );
    }

    // Paths seen but not yet processed because a stage was busy. Drained
    // on a short backoff; never a blocking wait, never a drop.
    let mut pending: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let mut last_maintenance = Instant::now();
    // Bulk mode: a large import scans as it arrives and runs everything else
    // once it goes quiet (see `next_mode`).
    let settings = &ctx.library.settings;
    let bulk_threshold = settings
        .watch_bulk_threshold
        .unwrap_or(videre_core::library_config::WATCH_BULK_THRESHOLD_DEFAULT)
        as usize;
    let bulk_quiet = Duration::from_millis(
        settings
            .watch_bulk_quiet_ms
            .unwrap_or(videre_core::library_config::WATCH_BULK_QUIET_MS_DEFAULT),
    );
    let mut bulk = false;
    let mut last_event = Instant::now();
    let drain = |pending: &mut BTreeSet<std::path::PathBuf>,
                 io_backoff: &mut Duration,
                 bulk: &mut bool,
                 last_event: Instant| {
        let (mode, on) = next_mode(
            pending.len(),
            *bulk,
            last_event.elapsed(),
            bulk_threshold,
            bulk_quiet,
        );
        if on && !*bulk && !args.silent {
            tracing::info!(
                "videre watch: bulk: {} files waiting; scanning as they arrive, \
                 everything else once no file has arrived for {}s",
                pending.len(),
                bulk_quiet.as_secs()
            );
        }
        *bulk = on;
        drain_pending(args, ctx, pending, io_backoff, mode);
    };

    // Startup incremental scan, run with the watcher already registered:
    // everything that changed while watch was down is caught here, and
    // anything that lands while the scan walks queues in the channel and
    // drains right after, so the walk-to-watch gap cannot lose a file. The
    // hashed paths seed the pending set: a downstream stage that was busy
    // while the scan ran retries them on the backoff, not on the hourly
    // maintenance pass.
    if !args.silent {
        tracing::info!("videre watch: startup scan");
    }
    // An incomplete startup reconcile (a stage was busy, the scan lock was
    // taken) is a debt: the loop retries it on the backoff exactly like a
    // dropped-events recovery that did not finish.
    let mut io_backoff = Duration::ZERO;
    let mut rescan_owed = {
        let result = reconcile(args, ctx);
        fold_io_backoff(&mut io_backoff, &result);
        !apply_rescan_outcome(
            &mut pending,
            result.map_err(|e| failed("startup scan", e)).ok(),
        )
    };
    // Test-only bounded exit: registration plus one startup pass then stop.
    // Test-only control, never documented as a user-facing knob.
    if std::env::var_os("VIDERE_WATCH_ONCE").is_some() {
        return Ok(());
    }

    loop {
        if orphaned(args) {
            return Ok(());
        }
        // Backing off a failing volume: hold for the cadence, letting
        // arriving events queue in the channel, then drain once at the
        // deadline. Everything below runs with the backoff cleared or due,
        // so an event storm cannot hit the volume ahead of the cadence.
        if !io_backoff.is_zero() {
            if !sleep_while_served(args, io_backoff) {
                return Ok(());
            }
            drain(&mut pending, &mut io_backoff, &mut bulk, last_event);
        }
        // A pending batch, an owed rescan, or the maintenance deadline: each
        // buys its own short wake instead of an idle block.
        let timeout = block_timeout(
            pending.is_empty() && !rescan_owed,
            last_maintenance.elapsed(),
            maintenance,
            // The io backoff has already been slept at the top of the loop,
            // so the pending cadence is all the extra wait a retry needs -
            // counting the backoff here too would double every wait.
            PENDING_RETRY_BACKOFF,
        );
        let timeout = bulk_wake(bulk, last_event.elapsed(), bulk_quiet)
            .map_or(timeout, |wake| timeout.min(wake));
        // Serving a parent: wake often enough to notice it has gone. A wake
        // with nothing due falls through the timeout arm doing nothing.
        let timeout = if args.exit_with.is_some() {
            timeout.min(PARENT_CHECK)
        } else {
            timeout
        };
        match rx.recv_timeout(timeout) {
            Ok(Ok(batch)) => {
                if videre::watch_events::needs_full_rescan(batch.iter().map(|e| &e.event)) {
                    // The backend dropped events: one incremental full
                    // reconcile, then resume. A complete reconcile supersedes
                    // anything pending; an incomplete one (a stage was busy,
                    // the scan errored) seeds what it hashed and keeps the
                    // batch for the backoff.
                    let result = reconcile(args, ctx);
                    fold_io_backoff(&mut io_backoff, &result);
                    let outcome = result.map_err(|e| failed("rescan", e));
                    rescan_owed = !apply_rescan_outcome(&mut pending, outcome.ok());
                    last_maintenance = Instant::now();
                    continue;
                }
                let mut arrived = false;
                for p in videre::watch_events::affected_paths(batch.iter().map(|e| &e.event)) {
                    arrived |= pending.insert(p);
                }
                // Only a path not already waiting is an arrival. Events for
                // files already pending (the bulk scan itself touches them)
                // would otherwise keep a finished import from ever going quiet.
                if arrived {
                    last_event = Instant::now();
                }
                drain(&mut pending, &mut io_backoff, &mut bulk, last_event);
            }
            Ok(Err(errs)) => {
                for e in errs {
                    failed("event", e);
                }
                // An event error may mean missed changes: reconcile to be safe.
                let result = reconcile(args, ctx);
                fold_io_backoff(&mut io_backoff, &result);
                let outcome = result.map_err(|e| failed("rescan", e));
                rescan_owed = !apply_rescan_outcome(&mut pending, outcome.ok());
                last_maintenance = Instant::now();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if rescan_owed {
                    // An earlier reconcile did not finish (a stage was busy,
                    // the scan lock was taken): attempt it again on the
                    // backoff. A complete attempt clears the debt, and the
                    // attempt itself counts as watch activity.
                    let result = reconcile(args, ctx);
                    fold_io_backoff(&mut io_backoff, &result);
                    let outcome = result.map_err(|e| failed("rescan", e));
                    rescan_owed = !apply_rescan_outcome(&mut pending, outcome.ok());
                    last_maintenance = Instant::now();
                } else if last_maintenance.elapsed() >= maintenance {
                    // The safety net must be a net: an incomplete maintenance
                    // reconcile (a stage was busy, the scan lock was taken)
                    // becomes a debt retried on the backoff, not an hour of
                    // silence. A complete one supersedes any waiting batch.
                    let result = reconcile(args, ctx);
                    fold_io_backoff(&mut io_backoff, &result);
                    let outcome = result.map_err(|e| failed("maintenance", e));
                    rescan_owed = !apply_rescan_outcome(&mut pending, outcome.ok());
                    last_maintenance = Instant::now();
                }
                drain(&mut pending, &mut io_backoff, &mut bulk, last_event);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // The backend died (its thread dropped or panicked): stop
                // pretending to watch. Returning the error sends run() into
                // the degraded arm, which reconciles and keeps the library
                // current instead of exiting a watcher in silence.
                return Err(anyhow::anyhow!("event channel disconnected"));
            }
        }
    }
}

/// A drain whose database will not open is the failing-volume case when
/// the error is a disk error: enter the backoff and keep the batch for
/// the deadline. Other open failures keep today's log-and-drop behavior.
fn note_drain_open_failure(io_backoff: &mut Duration, e: &anyhow::Error) {
    if videre_core::error_kind::is_fatal_io_error(e) {
        enter_io_backoff(io_backoff);
    }
}

/// Try to process everything pending as one batch. If the scan stage was
/// busy (another command holds the lock), the set is kept for the next
/// backoff tick; on success it is cleared. Downstream stages self-scope
/// through their own skip sets, so they run whenever the batch drained.
/// One tracked pipeline_runs row per stage per drain, because the batch is
/// coalesced before any stage runs.
fn drain_pending(
    args: &WatchArgs,
    ctx: &CommandContext,
    pending: &mut std::collections::BTreeSet<std::path::PathBuf>,
    io_backoff: &mut Duration,
    mode: DrainMode,
) {
    if pending.is_empty() {
        return;
    }
    if ctx.library.ensure_root_identity().is_err() {
        // An error: the batch's changes are dropped, not deferred.
        tracing::error!("videre watch: library root changed; dropping batch");
        pending.clear();
        return;
    }
    let conn = match videre_core::library_db::open_existing(&ctx.library) {
        Ok(c) => c,
        Err(e) => {
            note_drain_open_failure(io_backoff, &e);
            failed("open", e);
            return;
        }
    };
    let batch: Vec<std::path::PathBuf> = pending.iter().cloned().collect();
    // The batch survives until every enabled event-driven stage has actually
    // run: the scan is incremental, so the retry re-hashes nothing and the
    // busy stage finds its work through its own skip sets. Clearing earlier
    // would strand a scanned file on the hourly maintenance pass, which is
    // the silent drop the pending set exists to prevent.
    let mut complete = true;
    let mut fatal = false;
    if args.scan {
        match stage("scan", "scan stage", &mut fatal, || {
            run_scan_stage(args, ctx, &conn, Some(&batch), &mut Vec::new())
        }) {
            Some(StageOutcome::Ran) if mode == DrainMode::ScanOnly => {
                // Bulk: the files are indexed and visible; everything else
                // waits for the finish, so they stay pending.
                if let Err(e) =
                    videre_core::pipeline_runs::record_heartbeat_in(&conn, &ctx.library, "watch")
                {
                    failed("could not record the batch heartbeat", e);
                }
                return;
            }
            Some(StageOutcome::Ran) => {}
            Some(StageOutcome::Busy) => return, // keep pending; retry on the next backoff
            None => {
                complete = false;
                if fatal {
                    enter_io_backoff(io_backoff);
                    return; // keep pending; the backoff retries the batch
                }
            }
        }
    }
    if args.faces || args.heic || args.location || args.embed {
        if let Err(e) = face_db::create_faces_table(&conn) {
            failed("faces table", e);
            return;
        }
        if args.faces
            && stage("faces", "faces stage", &mut fatal, || {
                run_faces_stage(args, ctx, &conn)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
            if fatal {
                enter_io_backoff(io_backoff);
                return;
            }
        }
        // People for the batch's new faces, now rather than on the hourly
        // pass: the full regroup when the last one was cheap, else attach
        // only the new faces and leave the regroup to the hourly pass.
        if args.faces
            && stage("faces", "people stage", &mut fatal, || {
                run_people_stage(args, ctx, &conn, mode == DrainMode::BulkFinish)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
            if fatal {
                enter_io_backoff(io_backoff);
                return;
            }
        }
        if args.heic {
            stage("heic", "heic stage", &mut fatal, || {
                run_heic_stage(args, ctx, &conn)
            });
            if fatal {
                enter_io_backoff(io_backoff);
                return;
            }
        }
        if args.embed
            && stage("embed", "embed stage", &mut fatal, || {
                run_embed_stage(args, ctx, &conn)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
            if fatal {
                enter_io_backoff(io_backoff);
                return;
            }
        }
        if args.location
            && stage("locations", "location stage", &mut fatal, || {
                run_location_stage(args, ctx, &conn)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
            if fatal {
                enter_io_backoff(io_backoff);
                return;
            }
        }
        // Places for the batch's new GPS rows, now rather than on the hourly
        // pass: without this a copied photo kept no place while its original's
        // marker counted one, and the map disagreed with its own grid.
        if args.location
            && stage("locations", "places stage", &mut fatal, || {
                run_places_stage(args, ctx, &conn, mode == DrainMode::BulkFinish)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
            if fatal {
                enter_io_backoff(io_backoff);
                return;
            }
        }
    }
    // Sidecars for the batch's files, when export is on: a copied labelled
    // photo gets its sidecar now, not on the hourly full export.
    if args.export_xmp
        && stage("export", "export stage", &mut fatal, || {
            let hashes: Vec<String> = batch_rows(&conn, &batch)?
                .into_iter()
                .map(|(_, hash)| hash)
                .collect();
            super::export::export_hashes_in(&conn, ctx, &hashes)
        })
        .is_none()
    {
        complete = false;
        if fatal {
            enter_io_backoff(io_backoff);
            return;
        }
    }
    if !args.silent && !fatal {
        if let Err(e) = report_batch(args, ctx, &batch) {
            failed("batch report", e);
        }
    }
    if !fatal {
        // A batch without a disk fault resets the backoff: the volume
        // recovered.
        *io_backoff = Duration::ZERO;
    }
    if complete {
        pending.clear();
    }
    // A drained batch is watch activity the same way a cycle completion is.
    if let Err(e) = videre_core::pipeline_runs::record_heartbeat_in(&conn, &ctx.library, "watch") {
        failed("could not record the batch heartbeat", e);
    }
}

/// Whether a tracked stage actually ran, or found a lock busy and skipped.
/// The event loop uses `Busy` to defer a batch and retry it on a short
/// backoff, never blocking and never dropping work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StageOutcome {
    Ran,
    Busy,
}

/// Run one watch stage with its name recorded on everything it logs. A
/// failure is logged here, once, as `videre watch: <label>`, and the cycle
/// carries on: `None` tells the caller the stage did not finish. `name` is
/// the command the stage stands in for (`scan`, `faces`, `locations`, ...);
/// `label` keeps the wording watch has always used for that stage.
fn stage<T>(
    name: &'static str,
    label: &str,
    fatal: &mut bool,
    f: impl FnOnce() -> Result<T>,
) -> Option<T> {
    let _stage = videre_core::error_log::enter_stage(name);
    match f() {
        Ok(v) => Some(v),
        Err(e) => {
            if videre_core::error_kind::is_fatal_io_error(&e) {
                *fatal = true;
            }
            videre_core::error_log::report(
                tracing::Level::ERROR,
                &e.context(format!("videre watch: {label}")),
                None,
            );
            None
        }
    }
}

/// Log one of watch's own failures (not a stage's): the watcher survives it.
fn failed(what: &str, e: impl Into<anyhow::Error>) {
    videre_core::error_log::report(
        tracing::Level::ERROR,
        &e.into().context(format!("videre watch: {what}")),
        None,
    );
}

/// Run a tracked stage under the library's activity lease and a command lock,
/// in that order. A busy activity lease (an exclusive maintenance pass, or
/// another shared operation when this stage needs exclusive) or a busy command
/// lock is reported and skipped; the caller decides how to retry. Never
/// reacquires the watch lifetime lock. The tracked label and the lock are
/// separate on purpose: the label names the row in pipeline_runs, the lock
/// names the standalone command this stage must not overlap with.
fn tracked_stage(
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    command: &str,
    lock: &str,
    mode: videre_core::library_locks::ActivityMode,
    silent: bool,
    f: impl FnOnce() -> Result<()>,
) -> Result<StageOutcome> {
    let _activity = match videre_core::library_locks::try_activity(&ctx.library, mode) {
        Ok(guard) => guard,
        Err(_) => {
            if !silent {
                tracing::info!(
                    "videre watch: {command} stage busy (the library is in use); will retry"
                );
            }
            return Ok(StageOutcome::Busy);
        }
    };
    match videre_core::library_locks::try_command(&ctx.library, lock) {
        Ok(guard) => {
            videre_core::pipeline_runs::track_in_as(conn, &ctx.library, &guard, lock, command, f)?;
            Ok(StageOutcome::Ran)
        }
        Err(_) => {
            if !silent {
                tracing::info!(
                    "videre watch: {command} stage busy (a {lock} run is active); will retry"
                );
            }
            Ok(StageOutcome::Busy)
        }
    }
}

/// What one reconcile actually accomplished. `hashed` carries the paths the
/// scan hashed (empty when `--scan` was off or nothing changed); `complete`
/// is false when the scan errored or any batch-relevant stage (faces,
/// location, recluster) found its lock busy, which means the work is not
/// done and must not supersede a waiting batch.
struct ReconcileOutcome {
    hashed: Vec<std::path::PathBuf>,
    complete: bool,
    /// A stage or the scan hit a systemic disk error this cycle: the loop
    /// backs off instead of retrying on the pending cadence.
    fatal_io: bool,
}

/// Fold a reconcile's result into the pending set. A complete reconcile
/// supersedes the batch (its full walk re-found everything); an incomplete
/// one seeds the paths its scan did hash and keeps whatever was waiting, so
/// the backoff retries the busy stages instead of the hourly pass. `None`
/// (the reconcile itself errored) touches nothing. Returns `true` only when
/// the reconcile finished completely: `false` is a debt the caller owes and
/// must attempt again on the backoff, even when nothing was hashed.
fn apply_rescan_outcome(
    pending: &mut std::collections::BTreeSet<std::path::PathBuf>,
    outcome: Option<ReconcileOutcome>,
) -> bool {
    match outcome {
        Some(outcome) => {
            pending.extend(outcome.hashed);
            if outcome.complete {
                pending.clear();
                return true;
            }
            false
        }
        None => false,
    }
}

fn reconcile(args: &WatchArgs, ctx: &CommandContext) -> Result<ReconcileOutcome> {
    // Recheck before every cycle: a root renamed or replaced under a
    // long-running watch must fail the cycle rather than process whatever now
    // sits at the path. open_existing rechecks again at the write boundary.
    ctx.library.ensure_root_identity()?;
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    // The paths this cycle's scan hashed. The caller that needs them is the
    // startup one: its downstream stages can be busy while the scan runs, and
    // the hashed files are exactly the work those stages must retry.
    let mut hashed: Vec<std::path::PathBuf> = Vec::new();
    let mut complete = true;
    let mut cycle_fatal = false;
    if args.scan {
        // A scan failure this cycle does not invalidate earlier rows; log and
        // carry on to the stages below, but report the cycle as incomplete so
        // the caller keeps any waiting batch alive. A busy scan lock is the
        // same debt: the walk never ran, so nothing was re-found.
        if stage("scan", "scan stage", &mut cycle_fatal, || {
            run_scan_stage(args, ctx, &conn, None, &mut hashed)
        }) != Some(StageOutcome::Ran)
        {
            complete = false;
        }
    }
    if args.faces || args.heic || args.location || args.embed || args.prune || args.export_xmp {
        face_db::create_faces_table(&conn)?;
        if args.faces
            && stage("faces", "faces stage", &mut cycle_fatal, || {
                run_faces_stage(args, ctx, &conn)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
        }
        // A HEIC cache failure costs previews only, so the location, prune and
        // export stages still run; but the cycle is incomplete, so the caller
        // owes a retry instead of treating the work as done.
        if args.heic
            && stage("heic", "heic stage", &mut cycle_fatal, || {
                run_heic_stage(args, ctx, &conn)
            })
            .is_none()
        {
            complete = false;
        }
        if args.embed
            && stage("embed", "embed stage", &mut cycle_fatal, || {
                run_embed_stage(args, ctx, &conn)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
        }
        if args.location {
            if stage("locations", "location stage", &mut cycle_fatal, || {
                run_location_stage(args, ctx, &conn)
            }) != Some(StageOutcome::Ran)
            {
                complete = false;
            }
            // Names first, then regroup: re-cluster when the GPS data changed.
            if stage(
                "locations",
                "locations recluster stage",
                &mut cycle_fatal,
                || run_locations_recluster_stage(args, ctx, &conn),
            ) != Some(StageOutcome::Ran)
            {
                complete = false;
            }
        }
        if args.faces
            && stage("faces", "face recluster stage", &mut cycle_fatal, || {
                run_recluster_stage(args, ctx, &conn)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
        }
        if args.prune
            && stage("prune", "prune stage", &mut cycle_fatal, || {
                run_prune_stage(args, ctx, &conn)
            }) != Some(StageOutcome::Ran)
        {
            complete = false;
        }
        if args.export_xmp {
            stage("export", "export stage", &mut cycle_fatal, || {
                run_export_stage(args, ctx, &conn)
            });
        }
    }

    // The cycle completed: record the heartbeat that makes `videre status`
    // able to say "watch: running (last cycle 2m ago)". Best-effort: a
    // bookkeeping failure must not kill an otherwise healthy watcher.
    if let Err(e) = videre_core::pipeline_runs::record_heartbeat_in(&conn, &ctx.library, "watch") {
        failed("could not record the cycle heartbeat", e);
    }
    Ok(ReconcileOutcome {
        hashed,
        complete,
        fatal_io: cycle_fatal,
    })
}

/// Writes XMP sidecars for the current labels (marks, named face regions,
/// location, category), merging into any existing sidecar. Runs the same shared
/// writer as `videre export --xmp`, over every file, against the already-open
/// connection.
fn run_export_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    let n = super::export::export_all_in(conn, ctx)?;
    if !args.silent {
        tracing::info!("videre watch: export stage wrote {n} sidecar(s)");
    }
    Ok(())
}

/// Runs the same cleanup as `videre prune` (stale-row removal, modified_at
/// sync, orphan embeddings/cache cleanup) against the already-open
/// connection, tracked in `pipeline_runs` under "prune" exactly like a
/// standalone `videre prune` invocation would be.
fn run_prune_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<StageOutcome> {
    let prune_args = super::prune::PruneArgs::for_watch_stage(args.silent);
    tracked_stage(
        ctx,
        conn,
        "prune",
        "prune",
        videre_core::library_locks::ActivityMode::Exclusive,
        args.silent,
        || {
            let errors = super::prune::run_prune(&prune_args, &ctx.library, conn)?;
            if !args.silent && errors > 0 {
                tracing::info!("videre watch: prune stage finished with {errors} error(s)");
            }
            Ok(())
        },
    )
}

/// Queries (path, hash) pairs from file_hashes matching a SQL WHERE clause,
/// deduped to one representative path per hash.
/// The fixed set of extension filters `dedup_paths_by_hash` supports, a
/// closed enum rather than a raw `&str` WHERE-clause fragment, so there is no
/// way for a future caller to pass runtime-built SQL text into the query.
enum PathExtFilter {
    /// Every extension `videre faces` detects on: images plus HEIC.
    Faces,
    /// HEIC only, for the thumbnail-cache warming stage.
    HeicOnly,
}

fn dedup_paths_by_hash(
    conn: &rusqlite::Connection,
    filter: PathExtFilter,
) -> Result<Vec<(String, String)>> {
    let sql = match filter {
        PathExtFilter::Faces => {
            "SELECT path, hash FROM file_hashes \
             WHERE ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')"
        }
        PathExtFilter::HeicOnly => "SELECT path, hash FROM file_hashes WHERE ext = 'heic'",
    };
    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut seen = std::collections::HashSet::new();
    Ok(rows
        .into_iter()
        .filter(|(_, hash)| seen.insert(hash.clone()))
        .collect())
}

fn run_faces_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<StageOutcome> {
    tracked_stage(
        ctx,
        conn,
        "faces",
        "faces",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let all_paths = dedup_paths_by_hash(conn, PathExtFilter::Faces)?;
            // Skip already-scanned hashes (marker includes no-face images), unioned with
            // hashes that already have faces for pre-marker migration, same resumable
            // skip set as `videre faces`.
            let mut skip_hashes: std::collections::HashSet<String> =
                face_db::scanned_hashes(conn)?.into_iter().collect();
            skip_hashes.extend(face_db::hashes_with_faces(conn)?);
            // Also skip hashes the face decode has already failed on enough
            // times. This matters most in watch: it re-runs on every cycle, so
            // without this an undecodable file pays its timeout every loop
            // forever. Recording happens in the pipeline; `videre faces
            // --reprocess` is the retry hatch.
            decode_failures::ensure_table(conn)?;
            skip_hashes.extend(decode_failures::failed_hashes(
                conn,
                decode_failures::STAGE_FACES,
                decode_failures::FAILURE_THRESHOLD,
            )?);
            let to_process: Vec<(String, String)> = all_paths
                .into_iter()
                .filter(|(_, hash)| !skip_hashes.contains(hash))
                .collect();

            let new_faces = if to_process.is_empty() {
                0
            } else {
                let workers = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
                let result = run_face_pipeline_in(
                    &ctx.library,
                    conn,
                    &to_process,
                    8,
                    false,
                    args.silent,
                    None,
                    workers,
                )?;
                if !args.silent {
                    tracing::info!(
                        "videre watch: faces stage processed {} new hash(es), {} face(s)",
                        to_process.len(),
                        result.total_faces
                    );
                }
                result.total_faces
            };
            // Detection only here. Grouping the new faces is the people
            // stage that follows (run_people_stage), which runs the full
            // regroup only when the last one was cheap.
            let _ = new_faces;
            Ok(())
        },
    )
}

/// The periodic global face recluster, gated by the persisted watermark:
/// any face id above it means new faces exist, so one pass runs and then
/// advances the mark; otherwise this is a zero-cost skip. Runs on every
/// reconcile, and on an event batch only when the last pass was cheap or a
/// bulk import finishes (`run_people_stage`): at scale a full pass takes
/// minutes, so a batch then attaches only the new faces. The lock is `faces`, shared with detection and the
/// standalone command so they cannot overlap; the tracked label is its
/// own so `videre status` can show repair passes distinctly. The
/// watermark advances even when the quality gate filters every face
/// out: those faces are permanently unclusterable, and re-running the
/// pass for them every reconcile would be the waste the gate exists to
/// prevent.
fn run_recluster_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<StageOutcome> {
    // The stage's own lock is its identity while it runs (the
    // location-names precedent): a recluster can hold the shared faces lock
    // for minutes, and `videre status` reads the row's liveness from a lock
    // named after the row. Without this, a healthy pass reads as crashed.
    // Taken before the shared faces lock so a crash mid-acquisition
    // releases both in order.
    let _identity = match videre_core::library_locks::try_command(&ctx.library, "face-recluster") {
        Ok(guard) => guard,
        Err(_) => {
            if !args.silent {
                tracing::info!("videre watch: face recluster stage busy; will retry");
            }
            return Ok(StageOutcome::Busy);
        }
    };
    tracked_stage(
        ctx,
        conn,
        "face-recluster",
        "faces",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let max_id: i64 =
                conn.query_row("SELECT COALESCE(MAX(id), 0) FROM faces", [], |r| r.get(0))?;
            let watermark = videre_core::face_db::recluster_watermark(conn)?;
            if max_id <= watermark {
                if !args.silent {
                    tracing::info!("videre watch: face recluster up to date");
                }
                return Ok(());
            }
            let params = super::cluster_settings::resolve_for_run(
                &ctx.library.paths.state,
                &videre_ml::cluster_params::PartialClusteringParameters::default(),
                args.silent,
            );
            let clustering = run_clustering(conn, &params, args.silent)?;
            videre_core::face_db::advance_recluster_watermark(conn)?;
            if !args.silent {
                tracing::info!(
                    "videre watch: {}",
                    format_clustering_only_summary(clustering, params.eps)
                );
            }
            Ok(())
        },
    )
}

/// Writes `img` as a JPEG to `tmp_path`, then atomically renames it to
/// `final_path`. Returns true on success.
///
/// `image::DynamicImage::save()` infers the encoder from the file
/// extension via `Path::extension()`; a tmp path like `hash_240.tmp19181`
/// has extension `tmp19181`, which maps to no encoder and always fails.
/// The tmp-file-then-rename pattern is correct for atomic publishing into
/// the cache; only the encode step must not rely on extension inference,
/// so the format is passed explicitly.
fn publish_thumb(
    img: &image::DynamicImage,
    tmp_path: &std::path::Path,
    final_path: &std::path::Path,
) -> bool {
    img.save_with_format(tmp_path, image::ImageFormat::Jpeg)
        .is_ok()
        && std::fs::rename(tmp_path, final_path).is_ok()
}

fn run_heic_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<()> {
    let cache = &ctx.library.cache;
    let heic_paths = dedup_paths_by_hash(conn, PathExtFilter::HeicOnly)?;
    let mut converted = 0usize;
    let mut failed = 0usize;
    for (path, hash) in heic_paths {
        let need_240 = !videre_core::thumb_cache::thumb_exists_in(cache, &hash, 240);
        let need_1200 = !videre_core::thumb_cache::thumb_exists_in(cache, &hash, 1200);
        // Also feeds `videre faces`'s detection cache (see `load_image` in
        // videre-ml's pipeline.rs), a full-res original cached here means
        // detection can skip its own qlmanage decode entirely for this hash.
        let need_original = !videre_core::thumb_cache::original_exists_in(cache, &hash);
        if !need_240 && !need_1200 && !need_original {
            continue;
        }
        std::fs::create_dir_all(&cache.thumbnails).ok();
        // Convert once, then downscale the same in-memory image for each
        // missing size (largest first) instead of re-running QuickLook per
        // size. None (full resolution), not Some(n): the same decode also
        // seeds the `original_path` cache below, which detection's bbox
        // coordinates depend on being full resolution. See the safety note
        // on decode_via_quicklook. This means this call site does NOT get
        // lever-2a's render-size-cap treatment other watch-adjacent callers
        // do; the two levers are in tension here and full-res wins because
        // avoiding a second full qlmanage decode during `videre faces` is
        // the bigger saving of the two.
        match videre_core::heic::decode_via_quicklook(std::path::Path::new(&path), "watch", None) {
            Ok(img) => {
                if need_original {
                    let tmp_path = videre_core::thumb_cache::original_tmp_path_in(cache, &hash);
                    let final_path = videre_core::thumb_cache::original_path_in(cache, &hash);
                    if publish_thumb(&img, &tmp_path, &final_path) {
                        converted += 1;
                    } else {
                        failed += 1;
                        let _ = std::fs::remove_file(&tmp_path);
                    }
                }
                for size in [1200u32, 240] {
                    let need = if size == 240 { need_240 } else { need_1200 };
                    if !need {
                        continue;
                    }
                    let resized = if img.width() > size || img.height() > size {
                        img.resize(size, size, image::imageops::FilterType::Triangle)
                    } else {
                        img.clone()
                    };
                    let tmp_path = videre_core::thumb_cache::thumb_tmp_path_in(cache, &hash, size);
                    let final_path = videre_core::thumb_cache::thumb_path_in(cache, &hash, size);
                    if publish_thumb(&resized, &tmp_path, &final_path) {
                        converted += 1;
                    } else {
                        failed += 1;
                        let _ = std::fs::remove_file(&tmp_path);
                    }
                }
            }
            Err(error) => {
                videre_core::heic::warn_if_timeout(&error);
                if need_240 {
                    failed += 1;
                }
                if need_1200 {
                    failed += 1;
                }
                if need_original {
                    failed += 1;
                }
            }
        }
    }
    // Failures are a warning even under --silent; the plain count is progress.
    if failed > 0 {
        tracing::warn!("videre watch: heic stage cached {converted} thumbnail(s), {failed} failed");
    } else if !args.silent && converted > 0 {
        tracing::info!("videre watch: heic stage cached {converted} thumbnail(s)");
    }
    Ok(())
}

fn run_location_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<StageOutcome> {
    // Deliberately its own tracked label, not `locations`: that row means the
    // standalone clustering recompute (`videre locations` rebuilds
    // `location_clusters` from scratch), while this stage incrementally fills
    // `location_name` — a column the recompute never touches; it names the
    // clusters instead. And deliberately `location-names`, not `geocode`:
    // `videre_core::geocode` is forward geocoding (place name -> coordinates),
    // this is the reverse direction.
    //
    // The lock, though, is `locations`, shared with the standalone recompute,
    // and that coordination is required rather than optional: SQLite has a
    // single writer, the recompute holds it inside one transaction for
    // minutes (measured ~8 on a 70k-file library), and library connections
    // use a 5s busy timeout. With a lock of its own, a cycle overlapping a
    // recompute would die on SQLITE_BUSY and record a failed (or seemingly
    // crashed) run for work that is merely postponed; sharing the lock turns
    // that into the clean skip-and-retry below.
    // The stage's own lock is its identity in the read path: it is held only
    // by this stage, while `locations` below is shared with the standalone
    // recompute, so the reader can always tell the two holders apart. Taken
    // first so a crash mid-acquisition releases both in order.
    let _names_lock = match videre_core::library_locks::try_command(&ctx.library, "location-names")
    {
        Ok(guard) => guard,
        Err(_) => {
            if !args.silent {
                tracing::info!("videre watch: location stage busy; will retry");
            }
            return Ok(StageOutcome::Busy);
        }
    };
    tracked_stage(
        ctx,
        conn,
        "location-names",
        "locations",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let unresolved: Vec<(f64, f64)> = {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT gps_lat, gps_lon FROM file_hashes \
                     WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL AND location_name IS NULL",
                )?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let mut resolved = 0usize;
            for (lat, lon) in unresolved {
                if let Some(name) =
                    videre_core::location::location_name_in(&ctx.library.cache, lat, lon)?
                {
                    conn.execute(
                        "UPDATE file_hashes SET location_name = ?1 \
                         WHERE ROUND(gps_lat, 6) = ROUND(?2, 6) AND ROUND(gps_lon, 6) = ROUND(?3, 6)",
                        rusqlite::params![name, lat, lon],
                    )?;
                    resolved += 1;
                }
            }
            if !args.silent && resolved > 0 {
                tracing::info!("videre watch: location stage resolved {resolved} coordinate(s)");
            }
            Ok(())
        },
    )
}

/// The location recluster: the same from-scratch pass the standalone command
/// runs, gated by a fingerprint over the GPS-bearing data. The gate is one
/// query; the recompute is the expensive part. Runs on every reconcile, and on
/// an event batch only when the last recompute was cheap or a bulk import
/// finishes (`run_places_stage`); otherwise a batch places only the new rows
/// and this rebalances on the next reconcile. Tracked under the `locations` row
/// and lock, the same work the standalone command does, so status liveness
/// needs no new machinery. A standalone run at a non-default radius is a manual
/// choice: the watcher leaves it alone rather than silently reclustering at the
/// default (the radius the last recompute used is recorded by that recompute).
/// Set once this process has said the embedding model is not downloaded, so a
/// long-running watch says it once rather than every batch.
static EMBED_MODEL_MISSING_NOTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Embed, then classify, the files not yet embedded under the library's model,
/// through the same work `videre embed` and `videre classify` do. Their own
/// pending sets scope them, as the faces stage is scoped. Never downloads the
/// model: a background watcher must not start a multi-gigabyte fetch, so until
/// `videre embed` has run once the stage says so and the files stay outstanding
/// in `videre status`.
fn run_embed_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<StageOutcome> {
    use std::sync::atomic::Ordering;
    let model_id = videre_core::embeddings::resolve_model_id_from(&ctx.library.settings, None)?;
    if !videre_ml::model::is_cached(&model_id) {
        if !EMBED_MODEL_MISSING_NOTED.swap(true, Ordering::Relaxed) {
            tracing::info!(
                "videre watch: embed skipped: model {model_id} not downloaded; \
                 run `videre embed` once to fetch it"
            );
        }
        return Ok(StageOutcome::Ran);
    }
    // Each stage attaches its model database, which a connection can hold
    // only once: give each its own.
    let embedded = tracked_stage(
        ctx,
        conn,
        "embed",
        "embed",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let own = videre_core::library_db::open_existing(&ctx.library)?;
            super::embed::run_in(
                &super::embed::EmbedArgs::for_pipeline(args.silent),
                ctx,
                &own,
                &model_id,
            )
        },
    )?;
    if embedded != StageOutcome::Ran
        || !videre_core::embeddings_db::db_path_in(&ctx.library, &model_id)?.exists()
    {
        return Ok(embedded);
    }
    tracked_stage(
        ctx,
        conn,
        "classify",
        "classify",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let own = videre_core::library_db::open_existing(&ctx.library)?;
            super::classify::run_in(
                &super::classify::ClassifyArgs::for_pipeline(args.silent),
                &ctx.library,
                &own,
                &model_id,
            )
        },
    )
}

/// The radius the last recompute ran at, else the default: the radius a place
/// created for a new row gets, so it matches its neighbours.
fn stored_radius(conn: &rusqlite::Connection) -> Result<f64> {
    use videre_core::location_cluster;
    Ok(
        videre_core::library_state::get_string(conn, location_cluster::LOCATIONS_RADIUS)?
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(location_cluster::DEFAULT_CLUSTER_RADIUS_KM),
    )
}

/// Places for an event batch's new GPS rows. When the last full recompute was
/// cheap (`recompute_cost`), or `force_full` (a bulk import's finish), the batch
/// runs it, exactly as the hourly pass would; otherwise, and always under a
/// manual radius the watcher must not recompute at, only the unplaced rows are
/// placed, and the full recompute is left to the hourly pass.
fn run_places_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    force_full: bool,
) -> Result<StageOutcome> {
    use videre_core::location_cluster;
    let radius = stored_radius(conn)?;
    let manual = (radius - location_cluster::DEFAULT_CLUSTER_RADIUS_KM).abs() > f64::EPSILON;
    if !manual
        && (force_full
            || videre_core::recompute_cost::is_cheap(conn, videre_core::recompute_cost::LOCATIONS)?)
    {
        return run_locations_recluster_stage(args, ctx, conn);
    }
    tracked_stage(
        ctx,
        conn,
        "location-assign",
        "locations",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let out = location_cluster::assign_new_rows(conn, &ctx.library.cache, radius)?;
            if !args.silent && out.joined + out.created > 0 {
                tracing::info!(
                    "videre watch: placed {} new location(s) ({} new place(s))",
                    out.joined + out.created,
                    out.created
                );
            }
            Ok(())
        },
    )
}

/// People for an event batch's new faces: the full regroup when the last one
/// was cheap or `force_full` (a bulk import's finish), else only the new faces
/// attach to their nearest person and the regroup is left to the hourly pass.
fn run_people_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    force_full: bool,
) -> Result<StageOutcome> {
    if force_full
        || videre_core::recompute_cost::is_cheap(conn, videre_core::recompute_cost::FACES)?
    {
        return run_recluster_stage(args, ctx, conn);
    }
    tracked_stage(
        ctx,
        conn,
        "face-attach",
        "faces",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let params = super::cluster_settings::resolve_for_run(
                &ctx.library.paths.state,
                &videre_ml::cluster_params::PartialClusteringParameters::default(),
                true,
            );
            videre_ml::pipeline::attach_new_faces(conn, &params, args.silent)?;
            Ok(())
        },
    )
}

fn run_locations_recluster_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
) -> Result<StageOutcome> {
    use videre_core::location_cluster;
    tracked_stage(
        ctx,
        conn,
        "locations",
        "locations",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let live = location_cluster::gps_fingerprint(conn)?;
            let stored_fp = videre_core::library_state::get_string(
                conn,
                location_cluster::LOCATIONS_GPS_FINGERPRINT,
            )?;
            let stored_radius =
                videre_core::library_state::get_string(conn, location_cluster::LOCATIONS_RADIUS)?
                    .and_then(|s| s.parse::<f64>().ok());
            let radius = location_cluster::DEFAULT_CLUSTER_RADIUS_KM;
            if let Some(r) = stored_radius {
                if (r - radius).abs() > f64::EPSILON {
                    if !args.silent {
                        tracing::info!(
                            "videre watch: locations: manual radius {r}km in effect; \
                             the watcher leaves it alone"
                        );
                    }
                    return Ok(());
                }
            }
            if stored_fp.as_deref() == Some(live.as_str()) {
                if !args.silent {
                    tracing::info!("videre watch: locations up to date");
                }
                return Ok(());
            }
            // recompute_all records the fresh fingerprint and radius itself.
            location_cluster::recompute_all(conn, &ctx.library.cache, radius, args.silent)?;
            if !args.silent {
                tracing::info!("videre watch: location clusters rebuilt");
            }
            Ok(())
        },
    )
}

/// A populated folder moved into the library arrives as one event whose
/// path is a directory; hashing that path fails, so directory candidates
/// expand to the files inside them (same .videre exclusion as the walk).
/// File candidates pass through untouched. Extracted from the scoped scan
/// so the expansion rule has a deterministic test that cannot be skipped
/// by OS event delivery.
/// Print what the batch did for each of its files, read back from the database
/// (see `watch_report`), so a user can tell what was applied and what was not.
/// Its own connection: it attaches the model database to check embeddings.
fn report_batch(
    args: &WatchArgs,
    ctx: &CommandContext,
    batch: &[std::path::PathBuf],
) -> Result<()> {
    use super::watch_report::{gather, render, Stages};
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let rows = batch_rows(&conn, batch)?;
    if rows.is_empty() {
        return Ok(());
    }
    let model_id = videre_core::embeddings::resolve_model_id_from(&ctx.library.settings, None)?;
    let mut embeddable = std::collections::HashSet::new();
    if args.embed {
        embeddable = videre_core::embeddings::embeddable_images(&conn, &model_id)?
            .into_iter()
            .map(|p| p.hash)
            .collect();
        if videre_core::embeddings_db::db_path_in(&ctx.library, &model_id)?.exists() {
            videre_core::embeddings_db::attach_for_read_in(&conn, &ctx.library, &model_id)?;
        }
    }
    let stages = Stages {
        faces: args.faces,
        embed: args.embed,
        location: args.location,
        embed_skipped: (args.embed && !videre_ml::model::is_cached(&model_id))
            .then(|| "model not downloaded".to_string()),
    };
    let facts = gather(
        &conn,
        &ctx.library.paths.root,
        &rows,
        &stages,
        &model_id,
        &embeddable,
    )?;
    for line in render(&facts) {
        tracing::info!("videre watch: {line}");
    }
    Ok(())
}

/// `(path, hash)` for every indexed file at or under a batch's paths, a
/// directory event standing for everything in it. Files the scan did not index
/// (outside the media selection, unreadable) are simply absent.
fn batch_rows(
    conn: &rusqlite::Connection,
    batch: &[std::path::PathBuf],
) -> Result<Vec<(String, String)>> {
    use rusqlite::OptionalExtension;
    let mut stmt = conn.prepare("SELECT hash FROM file_hashes WHERE path = ?1")?;
    let mut rows = Vec::new();
    for p in expand_candidates(batch) {
        let path = p.to_string_lossy().into_owned();
        if let Some(hash) = stmt
            .query_row([&path], |r| r.get::<_, String>(0))
            .optional()?
        {
            rows.push((path, hash));
        }
    }
    Ok(rows)
}

fn expand_candidates(paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut expanded: Vec<std::path::PathBuf> = Vec::new();
    for p in paths {
        if p.is_dir() {
            expanded.extend(
                walkdir::WalkDir::new(p)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                    .map(|e| e.path().to_path_buf())
                    .filter(|p| !p.components().any(|c| c.as_os_str() == ".videre")),
            );
        } else {
            expanded.push(p.clone());
        }
    }
    expanded
}

fn run_scan_stage(
    args: &WatchArgs,
    ctx: &CommandContext,
    conn: &rusqlite::Connection,
    candidates: Option<&[std::path::PathBuf]>,
    hashed: &mut Vec<std::path::PathBuf>,
) -> Result<StageOutcome> {
    let selection = super::selection_args::path_selection(Some(&args.media), None)?;
    let root = ctx.library.paths.root.clone();
    let scoped = candidates.map(|c| c.to_vec());
    tracked_stage(
        ctx,
        conn,
        "scan",
        "scan",
        videre_core::library_locks::ActivityMode::Shared,
        args.silent,
        || {
            let base: Vec<_> = match &scoped {
                None => scanner::scan(&root)
                    .into_iter()
                    .filter(|p| !p.components().any(|c| c.as_os_str() == ".videre"))
                    .collect(),
                Some(paths) => expand_candidates(paths),
            };
            let walked = base.len();
            let paths: Vec<_> = if selection.is_empty() {
                base
            } else {
                base.into_iter().filter(|p| selection.accepts(p)).collect()
            };
            if !selection.is_empty() && !args.silent {
                tracing::info!(
                    "videre watch: scan stage considering {} of {} file(s) ({})",
                    paths.len(),
                    walked,
                    selection.describe()
                );
            }
            // Incremental in both modes: a spurious event on an unchanged
            // file hashes nothing, exactly like an unchanged file in a full
            // walk. XMP reconcile covers just the files hashed here; the full
            // walk's sidecar-change sweep keeps its own incremental rule
            // through reconcile_xmp_in.
            let prec = args.xmp.resolve_from(&ctx.library.settings)?;
            let written =
                crate::indexing::index_paths(conn, &ctx.library, paths, prec, args.silent)?;
            // Reported to the caller: the startup pass feeds these into the
            // pending set, so a downstream stage that was busy while the scan
            // ran retries them on the backoff instead of waiting an hour.
            let count = written.len();
            hashed.extend(written);
            if !args.silent {
                tracing::info!("videre watch: scan stage wrote {count} record(s)");
            }
            Ok(())
        },
    )
}

#[cfg(test)]
mod publish_thumb_tests {
    use super::*;

    #[test]
    fn publish_thumb_writes_jpeg_despite_tmp_extension() {
        // Regression test: the watch heic stage writes through a tmp path
        // shaped like `hash_240.tmp<pid>` (see thumb_cache::thumb_tmp_path),
        // then renames it into place. image::DynamicImage::save() infers the
        // encoder from the file extension, and a ".tmp<pid>" suffix maps to
        // no encoder, so a plain save() always fails on this tmp path shape.
        let tmp_dir =
            std::env::temp_dir().join(format!("publish_thumb_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let tmp_path = tmp_dir.join(format!("hash_240.tmp{}", std::process::id()));
        let final_path = tmp_dir.join("hash_240.jpg");

        let img = image::DynamicImage::new_rgb8(4, 4);
        let ok = publish_thumb(&img, &tmp_path, &final_path);

        assert!(
            ok,
            "publish_thumb should succeed even though the tmp path has no recognizable extension"
        );
        assert!(
            final_path.exists(),
            "final thumbnail file should exist after publish"
        );
        assert!(!tmp_path.exists(), "tmp file should be gone after rename");

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}

#[cfg(test)]
mod stage_query_tests {
    use super::*;
    use rusqlite::Connection;

    fn db_with(rows: &[(&str, &str, &str)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                created_at TEXT, modified_at TEXT, ext TEXT, mime TEXT, phash INTEGER,
                exif_date TEXT, capture_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);",
        )
        .unwrap();
        for (path, hash, ext) in rows {
            c.execute(
                "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, ?3)",
                rusqlite::params![path, hash, ext],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn the_faces_filter_takes_images_including_heic_and_no_video() {
        let c = db_with(&[
            ("/a.jpg", "h1", "jpg"),
            ("/b.heic", "h2", "heic"),
            ("/c.png", "h3", "png"),
            ("/d.mov", "h4", "mov"),
            ("/e.mp4", "h5", "mp4"),
            ("/f.dng", "h6", "dng"),
        ]);
        let got = dedup_paths_by_hash(&c, PathExtFilter::Faces).unwrap();
        let mut exts: Vec<_> = got
            .iter()
            .map(|(p, _)| p.rsplit('.').next().unwrap())
            .collect();
        exts.sort();
        assert_eq!(
            exts,
            vec!["heic", "jpg", "png"],
            "video and raw must not reach face detection"
        );
    }

    #[test]
    fn the_heic_filter_takes_only_heic() {
        let c = db_with(&[("/a.jpg", "h1", "jpg"), ("/b.heic", "h2", "heic")]);
        let got = dedup_paths_by_hash(&c, PathExtFilter::HeicOnly).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].0.ends_with(".heic"));
    }

    #[test]
    fn duplicates_collapse_to_one_path_per_hash() {
        // The point of the dedup: three copies of one photo cost one decode,
        // not three. Whichever path wins, the hash must appear once.
        let c = db_with(&[
            ("/one.jpg", "same", "jpg"),
            ("/two.jpg", "same", "jpg"),
            ("/three.jpg", "same", "jpg"),
            ("/other.jpg", "different", "jpg"),
        ]);
        let got = dedup_paths_by_hash(&c, PathExtFilter::Faces).unwrap();
        assert_eq!(got.len(), 2);
        let mut hashes: Vec<_> = got.iter().map(|(_, h)| h.as_str()).collect();
        hashes.sort();
        assert_eq!(hashes, vec!["different", "same"]);
    }

    #[test]
    fn an_empty_library_yields_no_work_rather_than_an_error() {
        let c = db_with(&[]);
        assert!(dedup_paths_by_hash(&c, PathExtFilter::Faces)
            .unwrap()
            .is_empty());
        assert!(dedup_paths_by_hash(&c, PathExtFilter::HeicOnly)
            .unwrap()
            .is_empty());
    }
}

#[cfg(test)]
mod scoping_tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: WatchArgs,
    }

    fn parse(extra: &[&str]) -> WatchArgs {
        let mut v = vec!["watch"];
        v.extend_from_slice(extra);
        Wrap::parse_from(v).args
    }

    #[test]
    fn watch_accepts_the_media_flags_only() {
        // The walk is rooted at the invocation library and has not opened any
        // file, so it can answer only the media flags (--type/--ext). --date,
        // --location and the data-derived selectors must fail to parse rather
        // than fail at runtime.
        let a = parse(&["--type", "image", "--ext", "heic"]);
        let sel = super::super::selection_args::path_selection(Some(&a.media), None).unwrap();
        assert!(!sel.is_empty());

        for bad in [
            vec!["watch", "--date", "2024"],
            vec!["watch", "--location", "Berlin"],
            vec!["watch", "--person", "Alice"],
            vec!["watch", "--category", "screenshot"],
            vec!["watch", "--path", "/tmp/x"],
        ] {
            assert!(
                Wrap::try_parse_from(&bad).is_err(),
                "watch must reject {:?}: it is not part of watch's vocabulary",
                bad[1]
            );
        }
    }

    #[test]
    fn no_flags_means_an_empty_selection_that_accepts_everything() {
        let a = parse(&[]);
        let sel = super::super::selection_args::path_selection(Some(&a.media), None).unwrap();
        assert!(sel.is_empty(), "an unscoped watch must not filter the walk");
        assert!(sel.accepts(std::path::Path::new("/anything/at/all.mov")));
    }

    #[test]
    fn a_type_filter_narrows_the_walk_the_same_way_scan_does() {
        let a = parse(&["--type", "video"]);
        let sel = super::super::selection_args::path_selection(Some(&a.media), None).unwrap();
        assert!(sel.accepts(std::path::Path::new("/x/clip.mov")));
        assert!(!sel.accepts(std::path::Path::new("/x/photo.jpg")));
    }

    #[test]
    fn the_removed_interval_flag_is_gone() {
        assert!(
            Wrap::try_parse_from(["watch", "--interval", "60"]).is_err(),
            "--interval was removed; it must fail to parse rather than be accepted silently"
        );
    }
}
