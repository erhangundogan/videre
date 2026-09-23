//! Records files that a decode stage tried and could not turn into pixels, so a
//! guaranteed failure is not re-attempted on every run.
//!
//! `videre embed` and `videre faces` both decode a file before they can do their
//! work, and a file that hangs QuickLook or is otherwise undecodable produces
//! nothing, is not recorded anywhere, and therefore stays pending forever: every
//! later run pays the same multi-second timeout for the same guaranteed failure.
//! `scan` already has the unknown-mime sentinel for "could not identify"; this is
//! the equivalent for "identified as decodable, but the decode failed".
//!
//! The skip is deliberately not one-strike. A QuickLook timeout can be transient
//! (two videre commands contending for the one QuickLook agent push each other
//! past the timeout, and the file converts fine uncontended next run), so a
//! single failure must not condemn a file. A row records how many times a stage
//! has failed on a hash; a consumer skips only once the count reaches
//! [`FAILURE_THRESHOLD`], and any success [`clear`]s the row, so a
//! contention victim that later succeeds never sticks while a genuinely
//! undecodable file crosses the threshold and is skipped for good.

use rusqlite::Connection;
use std::collections::HashSet;

/// Stage keys. A failure is per (hash, stage): a file can be undecodable for one
/// stage's decode path and fine for another, and the retry hatches are per stage.
pub const STAGE_EMBED: &str = "embed";
pub const STAGE_FACES: &str = "faces";
pub const STAGE_THUMBNAIL: &str = "thumbnail";

/// How many times a stage must fail on a hash before it is skipped. Two, not one,
/// so a single transient timeout (QuickLook contention) does not condemn a file
/// that would decode fine uncontended.
pub const FAILURE_THRESHOLD: u32 = 2;

/// Create the table if it is absent. Idempotent; safe on every open.
pub fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS decode_failures (
            hash        TEXT NOT NULL,
            stage       TEXT NOT NULL,
            error       TEXT NOT NULL,
            fail_count  INTEGER NOT NULL DEFAULT 1,
            last_failed_at TEXT DEFAULT (datetime('now')),
            PRIMARY KEY (hash, stage)
        );",
    )
}

/// Record one decode failure for `(hash, stage)`: insert the row at count 1, or
/// bump an existing row's count and store the latest error. Idempotent in shape,
/// not in effect: each call is one more strike.
pub fn record(conn: &Connection, hash: &str, stage: &str, error: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO decode_failures (hash, stage, error, fail_count, last_failed_at)
             VALUES (?1, ?2, ?3, 1, datetime('now'))
         ON CONFLICT(hash, stage) DO UPDATE SET
             fail_count = fail_count + 1,
             error = excluded.error,
             last_failed_at = excluded.last_failed_at",
        rusqlite::params![hash, stage, error],
    )?;
    Ok(())
}

/// Drop the failure row for `(hash, stage)`, if any. Called after a successful
/// decode so a file that recovers (a transient timeout, or a videre fix) is
/// tried normally again.
pub fn clear(conn: &Connection, hash: &str, stage: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM decode_failures WHERE hash = ?1 AND stage = ?2",
        rusqlite::params![hash, stage],
    )?;
    Ok(())
}

/// Drop every failure row for a stage, returning how many were removed. The
/// retry hatch behind `--reprocess`: it makes a stage try every file again.
pub fn clear_stage(conn: &Connection, stage: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM decode_failures WHERE stage = ?1",
        rusqlite::params![stage],
    )
}

/// Hashes whose failure count for `stage` has reached `min_count`, i.e. the set a
/// consumer should skip. Returns an empty set if the table does not exist yet.
pub fn failed_hashes(
    conn: &Connection,
    stage: &str,
    min_count: u32,
) -> rusqlite::Result<HashSet<String>> {
    // A read path (the gallery, status) may open a library prepared before this
    // table existed; "nothing recorded" is the honest answer, not an error.
    if !crate::db::table_exists(conn, "decode_failures")? {
        return Ok(HashSet::new());
    }
    let mut stmt =
        conn.prepare("SELECT hash FROM decode_failures WHERE stage = ?1 AND fail_count >= ?2")?;
    let rows = stmt.query_map(rusqlite::params![stage, min_count], |r| {
        r.get::<_, String>(0)
    })?;
    let failed: HashSet<String> = rows.collect::<rusqlite::Result<_>>()?;
    if !failed.is_empty() {
        tracing::debug!(
            stage,
            skipped = failed.len(),
            "skipping file(s) that failed to decode {min_count} or more times"
        );
    }
    Ok(failed)
}

/// The current failure count for `(hash, stage)`, or 0 if there is no row.
pub fn fail_count(conn: &Connection, hash: &str, stage: &str) -> rusqlite::Result<u32> {
    conn.query_row(
        "SELECT fail_count FROM decode_failures WHERE hash = ?1 AND stage = ?2",
        rusqlite::params![hash, stage],
        |r| r.get(0),
    )
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(0),
        other => Err(other),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_table(&conn).unwrap();
        conn
    }

    #[test]
    fn record_creates_a_row_at_count_one() {
        let conn = db();
        record(&conn, "h1", STAGE_EMBED, "boom").unwrap();
        assert_eq!(fail_count(&conn, "h1", STAGE_EMBED).unwrap(), 1);
    }

    #[test]
    fn record_increments_on_repeat_and_keeps_latest_error() {
        let conn = db();
        record(&conn, "h1", STAGE_EMBED, "first").unwrap();
        record(&conn, "h1", STAGE_EMBED, "second").unwrap();
        assert_eq!(fail_count(&conn, "h1", STAGE_EMBED).unwrap(), 2);
        let err: String = conn
            .query_row(
                "SELECT error FROM decode_failures WHERE hash='h1' AND stage='embed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(err, "second", "the most recent error is kept");
    }

    #[test]
    fn failed_hashes_respects_the_threshold() {
        let conn = db();
        record(&conn, "once", STAGE_EMBED, "e").unwrap();
        record(&conn, "twice", STAGE_EMBED, "e").unwrap();
        record(&conn, "twice", STAGE_EMBED, "e").unwrap();
        let set = failed_hashes(&conn, STAGE_EMBED, FAILURE_THRESHOLD).unwrap();
        assert!(
            !set.contains("once"),
            "a single failure must not be skipped (could be transient)"
        );
        assert!(set.contains("twice"), "two failures crosses the threshold");
    }

    #[test]
    fn clear_removes_a_hash_so_it_is_tried_again() {
        let conn = db();
        record(&conn, "h1", STAGE_EMBED, "e").unwrap();
        record(&conn, "h1", STAGE_EMBED, "e").unwrap();
        clear(&conn, "h1", STAGE_EMBED).unwrap();
        assert_eq!(fail_count(&conn, "h1", STAGE_EMBED).unwrap(), 0);
        assert!(failed_hashes(&conn, STAGE_EMBED, FAILURE_THRESHOLD)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn clear_stage_removes_only_that_stage() {
        let conn = db();
        record(&conn, "h1", STAGE_EMBED, "e").unwrap();
        record(&conn, "h2", STAGE_EMBED, "e").unwrap();
        record(&conn, "h1", STAGE_FACES, "e").unwrap();
        let removed = clear_stage(&conn, STAGE_EMBED).unwrap();
        assert_eq!(removed, 2, "both embed rows removed");
        assert_eq!(
            fail_count(&conn, "h1", STAGE_FACES).unwrap(),
            1,
            "the faces row survives"
        );
    }

    #[test]
    fn stages_are_independent() {
        let conn = db();
        record(&conn, "h1", STAGE_EMBED, "e").unwrap();
        record(&conn, "h1", STAGE_EMBED, "e").unwrap();
        // Same hash, different stage: its own count, untouched by the embed one.
        assert_eq!(fail_count(&conn, "h1", STAGE_FACES).unwrap(), 0);
        assert!(!failed_hashes(&conn, STAGE_FACES, FAILURE_THRESHOLD)
            .unwrap()
            .contains("h1"));
    }

    #[test]
    fn failed_hashes_on_a_missing_table_is_empty_not_an_error() {
        // A read path may see a library opened before this table existed.
        let conn = Connection::open_in_memory().unwrap();
        assert!(failed_hashes(&conn, STAGE_EMBED, FAILURE_THRESHOLD)
            .unwrap()
            .is_empty());
    }
}
