use super::faces::format_clustering_only_summary;
use crate::command_context::CommandContext;
use anyhow::Result;
use rayon::prelude::*;
use std::time::{Duration, Instant};
use videre::{hasher, scanner, sqlite_output, types};
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
    /// Sync stale rows/cache and clean orphans each cycle (same cleanup as
    /// `videre prune`). Opt-in only, unlike the other four stages, this is
    /// NOT included when no stage flags are passed, so existing `videre
    /// watch` invocations keep their current behavior unchanged. Never
    /// deletes real files, only stale db rows and cache entries for files
    /// already gone from disk.
    #[arg(long)]
    prune: bool,
    /// Write XMP sidecars for updated labels each cycle (opt-in). Also enabled
    /// by `videre config set export-xmp-on-watch true`.
    #[arg(long)]
    export_xmp: bool,

    #[command(flatten)]
    xmp: crate::xmp::XmpArg,

    #[arg(long)]
    silent: bool,
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
    if !(args.scan || args.faces || args.heic || args.location || args.prune) {
        args.scan = true;
        args.faces = true;
        args.heic = true;
        args.location = true;
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

    // The watcher registers BEFORE the startup reconcile: anything that lands
    // while the startup scan walks queues in the event channel and drains
    // right after, so the gap between "walked this directory" and "watching
    // this directory" cannot swallow a file. Registration failure (no event
    // backend on this mount) reaches the degraded arm, which runs the same
    // startup scan before falling back to the internal rescan.
    match event_loop(&args, ctx) {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!(
                "videre watch: live events unavailable ({e:#}); rescanning every \
                 {FALLBACK_RESCAN_SECS}s. On Linux, raising fs.inotify.max_user_watches \
                 may restore live watching."
            );
            if let Err(e) = reconcile(&args, ctx) {
                eprintln!("videre watch: startup scan error: {e}");
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
        std::thread::sleep(Duration::from_secs(FALLBACK_RESCAN_SECS));
        if let Err(e) = reconcile(args, ctx) {
            eprintln!("videre watch: rescan error: {e}");
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

#[cfg(test)]
mod scheduler_tests {
    use super::*;

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
        eprintln!(
            "videre watch: watching live (debounce {}ms)",
            debounce.as_millis()
        );
    }

    // Paths seen but not yet processed because a stage was busy. Drained
    // on a short backoff; never a blocking wait, never a drop.
    let mut pending: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let mut last_maintenance = Instant::now();

    // Startup incremental scan, run with the watcher already registered:
    // everything that changed while watch was down is caught here, and
    // anything that lands while the scan walks queues in the channel and
    // drains right after, so the walk-to-watch gap cannot lose a file. The
    // hashed paths seed the pending set: a downstream stage that was busy
    // while the scan ran retries them on the backoff, not on the hourly
    // maintenance pass.
    if !args.silent {
        eprintln!("videre watch: startup scan");
    }
    // An incomplete startup reconcile (a stage was busy, the scan lock was
    // taken) is a debt: the loop retries it on the backoff exactly like a
    // dropped-events recovery that did not finish.
    let mut rescan_owed = !apply_rescan_outcome(
        &mut pending,
        match reconcile(args, ctx) {
            Ok(outcome) => Some(outcome),
            Err(e) => {
                eprintln!("videre watch: startup scan error: {e}");
                None
            }
        },
    );
    // Test-only bounded exit: registration plus one startup pass then stop.
    // Same category as VIDERE_TEST_REQUIRE_MODELS; never documented as a knob.
    if std::env::var_os("VIDERE_WATCH_ONCE").is_some() {
        return Ok(());
    }

    loop {
        // A pending batch, an owed rescan, or the maintenance deadline: each
        // buys its own short wake instead of an idle block.
        let timeout = block_timeout(
            pending.is_empty() && !rescan_owed,
            last_maintenance.elapsed(),
            maintenance,
            PENDING_RETRY_BACKOFF,
        );
        match rx.recv_timeout(timeout) {
            Ok(Ok(batch)) => {
                if videre::watch_events::needs_full_rescan(batch.iter().map(|e| &e.event)) {
                    // The backend dropped events: one incremental full
                    // reconcile, then resume. A complete reconcile supersedes
                    // anything pending; an incomplete one (a stage was busy,
                    // the scan errored) seeds what it hashed and keeps the
                    // batch for the backoff.
                    let outcome = reconcile(args, ctx).map_err(|e| {
                        eprintln!("videre watch: rescan error: {e}");
                        e
                    });
                    rescan_owed = !apply_rescan_outcome(&mut pending, outcome.ok());
                    last_maintenance = Instant::now();
                    continue;
                }
                for p in videre::watch_events::affected_paths(batch.iter().map(|e| &e.event)) {
                    pending.insert(p);
                }
                drain_pending(args, ctx, &mut pending);
            }
            Ok(Err(errs)) => {
                for e in errs {
                    eprintln!("videre watch: event error: {e}");
                }
                // An event error may mean missed changes: reconcile to be safe.
                let outcome = reconcile(args, ctx).map_err(|e| {
                    eprintln!("videre watch: rescan error: {e}");
                    e
                });
                rescan_owed = !apply_rescan_outcome(&mut pending, outcome.ok());
                last_maintenance = Instant::now();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if rescan_owed {
                    // An earlier reconcile did not finish (a stage was busy,
                    // the scan lock was taken): attempt it again on the
                    // backoff. A complete attempt clears the debt, and the
                    // attempt itself counts as watch activity.
                    let outcome = reconcile(args, ctx).map_err(|e| {
                        eprintln!("videre watch: rescan error: {e}");
                        e
                    });
                    rescan_owed = !apply_rescan_outcome(&mut pending, outcome.ok());
                    last_maintenance = Instant::now();
                } else if last_maintenance.elapsed() >= maintenance {
                    // The safety net must be a net: an incomplete maintenance
                    // reconcile (a stage was busy, the scan lock was taken)
                    // becomes a debt retried on the backoff, not an hour of
                    // silence. A complete one supersedes any waiting batch.
                    let outcome = reconcile(args, ctx).map_err(|e| {
                        eprintln!("videre watch: maintenance error: {e}");
                        e
                    });
                    rescan_owed = !apply_rescan_outcome(&mut pending, outcome.ok());
                    last_maintenance = Instant::now();
                }
                drain_pending(args, ctx, &mut pending);
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
) {
    if pending.is_empty() {
        return;
    }
    if ctx.library.ensure_root_identity().is_err() {
        eprintln!("videre watch: library root changed; dropping batch");
        pending.clear();
        return;
    }
    let conn = match videre_core::library_db::open_existing(&ctx.library) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("videre watch: open error: {e}");
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
    if args.scan {
        match run_scan_stage(args, ctx, &conn, Some(&batch), &mut Vec::new()) {
            Ok(StageOutcome::Ran) => {}
            Ok(StageOutcome::Busy) => return, // keep pending; retry on the next backoff
            Err(e) => {
                eprintln!("videre watch: scan stage error: {e}");
                complete = false;
            }
        }
    }
    if args.faces || args.heic || args.location {
        if let Err(e) = face_db::create_faces_table(&conn) {
            eprintln!("videre watch: faces table error: {e}");
            return;
        }
        videre_core::location::ensure_location_column(&conn);
        if args.faces {
            match run_faces_stage(args, ctx, &conn) {
                Ok(StageOutcome::Ran) => {}
                Ok(StageOutcome::Busy) => complete = false,
                Err(e) => {
                    eprintln!("videre watch: faces stage error: {e}");
                    complete = false;
                }
            }
        }
        if args.heic {
            if let Err(e) = run_heic_stage(args, ctx, &conn) {
                eprintln!("videre watch: heic stage error: {e}");
            }
        }
        if args.location {
            match run_location_stage(args, ctx, &conn) {
                Ok(StageOutcome::Ran) => {}
                Ok(StageOutcome::Busy) => complete = false,
                Err(e) => {
                    eprintln!("videre watch: location stage error: {e}");
                    complete = false;
                }
            }
        }
    }
    if complete {
        pending.clear();
    }
    // A drained batch is watch activity the same way a cycle completion is.
    if let Err(e) = videre_core::pipeline_runs::record_heartbeat_in(&conn, &ctx.library, "watch") {
        eprintln!("videre watch: could not record the batch heartbeat: {e}");
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
                eprintln!("videre watch: {command} stage busy (the library is in use); will retry");
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
                eprintln!(
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
    if args.scan {
        // A scan failure this cycle does not invalidate earlier rows; log and
        // carry on to the stages below, but report the cycle as incomplete so
        // the caller keeps any waiting batch alive. A busy scan lock is the
        // same debt: the walk never ran, so nothing was re-found.
        match run_scan_stage(args, ctx, &conn, None, &mut hashed) {
            Ok(StageOutcome::Ran) => {}
            Ok(StageOutcome::Busy) => complete = false,
            Err(e) => {
                eprintln!("videre watch: scan stage error: {e}");
                complete = false;
            }
        }
    }
    if args.faces || args.heic || args.location || args.prune || args.export_xmp {
        face_db::create_faces_table(&conn)?;
        videre_core::location::ensure_location_column(&conn);
        if args.faces {
            match run_faces_stage(args, ctx, &conn) {
                Ok(StageOutcome::Ran) => {}
                Ok(StageOutcome::Busy) => complete = false,
                Err(e) => {
                    eprintln!("videre watch: faces stage error: {e}");
                    complete = false;
                }
            }
        }
        if args.heic {
            run_heic_stage(args, ctx, &conn)?;
        }
        if args.location {
            match run_location_stage(args, ctx, &conn) {
                Ok(StageOutcome::Ran) => {}
                Ok(StageOutcome::Busy) => complete = false,
                Err(e) => {
                    eprintln!("videre watch: location stage error: {e}");
                    complete = false;
                }
            }
            // Names first, then regroup: re-cluster when the GPS data changed.
            match run_locations_recluster_stage(args, ctx, &conn) {
                Ok(StageOutcome::Ran) => {}
                Ok(StageOutcome::Busy) => complete = false,
                Err(e) => {
                    eprintln!("videre watch: locations recluster stage error: {e}");
                    complete = false;
                }
            }
        }
        if args.faces {
            match run_recluster_stage(args, ctx, &conn) {
                Ok(StageOutcome::Ran) => {}
                Ok(StageOutcome::Busy) => complete = false,
                Err(e) => {
                    eprintln!("videre watch: face recluster stage error: {e}");
                    complete = false;
                }
            }
        }
        if args.prune {
            match run_prune_stage(args, ctx, &conn) {
                Ok(StageOutcome::Ran) => {}
                Ok(StageOutcome::Busy) => complete = false,
                Err(e) => {
                    eprintln!("videre watch: prune stage error: {e}");
                    complete = false;
                }
            }
        }
        if args.export_xmp {
            if let Err(e) = run_export_stage(args, ctx, &conn) {
                eprintln!("videre watch: export stage error: {e}");
            }
        }
    }

    // The cycle completed: record the heartbeat that makes `videre status`
    // able to say "watch: running (last cycle 2m ago)". Best-effort: a
    // bookkeeping failure must not kill an otherwise healthy watcher.
    if let Err(e) = videre_core::pipeline_runs::record_heartbeat_in(&conn, &ctx.library, "watch") {
        eprintln!("videre watch: could not record the cycle heartbeat: {e}");
    }
    Ok(ReconcileOutcome { hashed, complete })
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
        eprintln!("videre watch: export stage wrote {n} sidecar(s)");
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
                eprintln!("videre watch: prune stage finished with {errors} error(s)");
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
                    eprintln!(
                        "videre watch: faces stage processed {} new hash(es), {} face(s)",
                        to_process.len(),
                        result.total_faces
                    );
                }
                result.total_faces
            };
            // Detection only on this path: the global recluster is a
            // minutes-long pass at scale and runs as its own reconcile-gated
            // stage (run_recluster_stage), never here, so an event burst
            // costs scoped scans, not clustering passes.
            let _ = new_faces;
            Ok(())
        },
    )
}

/// The periodic global face recluster, gated by the persisted watermark:
/// any face id above it means new faces exist, so one pass runs and then
/// advances the mark; otherwise this is a zero-cost skip. Runs on
/// reconciles only, never on the event path: a full pass is minutes-long
/// at scale. The lock is `faces`, shared with detection and the
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
                eprintln!("videre watch: face recluster stage busy; will retry");
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
                    eprintln!("videre watch: face recluster up to date");
                }
                return Ok(());
            }
            let clustering = run_clustering(
                conn,
                0.6,
                3,
                videre_core::face_cluster::DEFAULT_MERGE_SIM,
                videre_core::face_cluster::DEFAULT_MIN_FACE_PX,
                videre_core::face_cluster::DEFAULT_MAX_GENERIC_SIM,
                videre_core::face_cluster::DEFAULT_MAX_LANDMARK_ERR,
                videre_core::face_cluster::DEFAULT_MIN_BLUR,
                1.0,
                args.silent,
            )?;
            videre_core::face_db::advance_recluster_watermark(conn)?;
            if !args.silent {
                eprintln!(
                    "videre watch: {}",
                    format_clustering_only_summary(clustering, 0.6)
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
        // on heic_via_quicklook. This means this call site does NOT get
        // lever-2a's render-size-cap treatment other watch-adjacent callers
        // do; the two levers are in tension here and full-res wins because
        // avoiding a second full qlmanage decode during `videre faces` is
        // the bigger saving of the two.
        match videre_core::heic::heic_via_quicklook(&path, "watch", None) {
            Some(img) => {
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
            None => {
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
    if !args.silent && (converted > 0 || failed > 0) {
        if failed > 0 {
            eprintln!("videre watch: heic stage cached {converted} thumbnail(s), {failed} failed");
        } else {
            eprintln!("videre watch: heic stage cached {converted} thumbnail(s)");
        }
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
                eprintln!("videre watch: location stage busy; will retry");
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
                eprintln!("videre watch: location stage resolved {resolved} coordinate(s)");
            }
            Ok(())
        },
    )
}

/// The location recluster: the same from-scratch pass the standalone command
/// runs, gated by a fingerprint over the GPS-bearing data. The gate is one
/// query; the recompute is the expensive part. Runs on the reconcile only
/// (never in the event-batch drain) - cluster staleness is map-view staleness,
/// and reconcile freshness is the contract. Tracked under the `locations` row
/// and lock, the same work the standalone command does, so status liveness
/// needs no new machinery. A standalone run at a non-default radius is a manual
/// choice: the watcher leaves it alone rather than silently reclustering at the
/// default (the radius the last recompute used is recorded by that recompute).
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
                        eprintln!(
                            "videre watch: locations: manual radius {r}km in effect; \
                             the watcher leaves it alone"
                        );
                    }
                    return Ok(());
                }
            }
            if stored_fp.as_deref() == Some(live.as_str()) {
                if !args.silent {
                    eprintln!("videre watch: locations up to date");
                }
                return Ok(());
            }
            // recompute_all records the fresh fingerprint and radius itself.
            location_cluster::recompute_all(conn, &ctx.library.cache, radius, args.silent)?;
            if !args.silent {
                eprintln!("videre watch: location clusters rebuilt");
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
                eprintln!(
                    "videre watch: scan stage considering {} of {} file(s) ({})",
                    paths.len(),
                    walked,
                    selection.describe()
                );
            }
            // Incremental in both modes: a spurious event on an unchanged
            // file hashes nothing, exactly like an unchanged file in a full
            // walk. `--similar` is not a watch concept, so no phash.
            let sigs = videre_core::db::stored_signatures(conn).unwrap_or_default();
            let paths: Vec<_> = paths
                .into_iter()
                .filter(|p| videre::incremental::needs_processing(&sigs, p, false))
                .collect();
            let records: Vec<types::FileRecord> = paths
                .par_iter()
                .filter_map(|path| hasher::hash_file(path).ok())
                .collect();
            // Reported to the caller: the startup pass feeds these into the
            // pending set, so a downstream stage that was busy while the scan
            // ran retries them on the backoff instead of waiting an hour.
            hashed.extend(records.iter().map(|r| std::path::PathBuf::from(&r.path)));
            sqlite_output::write_records_in(conn, &ctx.library, &records)?;
            let prec = args.xmp.resolve_from(&ctx.library.settings)?;
            // XMP reconcile covers the files hashed in this run. A scoped
            // (event) run reconciles just what it hashed; the full walk's
            // sidecar-change sweep keeps its own incremental rule through
            // reconcile_xmp_in.
            let changed: std::collections::HashSet<String> =
                records.iter().map(|r| r.path.clone()).collect();
            crate::xmp::reconcile_xmp_in(conn, &ctx.library, prec, &changed, args.silent)?;
            if !args.silent {
                eprintln!("videre watch: scan stage wrote {} record(s)", records.len());
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
                exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);",
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
