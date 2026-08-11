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

use chrono::NaiveDate;

const DATE_FORMS: &str = "expected YYYY, YYYY-MM, YYYY-MM-DD, or YYYY-MM-DDTHH:MM:SS";

fn start_of(y: i32, m: u32, d: u32) -> Result<String> {
    NaiveDate::from_ymd_opt(y, m, d)
        .map(|x| format!("{}T00:00:00", x.format("%Y-%m-%d")))
        .ok_or_else(|| anyhow::anyhow!("invalid date {y:04}-{m:02}-{d:02}; {DATE_FORMS}"))
}

/// Expands `--date` shorthand into a half-open `[start, end)` range.
pub fn expand_date(spec: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = spec.split('-').collect();
    let bad = || anyhow::anyhow!("cannot parse date {spec:?}; {DATE_FORMS}");
    match parts.as_slice() {
        [y] => {
            let y: i32 = y.parse().map_err(|_| bad())?;
            Ok((start_of(y, 1, 1)?, start_of(y + 1, 1, 1)?))
        }
        [y, m] => {
            let (y, m): (i32, u32) = (y.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
            let start = start_of(y, m, 1)?;
            let end = if m == 12 {
                start_of(y + 1, 1, 1)?
            } else {
                start_of(y, m + 1, 1)?
            };
            Ok((start, end))
        }
        [y, m, d] => {
            let (y, m, d): (i32, u32, u32) = (
                y.parse().map_err(|_| bad())?,
                m.parse().map_err(|_| bad())?,
                d.parse().map_err(|_| bad())?,
            );
            let day = NaiveDate::from_ymd_opt(y, m, d).ok_or_else(bad)?;
            let next = day.succ_opt().ok_or_else(bad)?;
            Ok((
                format!("{}T00:00:00", day.format("%Y-%m-%d")),
                format!("{}T00:00:00", next.format("%Y-%m-%d")),
            ))
        }
        _ => Err(bad()),
    }
}

/// Normalises an `--after`/`--before` bound to full ISO-8601.
pub fn normalise_bound(spec: &str) -> Result<String> {
    if spec.contains('T') {
        return Ok(spec.to_string());
    }
    let (start, _) = expand_date(spec)?;
    Ok(start)
}

/// Hashes with at least one confirmed face labelled `name`.
pub fn by_person(conn: &Connection, name: &str) -> Result<HashSet<String>> {
    let mut stmt =
        conn.prepare("SELECT DISTINCT hash FROM faces WHERE person_label = ?1 AND confirmed = 1")?;
    let rows = stmt.query_map(rusqlite::params![name], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<HashSet<String>>>()?)
}

/// Hashes classified as `category` by `model_id`.
pub fn by_category(conn: &Connection, model_id: &str, category: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT hash FROM classifications WHERE model_id = ?1 AND category = ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![model_id, category], |r| {
        r.get::<_, String>(0)
    })?;
    Ok(rows.collect::<rusqlite::Result<HashSet<String>>>()?)
}

use std::collections::HashMap;

/// Great-circle distance in km.
fn haversine(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0;
    let (dlat, dlon) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().asin()
}

/// Hashes within `radius_km` of a point, mapped to their distance in km.
///
/// Returns distances rather than a bare set because they are the ranker's
/// input for `SortField::Distance`.
pub fn by_location(
    conn: &Connection,
    lat: f64,
    lon: f64,
    radius_km: f64,
) -> Result<HashMap<String, f64>> {
    let mut stmt = conn.prepare(
        "SELECT hash, gps_lat, gps_lon FROM file_hashes
         WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, f64>(1)?,
            r.get::<_, f64>(2)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (hash, plat, plon) = row?;
        let d = haversine(lat, lon, plat, plon);
        if d <= radius_km {
            // Keep the nearest path for a hash that appears at several coords.
            out.entry(hash)
                .and_modify(|e: &mut f64| *e = e.min(d))
                .or_insert(d);
        }
    }
    Ok(out)
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

    #[test]
    fn date_shorthand_expands_to_half_open_ranges() {
        assert_eq!(
            expand_date("2025").unwrap(),
            ("2025-01-01T00:00:00".into(), "2026-01-01T00:00:00".into())
        );
        assert_eq!(
            expand_date("2025-05").unwrap(),
            ("2025-05-01T00:00:00".into(), "2025-06-01T00:00:00".into())
        );
        assert_eq!(
            expand_date("2025-12").unwrap(),
            ("2025-12-01T00:00:00".into(), "2026-01-01T00:00:00".into())
        );
        assert_eq!(
            expand_date("2025-05-14").unwrap(),
            ("2025-05-14T00:00:00".into(), "2025-05-15T00:00:00".into())
        );
    }

    #[test]
    fn date_shorthand_handles_month_and_year_rollover() {
        assert_eq!(expand_date("2024-02-29").unwrap().1, "2024-03-01T00:00:00");
        assert_eq!(expand_date("2025-12-31").unwrap().1, "2026-01-01T00:00:00");
    }

    #[test]
    fn normalise_bound_accepts_date_or_datetime() {
        assert_eq!(
            normalise_bound("2025-05-14").unwrap(),
            "2025-05-14T00:00:00"
        );
        assert_eq!(
            normalise_bound("2025-05-14T09:30:00").unwrap(),
            "2025-05-14T09:30:00"
        );
    }

    #[test]
    fn bad_dates_are_rejected_with_a_helpful_message() {
        for bad in ["", "May 2025", "2025-13", "2025-02-30", "20250514"] {
            let err = expand_date(bad).unwrap_err().to_string();
            assert!(
                err.contains("YYYY"),
                "error for {bad:?} should name the accepted forms, got: {err}"
            );
        }
    }

    fn db_with_faces_and_classes() -> Connection {
        let conn = db();
        conn.execute_batch(
            "CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
                person_label TEXT, confirmed INTEGER DEFAULT 0);
             CREATE TABLE classifications (model_id TEXT NOT NULL, hash TEXT NOT NULL,
                category TEXT NOT NULL, confidence REAL NOT NULL,
                classified_at TEXT NOT NULL, PRIMARY KEY (model_id, hash));",
        )
        .unwrap();
        conn
    }

    #[test]
    fn person_predicate_returns_confirmed_only() {
        let conn = db_with_faces_and_classes();
        conn.execute_batch(
            "INSERT INTO faces (hash, person_label, confirmed) VALUES
                ('h1','Alice',1), ('h2','Alice',0), ('h3','Bob',1);",
        )
        .unwrap();
        let got = by_person(&conn, "Alice").unwrap();
        assert!(got.contains("h1"));
        assert!(!got.contains("h2"), "unconfirmed faces must not match");
        assert!(!got.contains("h3"));
    }

    #[test]
    fn category_predicate_is_model_scoped() {
        let conn = db_with_faces_and_classes();
        conn.execute_batch(
            "INSERT INTO classifications VALUES
                ('m1','h1','screenshot',0.9,'now'),
                ('m2','h2','screenshot',0.9,'now');",
        )
        .unwrap();
        let got = by_category(&conn, "m1", "screenshot").unwrap();
        assert!(got.contains("h1"));
        assert!(!got.contains("h2"), "another model's rows must not leak in");
    }

    #[test]
    fn predicates_return_empty_not_error_when_nothing_matches() {
        let conn = db_with_faces_and_classes();
        assert!(by_person(&conn, "Nobody").unwrap().is_empty());
        assert!(by_category(&conn, "m1", "meme").unwrap().is_empty());
    }

    fn db_with_gps() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                modified_at TEXT, exif_date TEXT, gps_lat REAL, gps_lon REAL);
             INSERT INTO file_hashes VALUES
                ('/near.jpg','hn',100,'2025-01-01T00:00:00',NULL,52.5200,13.4050),
                ('/far.jpg','hf',100,'2025-01-01T00:00:00',NULL,48.8566,2.3522),
                ('/nogps.jpg','hx',100,'2025-01-01T00:00:00',NULL,NULL,NULL);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn location_predicate_returns_only_within_radius_with_distances() {
        let conn = db_with_gps();
        // Berlin centre, 10 km radius.
        let got = by_location(&conn, 52.5200, 13.4050, 10.0).unwrap();
        assert!(got.contains_key("hn"));
        assert!(
            !got.contains_key("hf"),
            "Paris is not within 10 km of Berlin"
        );
        assert!(!got.contains_key("hx"), "a file with no GPS cannot match");
        assert!(
            got["hn"] < 0.1,
            "distance to itself should be ~0, got {}",
            got["hn"]
        );
    }

    #[test]
    fn location_distance_is_roughly_correct_over_a_long_span() {
        let conn = db_with_gps();
        let got = by_location(&conn, 52.5200, 13.4050, 2000.0).unwrap();
        let d = got["hf"];
        assert!(
            (870.0..890.0).contains(&d),
            "Berlin to Paris is ~878 km, got {d}"
        );
    }
}
