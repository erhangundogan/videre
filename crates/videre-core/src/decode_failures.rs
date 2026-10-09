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
//! [`FAILURE_THRESHOLD`], and any success [`clear`]s the row.
//!
//! What a skip means depends on why the decode failed, which each row keeps as
//! its `kind` (the [`ErrorKind`](crate::error_kind::ErrorKind) code). A file
//! that is not a decodable image (`decode_failed`) is skipped for good: it
//! cannot change without its content, and so its hash, changing too. Anything
//! else, a QuickLook timeout above all, is skipped only for [`RETRY_AFTER`]
//! after its last failure and then tried again by any run, watch included.
//! Skipping every failure for good left a photo with no faces after one busy
//! evening, with nothing that would ever try it again.

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

/// How long a failure that may not repeat (a timeout, an unreadable drive, or
/// an unknown cause) keeps its file skipped before it is tried again: long
/// enough that watch does not pay a timeout every cycle, short enough that a
/// file caught in one busy evening is processed the next day.
pub const RETRY_AFTER: &str = "-1 day";

/// Create the table if it is absent. Idempotent; safe on every open.
pub fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS decode_failures (
            hash        TEXT NOT NULL,
            stage       TEXT NOT NULL,
            error       TEXT NOT NULL,
            fail_count  INTEGER NOT NULL DEFAULT 1,
            last_failed_at TEXT DEFAULT (datetime('now')),
            kind        TEXT,
            PRIMARY KEY (hash, stage)
        );",
    )?;
    // Added in place to a table from before the kind was kept; its rows have
    // none, so they count as a failure that may not repeat.
    let has_kind = conn
        .prepare("SELECT 1 FROM pragma_table_info('decode_failures') WHERE name = 'kind'")?
        .exists([])?;
    if !has_kind {
        conn.execute_batch("ALTER TABLE decode_failures ADD COLUMN kind TEXT")?;
    }
    Ok(())
}

/// Record one decode failure for `(hash, stage)`: insert the row at count 1, or
/// bump an existing row's count and store the latest error. Idempotent in shape,
/// not in effect: each call is one more strike.
/// `kind` is why it failed, as tagged where the cause was known; `None` when it
/// was not, which is treated as a failure that may not repeat.
pub fn record(
    conn: &Connection,
    hash: &str,
    stage: &str,
    error: &str,
    kind: Option<crate::error_kind::ErrorKind>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO decode_failures (hash, stage, error, fail_count, last_failed_at, kind)
             VALUES (?1, ?2, ?3, 1, datetime('now'), ?4)
         ON CONFLICT(hash, stage) DO UPDATE SET
             fail_count = fail_count + 1,
             error = excluded.error,
             last_failed_at = excluded.last_failed_at,
             kind = excluded.kind",
        rusqlite::params![hash, stage, error, kind.map(|k| k.code())],
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
    // A table from before the kind was kept has no such column yet.
    ensure_table(conn)?;
    let mut stmt = conn.prepare(
        "SELECT hash FROM decode_failures
         WHERE stage = ?1 AND fail_count >= ?2
           AND (kind = ?3 OR last_failed_at > datetime('now', ?4))",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            stage,
            min_count,
            crate::error_kind::ErrorKind::DecodeFailed.code(),
            RETRY_AFTER
        ],
        |r| r.get::<_, String>(0),
    )?;
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
        record(&conn, "h1", STAGE_EMBED, "boom", None).unwrap();
        assert_eq!(fail_count(&conn, "h1", STAGE_EMBED).unwrap(), 1);
    }

    #[test]
    fn record_increments_on_repeat_and_keeps_latest_error() {
        let conn = db();
        record(&conn, "h1", STAGE_EMBED, "first", None).unwrap();
        record(&conn, "h1", STAGE_EMBED, "second", None).unwrap();
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
        record(&conn, "once", STAGE_EMBED, "e", None).unwrap();
        record(&conn, "twice", STAGE_EMBED, "e", None).unwrap();
        record(&conn, "twice", STAGE_EMBED, "e", None).unwrap();
        let set = failed_hashes(&conn, STAGE_EMBED, FAILURE_THRESHOLD).unwrap();
        assert!(
            !set.contains("once"),
            "a single failure must not be skipped (could be transient)"
        );
        assert!(set.contains("twice"), "two failures crosses the threshold");
    }

    /// Moves a row's last failure `days` into the past.
    fn age(conn: &Connection, hash: &str, days: i64) {
        conn.execute(
            "UPDATE decode_failures SET last_failed_at = datetime('now', ?2) WHERE hash = ?1",
            rusqlite::params![hash, format!("-{days} days")],
        )
        .unwrap();
    }

    #[test]
    fn a_broken_file_stays_skipped_and_a_timeout_is_retried_a_day_later() {
        use crate::error_kind::ErrorKind;
        let conn = db();
        for _ in 0..FAILURE_THRESHOLD {
            record(
                &conn,
                "bozuk",
                STAGE_FACES,
                "not an image",
                Some(ErrorKind::DecodeFailed),
            )
            .unwrap();
            record(
                &conn,
                "yavaş",
                STAGE_FACES,
                "qlmanage timed out",
                Some(ErrorKind::SourceUnavailable),
            )
            .unwrap();
        }
        let now = failed_hashes(&conn, STAGE_FACES, FAILURE_THRESHOLD).unwrap();
        assert!(
            now.contains("bozuk") && now.contains("yavaş"),
            "both rest for now"
        );

        age(&conn, "bozuk", 2);
        age(&conn, "yavaş", 2);
        let later = failed_hashes(&conn, STAGE_FACES, FAILURE_THRESHOLD).unwrap();
        assert!(
            later.contains("bozuk"),
            "a broken file cannot heal by waiting"
        );
        assert!(!later.contains("yavaş"), "a timeout is tried again");
    }

    #[test]
    fn a_failure_of_unknown_kind_is_retried_like_a_timeout() {
        // Rows recorded before the kind was kept, and failures whose cause was
        // not known, are given another chance rather than condemned.
        let conn = db();
        record(&conn, "eski", STAGE_EMBED, "e", None).unwrap();
        record(&conn, "eski", STAGE_EMBED, "e", None).unwrap();
        age(&conn, "eski", 2);
        assert!(failed_hashes(&conn, STAGE_EMBED, FAILURE_THRESHOLD)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_table_from_before_the_kind_gains_it_in_place() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE decode_failures (hash TEXT NOT NULL, stage TEXT NOT NULL,
                 error TEXT NOT NULL, fail_count INTEGER NOT NULL DEFAULT 1,
                 last_failed_at TEXT DEFAULT (datetime('now')), PRIMARY KEY (hash, stage));
             INSERT INTO decode_failures (hash, stage, error, fail_count)
                 VALUES ('eski', 'faces', 'qlmanage timed out', 2);",
        )
        .unwrap();
        ensure_table(&conn).unwrap();
        age(&conn, "eski", 2);
        assert!(failed_hashes(&conn, STAGE_FACES, FAILURE_THRESHOLD)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn clear_removes_a_hash_so_it_is_tried_again() {
        let conn = db();
        record(&conn, "h1", STAGE_EMBED, "e", None).unwrap();
        record(&conn, "h1", STAGE_EMBED, "e", None).unwrap();
        clear(&conn, "h1", STAGE_EMBED).unwrap();
        assert_eq!(fail_count(&conn, "h1", STAGE_EMBED).unwrap(), 0);
        assert!(failed_hashes(&conn, STAGE_EMBED, FAILURE_THRESHOLD)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn clear_stage_removes_only_that_stage() {
        let conn = db();
        record(&conn, "h1", STAGE_EMBED, "e", None).unwrap();
        record(&conn, "h2", STAGE_EMBED, "e", None).unwrap();
        record(&conn, "h1", STAGE_FACES, "e", None).unwrap();
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
        record(&conn, "h1", STAGE_EMBED, "e", None).unwrap();
        record(&conn, "h1", STAGE_EMBED, "e", None).unwrap();
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
