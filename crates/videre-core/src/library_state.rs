//! Small key-value state for library-scoped bookkeeping that is neither a
//! pipeline run nor a setting: one row per key, integer values, table
//! created on first use. Used today for the face recluster watermark; kept
//! generic so the next piece of watch state does not grow its own table.

use anyhow::Result;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

/// Key holding the highest `faces.id` covered by the last completed global
/// face recluster (see `face_db::advance_recluster_watermark`).
pub const FACE_RECLUSTER_WATERMARK: &str = "face_recluster_watermark";

fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS library_state (
            key   TEXT PRIMARY KEY,
            value INTEGER NOT NULL
        );",
    )
}

/// The value stored under `key`, or `None` when it was never set.
pub fn get(conn: &Connection, key: &str) -> Result<Option<i64>> {
    ensure_table(conn)?;
    let v = conn
        .query_row(
            "SELECT value FROM library_state WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v)
}

/// Store `value` under `key`, creating the table on first use.
pub fn set(conn: &Connection, key: &str, value: i64) -> Result<()> {
    ensure_table(conn)?;
    conn.execute(
        "INSERT INTO library_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn get_returns_none_before_the_first_set() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(get(&conn, FACE_RECLUSTER_WATERMARK).unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips_and_overwrites() {
        let conn = Connection::open_in_memory().unwrap();
        set(&conn, FACE_RECLUSTER_WATERMARK, 7).unwrap();
        set(&conn, FACE_RECLUSTER_WATERMARK, 42).unwrap();
        assert_eq!(get(&conn, FACE_RECLUSTER_WATERMARK).unwrap(), Some(42));
    }

    #[test]
    fn keys_are_independent() {
        let conn = Connection::open_in_memory().unwrap();
        set(&conn, "a", 1).unwrap();
        set(&conn, "b", 2).unwrap();
        assert_eq!(get(&conn, "a").unwrap(), Some(1));
        assert_eq!(get(&conn, "b").unwrap(), Some(2));
    }
}
