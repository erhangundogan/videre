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

/// The highest `faces.id` ever handed out. `faces.id` has no AUTOINCREMENT,
/// so SQLite would reuse a deleted top id, and learning events and questions
/// still name deleted faces by id; new faces are numbered above this instead.
pub const FACE_ID_HIGH_WATER: &str = "face_id_high_water";

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

/// The stored value, for callers inside a rusqlite-typed transaction.
pub fn value(conn: &Connection, key: &str) -> rusqlite::Result<Option<i64>> {
    ensure_table(conn)?;
    conn.query_row(
        "SELECT value FROM library_state WHERE key = ?1",
        [key],
        |r| r.get(0),
    )
    .optional()
}

/// The stored text, or `None` when there is none or no table yet: a pure read
/// that never creates the table, for checks on a read path.
pub fn peek_string(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    if !crate::db::table_exists(conn, "library_state")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT value FROM library_state WHERE key = ?1",
        [key],
        |r| r.get(0),
    )
    .optional()
}

/// Raise the stored value to at least `value`; never lowers it.
pub fn raise_to(conn: &Connection, key: &str, value: i64) -> rusqlite::Result<()> {
    ensure_table(conn)?;
    conn.execute(
        "INSERT INTO library_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = MAX(value, excluded.value)",
        rusqlite::params![key, value],
    )?;
    Ok(())
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

/// The string-valued form, for non-numeric state such as the location
/// fingerprint. SQLite's dynamic typing stores the text in the same `value`
/// column the integer accessors use; a key is only ever read back through the
/// accessor that wrote it, so the integer watermark and the text keys never
/// collide.
pub fn get_string(conn: &Connection, key: &str) -> Result<Option<String>> {
    ensure_table(conn)?;
    let v = conn
        .query_row(
            "SELECT value FROM library_state WHERE key = ?1",
            [key],
            |r| {
                // The `value` column has INTEGER affinity, so a numeric string such
                // as a radius ("15") is coerced to an integer on write. Read it back
                // through a dynamic value so both a text fingerprint and a coerced
                // number round-trip as strings.
                use rusqlite::types::ValueRef;
                Ok(match r.get_ref(0)? {
                    ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    ValueRef::Integer(i) => i.to_string(),
                    ValueRef::Real(f) => f.to_string(),
                    ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                    ValueRef::Null => String::new(),
                })
            },
        )
        .optional()?;
    Ok(v)
}

/// Store a string `value` under `key` (see [`get_string`]).
pub fn set_string(conn: &Connection, key: &str, value: &str) -> Result<()> {
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
    fn set_string_get_string_round_trips_text_and_coerced_numbers() {
        let conn = Connection::open_in_memory().unwrap();
        set_string(&conn, "fp", "v1:2:2:101.37:15.755").unwrap();
        assert_eq!(
            get_string(&conn, "fp").unwrap().as_deref(),
            Some("v1:2:2:101.37:15.755")
        );
        // A numeric string is coerced to an integer by the INTEGER-affinity
        // column; get_string still reads back the same digits.
        set_string(&conn, "radius", "15").unwrap();
        assert_eq!(get_string(&conn, "radius").unwrap().as_deref(), Some("15"));
        assert_eq!(get_string(&conn, "missing").unwrap(), None);
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
