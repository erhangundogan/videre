//! Per-command pipeline run history and liveness, for the desktop app's home
//! dashboard. See docs/superpowers/specs/2026-07-31-dashboard-stats-pass-b-design.md
//! for the full design, and in particular why `track()` below does not rely
//! on Drop/RAII for the success/failure bookkeeping - only the lock's
//! release does, and even that is backstopped by the OS releasing `flock` on
//! any process death.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// The eight commands tracked so far. Extended with `locations` on
/// 2026-08-01 (was seven after `prune`'s addition earlier the same day) -
/// it's a clean fit for the same one-shot start/finish model the others
/// use: `videre locations` is a full-recompute batch pass, not an
/// interactive per-query command. `report`, `search`, `mcp`, and `config`
/// remain deliberately excluded: `report --faces`/`--show-faces` and `mcp`
/// are long-running servers with no natural "finished" moment (the same
/// reason `videre watch` itself is excluded - see below), `search` is an
/// interactive per-query command rather than a library-processing pipeline
/// stage (true even for its new `--location` mode, which is a single query
/// like any other `search` invocation, not a batch job), and `config` is a
/// trivial instant read/write with nothing meaningful to time. Revisit only
/// if a real driver for tracking one of those emerges - see TECH_DEBT.md.
///
/// `videre watch` itself is deliberately not in this list - it has no
/// "finished" moment during normal operation, so it gets its own liveness
/// lock (see `watch_lock_path`) but no `pipeline_runs` row.
pub const TRACKED_COMMANDS: [&str; 8] =
    ["scan", "faces", "embed", "classify", "dedupe", "fix-dates", "prune", "locations"];

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

/// Holds an open, flock'd file for as long as it's alive. Dropping it closes
/// the file, which releases the flock - the OS does the same thing
/// automatically if the process dies without ever dropping this (SIGKILL,
/// power loss), so there is no correctness dependency on Drop actually
/// running; it's just the tidy path.
pub struct LockGuard(#[allow(dead_code)] File);

fn lock_path_for(db_path: &Path, command: &str) -> Result<PathBuf> {
    let canonical = db_path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", db_path.display()))?;
    Ok(PathBuf::from(format!("{}.{command}.lock", canonical.display())))
}

/// Acquires an exclusive, non-blocking advisory lock scoped to this exact
/// database file and command. Fails immediately (refusing the run, per the
/// concurrency decision in the design doc) if another live process already
/// holds it - never blocks waiting for it to free up.
pub fn acquire_lock(db_path: &Path, command: &str) -> Result<LockGuard> {
    use fs2::FileExt;
    let lock_path = lock_path_for(db_path, command)?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock file {}", lock_path.display()))?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("{command} is already running against {}", db_path.display()))?;
    Ok(LockGuard(file))
}

/// True if another live process currently holds `command`'s lock for
/// `db_path`. Never blocks: probes with a non-blocking try-lock and releases
/// immediately if it succeeds, so this is safe to call from a read path.
pub fn is_locked(db_path: &Path, command: &str) -> Result<bool> {
    use fs2::FileExt;
    let lock_path = lock_path_for(db_path, command)?;
    if !lock_path.exists() {
        return Ok(false);
    }
    let file = OpenOptions::new()
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock file {}", lock_path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&file).ok();
            Ok(false)
        }
        Err(_) => Ok(true),
    }
}

/// Wraps `f` with pipeline-run bookkeeping: refuses to start if `command` is
/// already running against `db_path`, records a `running` row before calling
/// `f`, then records `success`/`failed` (with `f`'s error message, if any)
/// once `f` returns - all before this function itself returns. Every
/// `std::process::exit` call site this design touches happens strictly after
/// its wrapped operation already returned a `Result` (see the design doc's
/// "key design insight"), so this finalization is never skipped by an exit
/// call - only an actual crash mid-`f()` skips it, which is exactly what the
/// lock-based `crashed` detection in `read_all` is for.
pub fn track<T>(
    conn: &Connection,
    db_path: &Path,
    command: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    ensure_pipeline_runs_table(conn)?;
    let _lock = acquire_lock(db_path, command)?;
    start_run(conn, command)?;
    let started = std::time::Instant::now();
    let result = f();
    let duration_ms = started.elapsed().as_millis() as i64;
    match &result {
        Ok(_) => finish_run(conn, command, "success", duration_ms, None)?,
        Err(e) => finish_run(conn, command, "failed", duration_ms, Some(&e.to_string()))?,
    }
    result
}

/// Installs a SIGINT handler that marks `command`'s row `interrupted` (using
/// its already-recorded `started_at` to compute duration) and exits 130, the
/// standard SIGINT exit code. Call this once, after `track()`'s `start_run`
/// has already written the `running` row for `command` - the handler opens
/// its own fresh connection since the main thread's `Connection` isn't
/// safely shareable across the handler boundary. Best-effort: any error
/// inside the handler is swallowed (there's no useful way to report it once
/// the process is already exiting on a signal).
pub fn install_sigint_handler(db_path: &Path, command: &'static str) -> Result<()> {
    let db_path = db_path.to_path_buf();
    ctrlc::set_handler(move || {
        if let Ok(conn) = Connection::open(&db_path) {
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
                .and_then(|s| chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok())
                .map(|started| {
                    (chrono::Utc::now().naive_utc() - started).num_milliseconds().max(0)
                })
                .unwrap_or(0);
            let _ = finish_run(&conn, command, "interrupted", duration_ms, None);
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
    /// "crashed" is computed here, never a stored value - see the design doc.
    pub status: Option<String>,
    pub duration_ms: Option<i64>,
    pub currently_running: bool,
}

pub fn read_all(conn: &Connection, db_path: &Path) -> Result<Vec<PipelineRunStatus>> {
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

        let currently_running = is_locked(db_path, command)?;

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
            .query_row("SELECT status FROM pipeline_runs WHERE command = 'embed'", [], |r| r.get(0))
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

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM pipeline_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "upsert, not a second row");
    }

    #[test]
    fn acquire_lock_refuses_a_second_concurrent_acquisition() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let db_path = db_file.path();

        let _first = acquire_lock(db_path, "faces").unwrap();
        let second = acquire_lock(db_path, "faces");
        assert!(second.is_err(), "a second concurrent lock on the same command must be refused");
    }

    #[test]
    fn acquire_lock_allows_different_commands_concurrently() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let db_path = db_file.path();

        let _faces_lock = acquire_lock(db_path, "faces").unwrap();
        let embed_lock = acquire_lock(db_path, "embed");
        assert!(embed_lock.is_ok(), "different commands must not contend for the same lock");
    }

    #[test]
    fn acquire_lock_is_available_again_after_release() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let db_path = db_file.path();

        {
            let _lock = acquire_lock(db_path, "scan").unwrap();
        } // dropped here, releasing the flock

        let second = acquire_lock(db_path, "scan");
        assert!(second.is_ok(), "lock must be available again once the guard is dropped");
    }

    #[test]
    fn track_records_success_and_returns_the_value() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let result = track(&conn, db_file.path(), "embed", || Ok(42)).unwrap();
        assert_eq!(result, 42);

        let status: String = conn
            .query_row("SELECT status FROM pipeline_runs WHERE command = 'embed'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "success");
    }

    #[test]
    fn track_records_failure_with_the_error_message() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let result: Result<()> = track(&conn, db_file.path(), "classify", || {
            Err(anyhow::anyhow!("something broke"))
        });
        assert!(result.is_err());

        let (status, summary): (String, Option<String>) = conn
            .query_row(
                "SELECT status, summary FROM pipeline_runs WHERE command = 'classify'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(summary.as_deref(), Some("something broke"));
    }

    #[test]
    fn track_refuses_when_already_locked() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let _held = acquire_lock(db_file.path(), "scan").unwrap();
        let result: Result<()> = track(&conn, db_file.path(), "scan", || Ok(()));
        assert!(result.is_err(), "track must refuse to run while the lock is already held");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pipeline_runs WHERE command = 'scan'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn read_all_reports_none_for_a_never_run_command() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let statuses = read_all(&conn, db_file.path()).unwrap();
        let embed = statuses.iter().find(|s| s.command == "embed").unwrap();
        assert_eq!(embed.last_run_at, None);
        assert_eq!(embed.status, None);
        assert!(!embed.currently_running);
    }

    #[test]
    fn read_all_reports_success_after_a_completed_run() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        track(&conn, db_file.path(), "embed", || Ok(())).unwrap();

        let statuses = read_all(&conn, db_file.path()).unwrap();
        let embed = statuses.iter().find(|s| s.command == "embed").unwrap();
        assert_eq!(embed.status.as_deref(), Some("success"));
        assert!(embed.last_run_at.is_some());
        assert!(!embed.currently_running);
    }

    #[test]
    fn read_all_reports_currently_running_while_locked() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        start_run(&conn, "faces").unwrap();
        let _held = acquire_lock(db_file.path(), "faces").unwrap();

        let statuses = read_all(&conn, db_file.path()).unwrap();
        let faces = statuses.iter().find(|s| s.command == "faces").unwrap();
        assert_eq!(faces.status.as_deref(), Some("running"));
        assert!(faces.currently_running);
    }

    #[test]
    fn read_all_reports_crashed_when_running_but_not_locked() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        start_run(&conn, "faces").unwrap();

        let statuses = read_all(&conn, db_file.path()).unwrap();
        let faces = statuses.iter().find(|s| s.command == "faces").unwrap();
        assert_eq!(faces.status.as_deref(), Some("crashed"));
        assert!(!faces.currently_running);

        let stored_status: String = conn
            .query_row("SELECT status FROM pipeline_runs WHERE command = 'faces'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored_status, "running", "read_all must not write back the crashed label");
    }

    #[test]
    fn install_sigint_handler_does_not_error_when_called_once() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        // Only one handler can be installed per process for the life of the
        // test binary; this just confirms the call itself succeeds.
        // (ctrlc::set_handler errors if called twice in the same process,
        // so this is deliberately the only test that calls it in this suite.)
        let result = install_sigint_handler(db_file.path(), "scan");
        assert!(result.is_ok());
    }
}
