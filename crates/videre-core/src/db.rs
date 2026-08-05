use rusqlite::Connection;
use std::path::Path;

/// Opens a SQLite connection and switches it to WAL journal mode, allows
/// one writer plus many concurrent readers without "database is locked"
/// errors, which matters once videre watch (writing in the background) and a
/// running videre report --show-faces server (reading/writing) hold separate
/// connections to the same file at the same time. WAL mode persists in the
/// database file itself once set, so this is idempotent, safe to call on
/// every connection open, not just the first.
pub fn open_wal(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(conn)
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn table_exists_true_for_existing_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE widgets (id INTEGER);").unwrap();
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
}
