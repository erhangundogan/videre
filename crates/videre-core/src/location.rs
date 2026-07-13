use reverse_geocoder::ReverseGeocoder;
use rusqlite::Connection;
use std::sync::OnceLock;

/// Idempotent migration: adds `file_hashes.location_name` if it doesn't
/// already exist. Mirrors the `ALTER TABLE faces ADD COLUMN is_primary`
/// pattern in face_db.rs - errors (column already exists) are ignored.
pub fn ensure_location_column(conn: &Connection) {
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN location_name TEXT");
}

/// Process-wide, lazily-built reverse geocoder. `ReverseGeocoder::new()`
/// parses an embedded ~144,564-row / 7.8MB `cities.csv` and builds a KD-tree
/// from scratch, which is expensive to redo per lookup. Built once per
/// process and reused by every caller (both the single-call `location_name`
/// below and any bulk caller using `geocoder()` directly).
static GEOCODER: OnceLock<ReverseGeocoder> = OnceLock::new();

/// Returns the process-wide reverse geocoder, building it on first access.
/// Callers doing many lookups in a loop (e.g. `dupe-watch`'s location stage)
/// should call this once and reuse the reference rather than calling
/// `location_name` per coordinate, since `location_name` itself goes through
/// this same cached instance but still incurs a function-call/lookup
/// pattern per site - using `geocoder()` directly makes the "build once"
/// intent explicit at bulk call sites.
pub fn geocoder() -> &'static ReverseGeocoder {
    GEOCODER.get_or_init(ReverseGeocoder::new)
}

/// Reverse-geocodes (lat, lon) to a human-readable "City, Country" string
/// using an offline GeoNames-derived dataset (no network calls). Always
/// returns Some(..) since the bundled dataset covers the whole globe with a
/// nearest-city match - there's always some nearest record.
///
/// Uses a process-wide cached `ReverseGeocoder` (see `geocoder()`), so
/// repeated calls - whether from a single on-demand lookup or a loop over
/// many coordinates - only pay the dataset-parsing/KD-tree-build cost once.
pub fn location_name(lat: f64, lon: f64) -> Option<String> {
    let result = geocoder().search((lat, lon));
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
