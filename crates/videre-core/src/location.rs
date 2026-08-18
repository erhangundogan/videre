use reverse_geocoder::ReverseGeocoder;
use rusqlite::Connection;
use std::sync::OnceLock;

/// Our own GeoNames extract, carrying real UTF-8 place names.
///
/// The crate's bundled dataset is ASCII-only: it ships GeoNames' `asciiname`
/// column, which stores `Üsküdar` as `UEskuedar` and `Malmö` as `Malmoe`. That
/// is GeoNames' own data rather than a defect in the crate, and it cannot be
/// undone in code - `UE` -> `Ü` is ambiguous and would corrupt names that are
/// already correct - so the only fix is to supply different data.
///
/// Built from `cities1000` using column 2 (`name`) instead of column 3
/// (`asciiname`): 170,761 rows, of which **21% contain a non-ASCII character**.
/// This is not a Turkish edge case, it is a fifth of the planet.
///
/// `admin1` and `admin2` are deliberately empty. `location_name` formats only
/// `name` and `cc`, nothing in videre reads the other two, and dropping them
/// makes this smaller than the file it replaces: 5.7MB against 7.5MB.
const CITIES_CSV: &str = include_str!("../data/cities.csv");

/// Idempotent migration: adds `file_hashes.location_name` if it doesn't
/// already exist. Mirrors the `ALTER TABLE faces ADD COLUMN is_primary`
/// pattern in face_db.rs, errors (column already exists) are ignored.
pub fn ensure_location_column(conn: &Connection) {
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN location_name TEXT");
}

/// Process-wide, lazily-built reverse geocoder. Building one parses the whole
/// 170,761-row CSV and constructs a KD-tree, which is expensive to redo per
/// lookup. Built once per process and reused by every caller (both the
/// single-call `location_name` below and any bulk caller using `geocoder()`
/// directly).
static GEOCODER: OnceLock<ReverseGeocoder> = OnceLock::new();

/// Returns the process-wide reverse geocoder, building it on first access.
/// Callers doing many lookups in a loop (e.g. `videre watch`'s location stage)
/// should call this once and reuse the reference rather than calling
/// `location_name` per coordinate, since `location_name` itself goes through
/// this same cached instance but still incurs a function-call/lookup
/// pattern per site, using `geocoder()` directly makes the "build once"
/// intent explicit at bulk call sites.
pub fn geocoder() -> &'static ReverseGeocoder {
    GEOCODER.get_or_init(|| {
        // `ReverseGeocoder` can only be built from its own embedded CSV or from
        // a path, so the bundled UTF-8 data has to reach the disk once. Written
        // under `<videre home>/geo/`, reused forever after.
        match materialize_cities_csv().and_then(|p| Ok(ReverseGeocoder::from_path(p)?)) {
            Ok(g) => g,
            Err(e) => {
                // Falling back keeps place names working (mangled, as before)
                // rather than failing a scan outright, but it is worth saying
                // out loud: silently reverting to ASCII names is exactly the
                // bug this replaced.
                eprintln!(
                    "warning: could not load videre's place-name data ({e}); \
                     falling back to the ASCII-only built-in, so names like \
                     Üsküdar will appear as UEskuedar"
                );
                ReverseGeocoder::new()
            }
        }
    })
}

/// Writes the embedded city data to `<videre home>/geo/cities-<len>.csv` if it
/// is not already there, and returns the path.
///
/// Written to a temporary file and renamed, because `videre watch` and a manual
/// command run concurrently by design: two processes materializing at once must
/// not let one read a half-written file. Rename is atomic within a directory.
///
/// The byte length is in the filename so a future data update lands beside the
/// old file rather than needing a staleness check.
fn materialize_cities_csv() -> anyhow::Result<std::path::PathBuf> {
    use std::io::Write;

    let dir = crate::home::videre_home()?.join("geo");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("cities-{}.csv", CITIES_CSV.len()));
    if path.exists() {
        return Ok(path);
    }

    let tmp = dir.join(format!(
        "cities-{}.{}.tmp",
        CITIES_CSV.len(),
        std::process::id()
    ));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(CITIES_CSV.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Reverse-geocodes (lat, lon) to a human-readable "City, Country" string
/// using an offline GeoNames-derived dataset (no network calls). Always
/// returns Some(..) since the bundled dataset covers the whole globe with a
/// nearest-city match, there's always some nearest record.
///
/// Uses a process-wide cached `ReverseGeocoder` (see `geocoder()`), so
/// repeated calls, whether from a single on-demand lookup or a loop over
/// many coordinates, only pay the dataset-parsing/KD-tree-build cost once.
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
        conn.execute_batch("CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL);")
            .unwrap();
        crate::db::ensure_file_hashes_columns(&conn);
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
        assert!(
            name.contains("FR"),
            "expected France country code, got: {name}"
        );
    }

    #[test]
    fn place_names_keep_their_diacritics() {
        // The bug this data replaced: the crate's own dataset is GeoNames'
        // `asciiname` column, which renders Üsküdar as "UEskuedar" and Malmö as
        // "Malmoe". Asserting the mangled forms are *absent* matters as much as
        // the correct ones being present, because the fallback path in
        // `geocoder()` would still return a plausible-looking name.
        for (lat, lon, want, mangled) in [
            (41.02274, 29.01366, "Üsküdar", "UEskuedar"),
            (55.60587, 13.00073, "Malmö", "Malmoe"),
        ] {
            let got = location_name(lat, lon).unwrap();
            assert!(got.starts_with(want), "expected {want}, got {got}");
            assert!(!got.contains(mangled), "still ASCII-mangled: {got}");
        }
    }

    #[test]
    fn the_bundled_data_is_actually_unicode() {
        // Guards the data file itself rather than a lookup: regenerating it
        // from the wrong GeoNames column would leave every test above passing
        // only if the specific cities happened to survive.
        let non_ascii = CITIES_CSV
            .lines()
            .skip(1)
            .filter(|l| l.chars().any(|c| !c.is_ascii()))
            .count();
        assert!(
            non_ascii > 30_000,
            "only {non_ascii} rows carry non-ASCII names; the data was probably \
             built from GeoNames' asciiname column again"
        );
    }
}
