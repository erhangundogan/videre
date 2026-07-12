use reverse_geocoder::ReverseGeocoder;
use rusqlite::Connection;

/// Idempotent migration: adds `file_hashes.location_name` if it doesn't
/// already exist. Mirrors the `ALTER TABLE faces ADD COLUMN is_primary`
/// pattern in face_db.rs - errors (column already exists) are ignored.
pub fn ensure_location_column(conn: &Connection) {
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN location_name TEXT");
}

/// Reverse-geocodes (lat, lon) to a human-readable "City, Country" string
/// using an offline GeoNames-derived dataset (no network calls). Always
/// returns Some(..) since the bundled dataset covers the whole globe with a
/// nearest-city match - there's always some nearest record.
pub fn location_name(lat: f64, lon: f64) -> Option<String> {
    let geocoder = ReverseGeocoder::new();
    let result = geocoder.search((lat, lon));
    let record = &result.record;
    if record.name.is_empty() {
        None
    } else {
        Some(format!("{}, {}", record.name, record.cc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_location_column_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL);",
        )
        .unwrap();
        ensure_location_column(&conn);
        ensure_location_column(&conn); // second call must not error
        conn.execute(
            "UPDATE file_hashes SET location_name = 'Paris, FR' WHERE path = 'x'",
            [],
        )
        .unwrap();
    }

    #[test]
    fn location_name_resolves_known_city() {
        // Coordinates for central Paris, France.
        let name = location_name(48.8566, 2.3522).unwrap();
        assert!(name.contains("FR"), "expected France country code, got: {name}");
    }
}
