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

/// The six commands tracked in this pass. `videre watch` itself is
/// deliberately not in this list - it has no "finished" moment during normal
/// operation, so it gets its own liveness lock (see `watch_lock_path`) but no
/// `pipeline_runs` row.
pub const TRACKED_COMMANDS: [&str; 6] =
    ["scan", "faces", "embed", "classify", "dedupe", "fix-dates"];

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
}
