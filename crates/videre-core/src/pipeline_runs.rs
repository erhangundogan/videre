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
    guard.ensure_matches(ctx, command)?;
    // The guard's match is a wiring check on path strings; this rechecks that
    // the root still names the same library before any run row is written, so
    // a root swapped out between guard acquisition and now is refused rather
    // than recorded against whatever now sits at the path.
    ctx.ensure_root_identity()?;
    ensure_pipeline_runs_table(conn)?;
    start_run(conn, command)?;
    let started = std::time::Instant::now();
    let result = f();
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    match result {
        Ok(value) => {
            finish_run(conn, command, "success", duration_ms, None)?;
            Ok(value)
        }
        Err(error) => {
            // Record the failure, but never let a bookkeeping error mask the
            // real one: the original error is what the caller acted on.
            if let Err(record_error) = finish_run(
                conn,
                command,
                "failed",
                duration_ms,
                Some(&error.to_string()),
            ) {
                return Err(error.context(format!(
                    "also could not record the failed run: {record_error}"
                )));
            }
            Err(error)
        }
    }
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
        let row: Option<(String, Option<i64>, String)> = conn
            .query_row(
                "SELECT started_at, duration_ms, status FROM pipeline_runs WHERE command = ?1",
                params![command],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let currently_running = crate::library_locks::command_locked(ctx, command)?;
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
        out.push(PipelineRunStatus {
            command: command.to_string(),
            last_run_at,
            status,
            duration_ms,
            currently_running,
        });
    }
    Ok(out)
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
pub fn install_sigint_handler_in(
    ctx: std::sync::Arc<crate::library::LibraryContext>,
    command: &'static str,
) -> Result<()> {
    ctx.ensure_root_identity()
        .context("validating the library before installing the SIGINT handler")?;
    ctrlc::set_handler(move || {
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
    .context("installing SIGINT handler")
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
}
