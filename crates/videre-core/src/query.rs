//! Composable search predicates.
//!
//! Each predicate independently resolves to a set of content hashes;
//! `candidates` intersects them. Keeping them here rather than in
//! `person_search`/`classify`/`geocode` means the intersection logic lives in
//! one testable place and those modules keep their existing callers unchanged.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;

/// The date a file is considered to have: its EXIF capture date when that is
/// present and valid, otherwise the filesystem modification time.
///
/// The `0000%` guard matches `output.rs::best_date`: a camera with an unset
/// clock writes `0000-00-00T00:00:00`, which must fall back rather than being
/// treated as year zero.
pub const EFFECTIVE_DATE_SQL: &str = "CASE WHEN exif_date IS NOT NULL \
     AND exif_date NOT LIKE '0000%' THEN exif_date ELSE modified_at END";

/// Hashes whose effective date is in `[after, before)`.
///
/// `before` is exclusive so that adjacent ranges tile without both matching
/// the boundary instant.
pub fn by_date(
    conn: &Connection,
    after: Option<&str>,
    before: Option<&str>,
) -> Result<HashSet<String>> {
    let mut sql =
        format!("SELECT DISTINCT hash FROM file_hashes WHERE {EFFECTIVE_DATE_SQL} IS NOT NULL");
    let mut params: Vec<String> = Vec::new();
    if let Some(a) = after {
        sql.push_str(&format!(" AND {EFFECTIVE_DATE_SQL} >= ?"));
        params.push(a.to_string());
    }
    if let Some(b) = before {
        sql.push_str(&format!(" AND {EFFECTIVE_DATE_SQL} < ?"));
        params.push(b.to_string());
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |r| {
        r.get::<_, String>(0)
    })?;
    Ok(rows.collect::<rusqlite::Result<HashSet<String>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL,
                size_bytes INTEGER, modified_at TEXT, exif_date TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn add(conn: &Connection, path: &str, hash: &str, exif: Option<&str>, mtime: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, modified_at, exif_date)
             VALUES (?1, ?2, 100, ?3, ?4)",
            rusqlite::params![path, hash, mtime, exif],
        )
        .unwrap();
    }

    #[test]
    fn date_filter_matches_on_exif_when_present() {
        let conn = db();
        add(
            &conn,
            "/a.jpg",
            "h1",
            Some("2025-05-14T10:00:00"),
            "2026-01-01T00:00:00",
        );
        let got = by_date(
            &conn,
            Some("2025-05-01T00:00:00"),
            Some("2025-06-01T00:00:00"),
        )
        .unwrap();
        assert!(got.contains("h1"), "exif_date must win over modified_at");
    }

    #[test]
    fn date_filter_falls_back_to_modified_at() {
        let conn = db();
        add(&conn, "/b.png", "h2", None, "2025-05-14T10:00:00");
        let got = by_date(
            &conn,
            Some("2025-05-01T00:00:00"),
            Some("2025-06-01T00:00:00"),
        )
        .unwrap();
        assert!(
            got.contains("h2"),
            "a file with no EXIF must match on modified_at"
        );
    }

    #[test]
    fn date_filter_ignores_zero_exif_dates() {
        let conn = db();
        add(
            &conn,
            "/c.jpg",
            "h3",
            Some("0000-00-00T00:00:00"),
            "2025-05-14T10:00:00",
        );
        let got = by_date(
            &conn,
            Some("2025-05-01T00:00:00"),
            Some("2025-06-01T00:00:00"),
        )
        .unwrap();
        assert!(
            got.contains("h3"),
            "an unset camera clock must fall back, not match year 0"
        );
    }

    #[test]
    fn before_is_exclusive_so_ranges_tile() {
        let conn = db();
        add(
            &conn,
            "/d.jpg",
            "h4",
            Some("2025-06-01T00:00:00"),
            "2025-06-01T00:00:00",
        );
        let may = by_date(
            &conn,
            Some("2025-05-01T00:00:00"),
            Some("2025-06-01T00:00:00"),
        )
        .unwrap();
        let jun = by_date(
            &conn,
            Some("2025-06-01T00:00:00"),
            Some("2025-07-01T00:00:00"),
        )
        .unwrap();
        assert!(
            !may.contains("h4"),
            "the boundary instant belongs to June only"
        );
        assert!(jun.contains("h4"));
    }

    #[test]
    fn open_ended_ranges_work() {
        let conn = db();
        add(
            &conn,
            "/e.jpg",
            "h5",
            Some("2025-05-14T10:00:00"),
            "2025-05-14T10:00:00",
        );
        assert!(by_date(&conn, Some("2025-01-01T00:00:00"), None)
            .unwrap()
            .contains("h5"));
        assert!(by_date(&conn, None, Some("2026-01-01T00:00:00"))
            .unwrap()
            .contains("h5"));
    }
}
