use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::Path;

/// Canonical stored form of a filesystem mtime: `DateTime<Utc>` then rfc3339.
///
/// Identical to `videre`'s `hasher::system_time_to_iso`, so a value stored at
/// scan time and one computed freshly here compare as equal strings. That
/// string equality is exact rather than lucky: both sides go through the same
/// `DateTime<Utc>` then `to_rfc3339()`, so timezone and format never diverge
/// (the rule BUG:21 established for prune's sync).
pub fn mtime_iso(t: std::time::SystemTime) -> String {
    let dt: DateTime<Utc> = t.into();
    dt.to_rfc3339()
}

/// The stored facts change-detection needs about one row, so scan and watch can
/// decide whether to skip a file without opening it.
#[derive(Clone, Debug)]
pub struct RowSig {
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub has_mime: bool,
    pub has_phash: bool,
}

/// True when the stored row is current for the requested work, so the file can
/// be skipped without reading it: unchanged (same size and mtime) AND complete
/// (a known mime, and a phash when `--similar` needs one). A file the walk sees
/// but cannot stat, or a row with no stored mtime, is never current.
pub fn is_current(
    sig: &RowSig,
    cur_size: u64,
    cur_mtime: Option<&str>,
    want_similar: bool,
) -> bool {
    sig.has_mime
        && (!want_similar || sig.has_phash)
        && sig.size_bytes == cur_size
        && cur_mtime.is_some()
        && sig.modified_at.as_deref() == cur_mtime
}

/// Load every row's signature in one query, keyed by path. Empty when the
/// table does not exist, so a first scan (no table yet) skips nothing.
pub fn stored_signatures(conn: &Connection) -> rusqlite::Result<HashMap<String, RowSig>> {
    if !table_exists(conn, "file_hashes")? {
        return Ok(HashMap::new());
    }
    let mut stmt = conn.prepare(
        "SELECT path, size_bytes, modified_at, mime IS NOT NULL, phash IS NOT NULL \
         FROM file_hashes",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            RowSig {
                size_bytes: r.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                modified_at: r.get::<_, Option<String>>(2)?,
                has_mime: r.get::<_, bool>(3)?,
                has_phash: r.get::<_, bool>(4)?,
            },
        ))
    })?;
    rows.collect()
}

/// Opens a SQLite connection and switches it to WAL journal mode, allows
/// one writer plus many concurrent readers without "database is locked"
/// errors, which matters once videre watch (writing in the background) and a
/// running videre gallery server (reading/writing) hold separate
/// connections to the same file at the same time. WAL mode persists in the
/// database file itself once set, so this is idempotent, safe to call on
/// every connection open, not just the first.
pub fn open_wal(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    ensure_file_hashes_columns(&conn);
    crate::face_db::ensure_people_table(&conn);
    let _ = crate::marks::ensure_marks_table(&conn);
    Ok(conn)
}

/// Idempotent column migrations for `file_hashes`, run on every open.
///
/// It must be every open rather than only on write: readers query `mime`, and
/// a library scanned before the column existed would otherwise fail with
/// "no such column" on `dedupe`, `stats`, `search`, and the rest, until the
/// user happened to re-scan. Errors (column already exists, or no table yet in
/// a brand-new database) are ignored, the same pattern
/// `location::ensure_location_column` and `face_db`'s `is_primary` use.
///
/// `open_wal` is only ever used for the main library database; per-model
/// embedding databases go through `Connection::open` in `embeddings_db`.
pub fn ensure_file_hashes_columns(conn: &Connection) {
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN mime TEXT;");
    // Video metadata, 0.14.0. NULL for images, and NULL for every row scanned
    // before that release: `--retry-incomplete` keys on `mime IS NULL` and does
    // not catch "scanned before these existed", so only a re-scan fills them.
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN duration_secs REAL;");
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN codec TEXT;");
}

/// Whether `name` exists as a table in `conn`, used by every reader that
/// queries an optional table (`faces`, `embeddings`, `classifications`) added
/// after `file_hashes` and not guaranteed present in an older db.
pub fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Paths already recorded with a known type, for `videre scan
/// --retry-incomplete`.
///
/// One query rather than a lookup per path: a library has tens of thousands of
/// rows, and 70,000 point queries would cost more than the file reads this
/// exists to avoid. Measured for scale: a full scan of a 70,601-file library
/// reads roughly 460 GB in 9m50s, while walking it takes 1.8s.
///
/// Rows carrying `mime_probe::UNKNOWN_MIME` count as known: the file was read
/// and checked, and re-reading it would never produce a different answer.
/// Only NULL, meaning never scanned, is left out.
///
/// A missing table is an empty set, not an error, so scanning into a fresh
/// database degrades to a normal full scan.
pub fn paths_with_known_mime(
    conn: &Connection,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    if !table_exists(conn, "file_hashes")? {
        return Ok(std::collections::HashSet::new());
    }
    let mut stmt = conn.prepare("SELECT path FROM file_hashes WHERE mime IS NOT NULL")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn is_current_only_when_unchanged_and_complete() {
        let base = RowSig {
            size_bytes: 10,
            modified_at: Some("2024-01-01T00:00:00+00:00".to_string()),
            has_mime: true,
            has_phash: false,
        };
        let m = base.modified_at.as_deref();
        // unchanged + known mime, not asking for similar -> current (skip)
        assert!(is_current(&base, 10, m, false));
        // size differs -> not current
        assert!(!is_current(&base, 11, m, false));
        // mtime differs -> not current
        assert!(!is_current(
            &base,
            10,
            Some("2024-02-02T00:00:00+00:00"),
            false
        ));
        // missing mtime on disk -> not current
        assert!(!is_current(&base, 10, None, false));
        // mime unknown -> not current even if unchanged (backfill mime)
        let no_mime = RowSig {
            has_mime: false,
            ..base.clone()
        };
        assert!(!is_current(&no_mime, 10, m, false));
        // --similar with no phash -> not current (backfill phash)
        assert!(!is_current(&base, 10, m, true));
        // --similar with phash present -> current
        let with_phash = RowSig {
            has_phash: true,
            ..base.clone()
        };
        assert!(is_current(&with_phash, 10, m, true));
    }

    #[test]
    fn stored_signatures_reads_size_mtime_and_completeness() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT, size_bytes INTEGER,
                modified_at TEXT, mime TEXT, phash INTEGER);
             INSERT INTO file_hashes VALUES ('a.jpg','h',10,'2024-01-01T00:00:00+00:00','image/jpeg',NULL);
             INSERT INTO file_hashes VALUES ('b.dng','h2',20,'2024-01-02T00:00:00+00:00',NULL,NULL);",
        )
        .unwrap();
        let sigs = stored_signatures(&conn).unwrap();
        assert_eq!(sigs["a.jpg"].size_bytes, 10);
        assert!(sigs["a.jpg"].has_mime && !sigs["a.jpg"].has_phash);
        assert!(!sigs["b.dng"].has_mime);
    }

    #[test]
    fn stored_signatures_empty_without_the_table() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(stored_signatures(&conn).unwrap().is_empty());
    }

    #[test]
    fn table_exists_true_for_existing_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE widgets (id INTEGER);")
            .unwrap();
        assert!(table_exists(&conn, "widgets").unwrap());
    }

    #[test]
    fn table_exists_false_for_missing_table() {
        let conn = Connection::open_in_memory().unwrap();
        assert!(!table_exists(&conn, "widgets").unwrap());
    }

    #[test]
    fn open_wal_sets_journal_mode() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = open_wal(&db_path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn open_wal_is_idempotent_across_repeated_opens() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        open_wal(&db_path).unwrap();
        // Second open on the same file must not error, WAL mode already
        // persisted from the first open.
        let conn = open_wal(&db_path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    fn db_with_rows(rows: &[(&str, Option<&str>)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT, ext TEXT, mime TEXT);",
        )
        .unwrap();
        for (path, mime) in rows {
            conn.execute(
                "INSERT INTO file_hashes (path, hash, ext, mime) VALUES (?1, 'h', 'jpg', ?2)",
                rusqlite::params![path, mime],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn paths_with_known_mime_excludes_null_rows() {
        let conn = db_with_rows(&[("/done.jpg", Some("image/jpeg")), ("/todo.jpg", None)]);
        let set = paths_with_known_mime(&conn).unwrap();
        assert!(set.contains("/done.jpg"));
        assert!(
            !set.contains("/todo.jpg"),
            "NULL means never scanned, so it must be retried"
        );
    }

    #[test]
    fn paths_with_known_mime_includes_the_sentinel() {
        // The whole point: a file checked and found unidentifiable is done,
        // not pending, so the retry set shrinks to empty instead of looping.
        let conn = db_with_rows(&[("/weird.jpg", Some(crate::mime_probe::UNKNOWN_MIME))]);
        assert!(paths_with_known_mime(&conn).unwrap().contains("/weird.jpg"));
    }

    #[test]
    fn paths_with_known_mime_on_a_table_that_does_not_exist_is_empty_not_an_error() {
        // Scanning into a fresh database: everything is incomplete.
        let conn = Connection::open_in_memory().unwrap();
        assert!(paths_with_known_mime(&conn).unwrap().is_empty());
    }
}
