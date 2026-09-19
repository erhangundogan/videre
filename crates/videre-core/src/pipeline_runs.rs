//! Per-command pipeline run history and liveness, surfaced by `videre stats`
//! and other dashboard-style callers.
//! See docs/superpowers/specs/2026-07-31-dashboard-stats-pass-b-design.md
//! for the full design, and in particular why `track_in()` below does not rely
//! on Drop/RAII for the success/failure bookkeeping: the library-scoped command
//! lock's release does, backstopped by the OS releasing `flock` on any process
//! death.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// The eight commands tracked so far. Extended with `locations` on
/// 2026-08-01 (was seven after `prune`'s addition earlier the same day).
/// It's a clean fit for the same one-shot start/finish model the others
/// use: `videre locations` is a full-recompute batch pass, not an
/// interactive per-query command. `gallery`, `search`, `mcp`, and `config`
/// remain deliberately excluded: `report --faces`/`--show-faces` and `mcp`
/// are long-running servers with no natural "finished" moment (the same
/// reason `videre watch` itself is excluded. See below), `search` is an
/// interactive per-query command rather than a library-processing pipeline
/// stage (true even for its new `--location` mode, which is a single query
/// like any other `search` invocation, not a batch job), and `config` is a
/// trivial instant read/write with nothing meaningful to time. Revisit only
/// if a real driver for tracking one of those emerges. See TECH_DEBT.md.
///
/// `videre watch` itself is deliberately not in this list, it has no
/// "finished" moment during normal operation, so it gets its own liveness
/// lock (see `watch_lock_path`) but no `pipeline_runs` row.
pub const TRACKED_COMMANDS: [&str; 8] = [
    "scan",
    "faces",
    "embed",
    "classify",
    "dedupe",
    "fix-dates",
    "prune",
    "locations",
];

pub fn ensure_pipeline_runs_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pipeline_runs (
            command      TEXT PRIMARY KEY,
            started_at   TEXT NOT NULL,
            finished_at  TEXT,
            status       TEXT NOT NULL,
            duration_ms  INTEGER,
            summary      TEXT
        );",
    )
}

pub fn start_run(conn: &Connection, command: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO pipeline_runs (command, started_at, status)
         VALUES (?1, datetime('now'), 'running')
         ON CONFLICT(command) DO UPDATE SET
             started_at = excluded.started_at,
             status = 'running',
             finished_at = NULL,
             duration_ms = NULL,
             summary = NULL",
        params![command],
    )?;
    Ok(())
}

pub fn finish_run(
    conn: &Connection,
    command: &str,
    status: &str,
    duration_ms: i64,
    summary: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE pipeline_runs SET
             finished_at = datetime('now'),
             status = ?2,
             duration_ms = ?3,
             summary = ?4
         WHERE command = ?1",
        params![command, status, duration_ms, summary],
    )?;
    Ok(())
}

/// Wraps `f` with pipeline-run bookkeeping for a command whose per-command
/// lock is already held as a
/// [`library_locks::CommandGuard`](crate::library_locks::CommandGuard).
///
/// The guard is verified rather than trusted: it must have been taken for
/// exactly `ctx` and `command`, because a mismatched pairing would record
/// one library's run against another (or one command's row against another
/// command's) with nothing visibly wrong. The model is `start_run` before
/// `f`, `success`/`failed` (with the error message) after, all before
/// returning. It never reacquires the supplied guard, so converting a command
/// to the library-scoped locks cannot double-lock.
pub fn track_in<T, F>(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
    guard: &crate::library_locks::CommandGuard,
    command: &str,
    f: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    track_in_as(conn, ctx, guard, command, command, f)
}

/// Like [`track_in`], but the recorded label may differ from the command the
/// guard was taken for. Watch's location stage shares the `locations` command
/// lock with the standalone clustering recompute (so the two cannot overlap
/// on SQLite's single writer) while writing its own `location-names` row.
/// The guard is still verified to belong to this library and to the command
/// it was actually taken for, so a mismatched guard is refused exactly as
/// `track_in` refuses one; only the row's name comes from `label`.
pub fn track_in_as<T, F>(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
    guard: &crate::library_locks::CommandGuard,
    lock_command: &str,
    label: &str,
    f: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    guard.ensure_matches(ctx, lock_command)?;
    // The guard's match is a wiring check on path strings; this rechecks that
    // the root still names the same library before any run row is written, so
    // a root swapped out between guard acquisition and now is refused rather
    // than recorded against whatever now sits at the path.
    ctx.ensure_root_identity()?;
    ensure_pipeline_runs_table(conn)?;
    start_run(conn, label)?;
    let started = std::time::Instant::now();
    let result = f();
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    match result {
        Ok(value) => {
            finish_run(conn, label, "success", duration_ms, None)?;
            Ok(value)
        }
        Err(error) => {
            // Record the failure, but never let a bookkeeping error mask the
            // real one: the original error is what the caller acted on.
            if let Err(record_error) =
                finish_run(conn, label, "failed", duration_ms, Some(&error.to_string()))
            {
                return Err(error.context(format!(
                    "also could not record the failed run: {record_error}"
                )));
            }
            Err(error)
        }
    }
}

/// Records that a recurring command (`watch`) completed one cycle. Unlike a
/// start/stop run, the heartbeat is the whole record: `started_at` is the
/// moment of the last successful cycle, and there is no duration to keep. A
/// watcher that dies mid-cycle leaves the previous cycle's heartbeat, so a
/// dead watcher reads as a stale last-cycle time, never as a fake crash.
pub fn record_heartbeat_in(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
    command: &str,
) -> Result<()> {
    // Same identity check as `track_in`: a root swapped between the cycle's
    // start and now must not have its cycle recorded against another library.
    ctx.ensure_root_identity()?;
    ensure_pipeline_runs_table(conn)?;
    conn.execute(
        "INSERT INTO pipeline_runs (command, started_at, status, summary)
         VALUES (?1, datetime('now'), 'success', 'last successful cycle')
         ON CONFLICT(command) DO UPDATE SET
             started_at = excluded.started_at,
             status = 'success',
             finished_at = NULL,
             duration_ms = NULL,
             summary = excluded.summary",
        params![command],
    )?;
    Ok(())
}

/// Every tracked command's last run and current liveness. Liveness is probed
/// through the library's own lock files (`library_locks::command_locked`), so a
/// library-scoped command shows up as running exactly when a library-scoped
/// reader asks; a `running` row whose lock no live process holds reads back as
/// `crashed`.
pub fn read_all_in(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
) -> Result<Vec<PipelineRunStatus>> {
    ensure_pipeline_runs_table(conn)?;
    let mut out = Vec::with_capacity(TRACKED_COMMANDS.len());
    for command in TRACKED_COMMANDS {
        out.push(read_one_in(conn, ctx, command)?);
    }
    // `watch` is deliberately absent from TRACKED_COMMANDS: it is a recurring
    // loop, not a start/stop run, so it never takes the per-command run-row
    // machinery. Its heartbeat (see `record_heartbeat_in`) makes it reportable
    // all the same; it appears here only once a heartbeat exists, so a library
    // never watched is not lectured about a stage it never started.
    if conn
        .query_row(
            "SELECT 1 FROM pipeline_runs WHERE command = 'watch'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    {
        out.push(read_one_in(conn, ctx, "watch")?);
    }
    // `location-names` is watch's incremental reverse-geocoding stage, on the
    // same only-once-a-row-exists rule as the heartbeat: a library that never
    // watched with --location is not lectured about it. It is deliberately
    // distinct from `locations`, whose row means the standalone clustering
    // recompute, and deliberately not called `geocode`, which in this codebase
    // means forward geocoding (see `videre_core::geocode`).
    if conn
        .query_row(
            "SELECT 1 FROM pipeline_runs WHERE command = 'location-names'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    {
        out.push(read_one_in(conn, ctx, "location-names")?);
    }
    // `face-recluster` is watch's periodic global face recluster, on the same
    // only-once-a-row-exists rule: a library that never watched with --faces
    // is not lectured about repair passes it never ran. Distinct from
    // `faces`, whose row means a detection run.
    if conn
        .query_row(
            "SELECT 1 FROM pipeline_runs WHERE command = 'face-recluster'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some()
    {
        out.push(read_one_in(conn, ctx, "face-recluster")?);
    }
    Ok(out)
}

/// One command's row plus lock-derived liveness. Shared by the tracked loop
/// and the watch heartbeat read, which need identical semantics.
///
/// The `locations` row is the one special case: its lock is shared with
/// watch's location-names stage, which holds both locks while it runs, so a
/// recompute is active exactly when the shared lock is held and the stage's
/// own lock is not. Every other command's liveness is its own lock.
fn read_one_in(
    conn: &Connection,
    ctx: &crate::library::LibraryContext,
    command: &str,
) -> Result<PipelineRunStatus> {
    let row: Option<(String, Option<i64>, String)> = conn
        .query_row(
            "SELECT started_at, duration_ms, status FROM pipeline_runs WHERE command = ?1",
            params![command],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let currently_running = match command {
        "locations" => {
            crate::library_locks::command_locked(ctx, "locations")?
                && !crate::library_locks::command_locked(ctx, "location-names")?
        }
        other => crate::library_locks::command_locked(ctx, other)?,
    };
    let (last_run_at, status, duration_ms) = match row {
        None => (None, None, None),
        Some((started_at, duration_ms, stored_status)) => {
            let status = if stored_status == "running" && !currently_running {
                "crashed".to_string()
            } else {
                stored_status
            };
            (Some(started_at), Some(status), duration_ms)
        }
    };
    Ok(PipelineRunStatus {
        command: command.to_string(),
        last_run_at,
        status,
        duration_ms,
        currently_running,
    })
}

/// Install a SIGINT handler that marks `command`'s row `interrupted` and exits
/// 130, against the library the context pins.
///
/// The library's identity is validated before the handler is installed (and
/// again inside it): a handler bound to a root that no longer names its
/// library would write another library's database on the way out. The
/// handler's connection opens without `CREATE`, so an exiting process can
/// never conjure a database that initialization is responsible for. Same
/// best-effort contract as the global version: errors inside the handler
/// are swallowed, there is no useful way to report them once the process is
/// already exiting on a signal.
/// A `videre pipeline` runs several tracked stages in one process, but the
/// `ctrlc` crate permits exactly one handler per process. So the handler is
/// installed once and then retargeted: each stage records the command it is
/// running, and the single handler marks whichever command is current when the
/// signal arrives. Re-installing is a silent no-op rather than the "already
/// registered" error every stage after the first used to raise.
static SIGINT_INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static SIGINT_COMMAND: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);

pub fn install_sigint_handler_in(
    ctx: std::sync::Arc<crate::library::LibraryContext>,
    command: &'static str,
) -> Result<()> {
    ctx.ensure_root_identity()
        .context("validating the library before installing the SIGINT handler")?;
    // Point the single, process-wide handler at the stage now running.
    if let Ok(mut current) = SIGINT_COMMAND.lock() {
        *current = Some(command);
    }
    // Later stages in the same process only retarget the handler above; one
    // handler per process is all `ctrlc` allows.
    if SIGINT_INSTALLED.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    ctrlc::set_handler(move || {
        let command = SIGINT_COMMAND
            .lock()
            .ok()
            .and_then(|c| *c)
            .unwrap_or(command);
        if ctx.ensure_root_identity().is_ok() {
            if let Ok(conn) = crate::library_db::open_without_create(&ctx.paths.db) {
                let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
                let started_at: Option<String> = conn
                    .query_row(
                        "SELECT started_at FROM pipeline_runs WHERE command = ?1",
                        params![command],
                        |r| r.get(0),
                    )
                    .optional()
                    .ok()
                    .flatten();
                let duration_ms = started_at
                    .and_then(|s| {
                        chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok()
                    })
                    .map(|started| {
                        (chrono::Utc::now().naive_utc() - started)
                            .num_milliseconds()
                            .max(0)
                    })
                    .unwrap_or(0);
                let _ = finish_run(&conn, command, "interrupted", duration_ms, None);
            }
        }
        std::process::exit(130);
    })
    .context("installing SIGINT handler")?;
    SIGINT_INSTALLED.store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PipelineRunStatus {
    pub command: String,
    pub last_run_at: Option<String>,
    /// "running" | "success" | "failed" | "interrupted" | "crashed" | None if never run.
    /// "crashed" is computed here, never a stored value. See the design doc.
    pub status: Option<String>,
    pub duration_ms: Option<i64>,
    pub currently_running: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_pipeline_runs_table(&conn).unwrap();
        conn
    }

    #[test]
    fn heartbeat_records_last_cycle_and_reads_back() {
        let (_t, ctx, conn) = in_library();
        // A fresh library has never run watch: no row, no liveness time.
        assert!(read_all_in(&conn, &ctx)
            .unwrap()
            .iter()
            .all(|r| r.command != "watch"));
        record_heartbeat_in(&conn, &ctx, "watch").unwrap();
        let w = read_all_in(&conn, &ctx)
            .unwrap()
            .into_iter()
            .find(|r| r.command == "watch")
            .expect("the heartbeat row must surface in the run read");
        assert!(w.last_run_at.is_some(), "started_at is the last-cycle time");
        assert_eq!(w.status.as_deref(), Some("success"));
        assert!(!w.currently_running, "no watch process holds the lock");

        // A second heartbeat is an update of the same row, not an error or a
        // second entry: recurring commands have exactly one now.
        record_heartbeat_in(&conn, &ctx, "watch").unwrap();
        assert_eq!(
            read_all_in(&conn, &ctx)
                .unwrap()
                .iter()
                .filter(|r| r.command == "watch")
                .count(),
            1
        );
    }

    #[test]
    fn ensure_pipeline_runs_table_is_idempotent() {
        let conn = test_db();
        ensure_pipeline_runs_table(&conn).unwrap();
    }

    #[test]
    fn start_run_then_finish_run_records_success() {
        let conn = test_db();
        start_run(&conn, "embed").unwrap();

        let status: String = conn
            .query_row(
                "SELECT status FROM pipeline_runs WHERE command = 'embed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "running");

        finish_run(&conn, "embed", "success", 1234, None).unwrap();

        let (status, duration_ms, summary): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT status, duration_ms, summary FROM pipeline_runs WHERE command = 'embed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "success");
        assert_eq!(duration_ms, 1234);
        assert_eq!(summary, None);
    }

    #[test]
    fn start_run_upserts_resetting_prior_finish_fields() {
        let conn = test_db();
        start_run(&conn, "embed").unwrap();
        finish_run(&conn, "embed", "failed", 500, Some("boom")).unwrap();

        start_run(&conn, "embed").unwrap(); // second run begins

        let (status, duration_ms, summary): (String, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT status, duration_ms, summary FROM pipeline_runs WHERE command = 'embed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "running");
        assert_eq!(duration_ms, None);
        assert_eq!(summary, None);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pipeline_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "upsert, not a second row");
    }

    /// One library with its state and locks directories in place, plus a
    /// connection with the runs table: the minimum the library-scoped
    /// bookkeeping needs, without pulling the database layer's
    /// initialization in. Locks only need the directories to exist.
    fn in_library() -> (
        tempfile::TempDir,
        crate::library::LibraryContext,
        Connection,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = crate::library::LibraryContext::new(&root, &temp.path().join("cache")).unwrap();
        std::fs::create_dir_all(&ctx.paths.locks).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        ensure_pipeline_runs_table(&conn).unwrap();
        (temp, ctx, conn)
    }

    #[test]
    fn track_in_records_runs_under_an_already_held_command_guard() {
        let (_t, ctx, conn) = in_library();
        let guard = crate::library_locks::try_command(&ctx, "scan").unwrap();
        let result = track_in(&conn, &ctx, &guard, "scan", || Ok(7)).unwrap();
        assert_eq!(result, 7);
        let (status, summary): (String, Option<String>) = conn
            .query_row(
                "SELECT status, summary FROM pipeline_runs WHERE command = 'scan'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "success");
        assert_eq!(summary, None);
        // The other commands' rows are untouched: one command's bookkeeping
        // never overwrites another's.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pipeline_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let failed: Result<()> =
            track_in(&conn, &ctx, &guard, "scan", || Err(anyhow::anyhow!("boom")));
        failed.unwrap_err();
        let (status, summary): (String, Option<String>) = conn
            .query_row(
                "SELECT status, summary FROM pipeline_runs WHERE command = 'scan'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(summary.as_deref(), Some("boom"));
    }

    #[test]
    fn track_in_refuses_a_guard_from_another_command_or_library() {
        let (_t, ctx, conn) = in_library();
        let guard = crate::library_locks::try_command(&ctx, "scan").unwrap();
        // A guard held for scan must not bookkeep embed's row.
        let result: Result<()> = track_in(&conn, &ctx, &guard, "embed", || Ok(()));
        let err = result.unwrap_err();
        assert!(format!("{err:#}").contains("scan"), "{err:#}");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pipeline_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "a refused guard must write no row");

        // Nor may one library's guard bookkeep another library's run, even
        // under the same command name.
        let temp = tempfile::tempdir().unwrap();
        let other_root = temp.path().join("other");
        std::fs::create_dir(&other_root).unwrap();
        let other =
            crate::library::LibraryContext::new(&other_root, &temp.path().join("cache")).unwrap();
        let result: Result<()> = track_in(&conn, &other, &guard, "scan", || Ok(()));
        let err = result.unwrap_err();
        assert!(
            format!("{err:#}").contains(other_root.file_name().unwrap().to_string_lossy().as_ref()),
            "{err:#}"
        );
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pipeline_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn read_all_in_answers_liveness_from_the_library_locks() {
        let (_t, ctx, conn) = in_library();
        let guard = crate::library_locks::try_command(&ctx, "scan").unwrap();
        track_in(&conn, &ctx, &guard, "scan", || Ok(())).unwrap();
        // Released: the finished run is not running.
        drop(guard);
        // A held command lock shows up as running to a library-scoped
        // reader, with no row of its own: the probe is what says live, not
        // the presence of a row.
        let _faces = crate::library_locks::try_command(&ctx, "faces").unwrap();
        let statuses = read_all_in(&conn, &ctx).unwrap();
        let scan = statuses.iter().find(|s| s.command == "scan").unwrap();
        assert_eq!(scan.status.as_deref(), Some("success"));
        assert!(!scan.currently_running);
        let faces = statuses.iter().find(|s| s.command == "faces").unwrap();
        assert_eq!(faces.status, None);
        assert!(faces.currently_running);
    }

    #[test]
    fn location_names_stage_surfaces_only_once_a_row_exists() {
        let (_t, ctx, conn) = in_library();
        // A library never watched with --location has no location-names row:
        // the read must not lecture about a stage it never ran (same rule as
        // the watch heartbeat above).
        assert!(read_all_in(&conn, &ctx)
            .unwrap()
            .iter()
            .all(|r| r.command != "location-names"));
        // One watch cycle writes the row; from then on the stage reports
        // like any tracked command.
        start_run(&conn, "location-names").unwrap();
        finish_run(&conn, "location-names", "success", 5, None).unwrap();
        let names = read_all_in(&conn, &ctx)
            .unwrap()
            .into_iter()
            .find(|r| r.command == "location-names")
            .expect("a written location-names row must surface in the run read");
        assert_eq!(names.status.as_deref(), Some("success"));
        assert!(!names.currently_running);
    }

    #[test]
    fn a_running_location_names_stage_reads_running_not_crashed() {
        // The stage holds BOTH locks while it runs: `locations` for
        // coordination with the standalone recompute, and its own
        // `location-names` lock as its identity. A healthy cycle therefore
        // reads as running and active on both rows' behalf, never crashed,
        // and `status --check` cannot fail during normal operation.
        let (_t, ctx, conn) = in_library();
        let guard = crate::library_locks::try_command(&ctx, "locations").unwrap();
        let names_guard = crate::library_locks::try_command(&ctx, "location-names").unwrap();
        start_run(&conn, "location-names").unwrap();

        let names = read_all_in(&conn, &ctx)
            .unwrap()
            .into_iter()
            .find(|r| r.command == "location-names")
            .expect("a started location-names row must surface");
        assert_eq!(
            names.status.as_deref(),
            Some("running"),
            "an actively running stage is running, not crashed"
        );
        assert!(names.currently_running);
        // The lock's holder is the location-names stage, so the locations
        // entry must not claim a recompute is running on the strength of the
        // shared lock alone.
        let locations = read_all_in(&conn, &ctx)
            .unwrap()
            .into_iter()
            .find(|r| r.command == "locations")
            .unwrap();
        assert!(!locations.currently_running);

        // Once the stage is gone without finishing, the stale running row
        // reads back as exactly what it is.
        drop(guard);
        drop(names_guard);
        let names = read_all_in(&conn, &ctx)
            .unwrap()
            .into_iter()
            .find(|r| r.command == "location-names")
            .unwrap();
        assert_eq!(names.status.as_deref(), Some("crashed"));
    }

    #[test]
    fn a_stale_names_row_does_not_mask_a_running_recompute() {
        // Watch died mid-stage: the location-names row is left "running"
        // while the OS released both locks. A standalone recompute then
        // starts and takes the locations lock. The stale row must read as
        // crashed, and the live recompute must keep its running report: the
        // stage's own lock being free is what proves the holder is the
        // recompute, not the stage.
        let (_t, ctx, conn) = in_library();
        start_run(&conn, "location-names").unwrap();

        let guard = crate::library_locks::try_command(&ctx, "locations").unwrap();
        start_run(&conn, "locations").unwrap();

        let statuses = read_all_in(&conn, &ctx).unwrap();
        let names = statuses
            .iter()
            .find(|r| r.command == "location-names")
            .unwrap();
        assert_eq!(
            names.status.as_deref(),
            Some("crashed"),
            "a stale row must not borrow the recompute's lock liveness"
        );
        let locations = statuses.iter().find(|r| r.command == "locations").unwrap();
        assert_eq!(locations.status.as_deref(), Some("running"));
        assert!(
            locations.currently_running,
            "the recompute is the real live holder and must be reported as such"
        );
        drop(guard);
    }

    #[test]
    fn track_in_as_records_a_label_under_another_commands_guard() {
        // Watch's location stage shares the `locations` command lock with the
        // standalone recompute while writing its own `location-names` row:
        // the guard is verified against the command it was taken for, and the
        // label is what lands in the table.
        let (_t, ctx, conn) = in_library();
        let guard = crate::library_locks::try_command(&ctx, "locations").unwrap();
        track_in_as(
            &conn,
            &ctx,
            &guard,
            "locations",
            "location-names",
            || Ok(()),
        )
        .unwrap();
        let (command, status): (String, String) = conn
            .query_row("SELECT command, status FROM pipeline_runs", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(command, "location-names");
        assert_eq!(status, "success");

        // A guard taken for a different command than the claimed lock is
        // still refused, exactly as track_in refuses one.
        let guard = crate::library_locks::try_command(&ctx, "scan").unwrap();
        let result: Result<()> = track_in_as(
            &conn,
            &ctx,
            &guard,
            "locations",
            "location-names",
            || Ok(()),
        );
        assert!(
            result.is_err(),
            "a scan guard must not bookkeep under the locations lock"
        );
    }

    #[test]
    fn install_sigint_handler_in_validates_the_library_before_installing() {
        // A context whose root was replaced after construction must be
        // refused before any handler is installed, so this deliberately
        // never reaches ctrlc::set_handler (only one handler can exist per
        // process, and another test in this suite owns that slot).
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = std::sync::Arc::new(
            crate::library::LibraryContext::new(&root, &temp.path().join("cache")).unwrap(),
        );
        std::fs::rename(&root, temp.path().join("moved")).unwrap();
        std::fs::create_dir(&root).unwrap();
        let err = install_sigint_handler_in(ctx, "scan").unwrap_err();
        assert!(format!("{err:#}").contains("no longer names"), "{err:#}");
    }

    #[test]
    fn install_sigint_handler_in_is_idempotent_within_a_process() {
        // A pipeline installs the handler once and retargets it for each later
        // stage, so a second install must succeed silently rather than raise
        // "already registered" - the warning pipeline used to print between
        // scan and faces.
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("photos");
        std::fs::create_dir(&root).unwrap();
        let ctx = std::sync::Arc::new(
            crate::library::LibraryContext::new(&root, &temp.path().join("cache")).unwrap(),
        );
        install_sigint_handler_in(ctx.clone(), "scan").expect("first install");
        install_sigint_handler_in(ctx, "faces")
            .expect("a second install in the same process must be a no-op, not an error");
    }
}
