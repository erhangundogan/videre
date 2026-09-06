use reverse_geocoder::ReverseGeocoder;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Our own GeoNames extract, carrying real UTF-8 place names.
///
/// The crate's bundled dataset is ASCII-only: it ships GeoNames' `asciiname`
/// column, which stores `Üsküdar` as `UEskuedar` and `Malmö` as `Malmoe`. That
/// is GeoNames' own data rather than a defect in the crate, and it cannot be
/// undone in code - `UE` -> `Ü` is ambiguous and would corrupt names that are
/// already correct - so the only fix is to supply different data.
///
/// Built from `cities1000` using column 2 (`name`) instead of column 3
/// (`asciiname`): 170,749 rows, of which **21% contain a non-ASCII character**.
/// This is not a Turkish edge case, it is a fifth of the planet.
///
/// Twelve rows are dropped: numbered administrative slices such as
/// `Sector 1, RO` (six of Bucharest's) and `Ward 3, MM`. GeoNames files them as
/// PPLX, "section of populated place", and their centroids can sit closer to a
/// photo than the city's own entry - a Bucharest cluster came back as
/// "Sector 1, RO" rather than "Bucharest, RO". Only these numbered ones go:
/// PPLX as a class holds 9,426 entries and most are names people use
/// (Rutherford, Werribee South), so excluding the class would lose more than it
/// fixed.
///
/// `admin1` and `admin2` are deliberately empty. `location_name` formats only
/// `name` and `cc`, nothing in videre reads the other two, and dropping them
/// makes this smaller than the file it replaces: 5.7MB against 7.5MB.
const CITIES_CSV: &str = include_str!("../data/cities.csv");

static EXPLICIT_GEOCODERS: OnceLock<Mutex<HashMap<std::path::PathBuf, Arc<ReverseGeocoder>>>> =
    OnceLock::new();

fn materialize_cities_csv_in(
    cache: &crate::library::CachePaths,
) -> anyhow::Result<std::path::PathBuf> {
    use std::io::Write;

    let digest = blake3::hash(CITIES_CSV.as_bytes()).to_hex();
    let path = cache.geo.join(digest.as_str()).join("cities.csv");
    if path.exists() {
        let bytes = std::fs::read(&path)
            .map_err(anyhow::Error::new)
            .map_err(|error| error.context(format!("read {}", path.display())))?;
        anyhow::ensure!(
            bytes == CITIES_CSV.as_bytes(),
            "cached place-name data at {} does not match its content hash",
            path.display()
        );
        return Ok(path);
    }
    crate::atomic_file::publish(&path, |file| {
        file.write_all(CITIES_CSV.as_bytes())?;
        Ok(())
    })?;
    Ok(path)
}

/// Return a reverse geocoder backed by the explicit shared cache.
///
/// Only successfully materialized and parsed datasets are cached. A failure
/// from one cache path therefore cannot become a fallback for another.
pub fn geocoder_in(cache: &crate::library::CachePaths) -> anyhow::Result<Arc<ReverseGeocoder>> {
    let path = materialize_cities_csv_in(cache)?;
    let geocoders = EXPLICIT_GEOCODERS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(geocoder) = geocoders.lock().unwrap().get(&path).cloned() {
        return Ok(geocoder);
    }
    let geocoder = Arc::new(
        ReverseGeocoder::from_path(&path)
            .map_err(anyhow::Error::new)
            .map_err(|error| error.context(format!("load {}", path.display())))?,
    );
    let mut locked = geocoders.lock().unwrap();
    Ok(locked
        .entry(path)
        .or_insert_with(|| geocoder.clone())
        .clone())
}

/// Resolve one coordinate using the selected explicit cache.
pub fn location_name_in(
    cache: &crate::library::CachePaths,
    lat: f64,
    lon: f64,
) -> anyhow::Result<Option<String>> {
    let geocoder = geocoder_in(cache)?;
    let result = geocoder.search((lat, lon));
    let record = &result.record;
    if record.name.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!("{}, {}", record.name, record.cc)))
    }
}

/// Idempotent migration: adds `file_hashes.location_name` if it doesn't
/// already exist. Mirrors the `ALTER TABLE faces ADD COLUMN is_primary`
/// pattern in face_db.rs, errors (column already exists) are ignored.
pub fn ensure_location_column(conn: &Connection) {
    let _ = conn.execute_batch("ALTER TABLE file_hashes ADD COLUMN location_name TEXT");
}

/// Process-wide, lazily-built reverse geocoder. Building one parses the whole
/// 170,749-row CSV and constructs a KD-tree, which is expensive to redo per
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

    fn cache(temp: &tempfile::TempDir) -> crate::library::CachePaths {
        crate::library::CachePaths {
            base: temp.path().to_path_buf(),
            thumbnails: temp.path().join("thumbnails"),
            geo: temp.path().join("geo"),
        }
    }

    #[test]
    fn explicit_dataset_is_content_addressed_and_exact() {
        let temp = tempfile::tempdir().unwrap();
        let cache = cache(&temp);
        let path = materialize_cities_csv_in(&cache).unwrap();
        let digest = blake3::hash(CITIES_CSV.as_bytes()).to_hex();
        assert_eq!(path, cache.geo.join(digest.as_str()).join("cities.csv"));
        assert_eq!(std::fs::read(path).unwrap(), CITIES_CSV.as_bytes());
    }

    #[test]
    fn explicit_geocoder_preserves_utf8_place_names() {
        let temp = tempfile::tempdir().unwrap();
        let cache = cache(&temp);
        let got = location_name_in(&cache, 41.02274, 29.01366)
            .unwrap()
            .unwrap();
        assert!(got.starts_with("Üsküdar"), "expected Üsküdar, got {got}");
        assert!(!got.contains("UEskuedar"), "still ASCII-mangled: {got}");
    }

    #[test]
    fn materialization_errors_do_not_fall_back_or_poison_another_cache() {
        let blocked = tempfile::tempdir().unwrap();
        std::fs::write(blocked.path().join("geo"), b"not a directory").unwrap();
        let blocked_cache = cache(&blocked);
        assert!(geocoder_in(&blocked_cache).is_err());

        let working = tempfile::tempdir().unwrap();
        let working_cache = cache(&working);
        let got = location_name_in(&working_cache, 55.60587, 13.00073)
            .unwrap()
            .unwrap();
        assert!(got.starts_with("Malmö"), "expected Malmö, got {got}");
    }

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
        let temp = tempfile::tempdir().unwrap();
        let name = location_name_in(&cache(&temp), 48.8566, 2.3522)
            .unwrap()
            .unwrap();
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
        let temp = tempfile::tempdir().unwrap();
        let cache = cache(&temp);
        for (lat, lon, want, mangled) in [
            (41.02274, 29.01366, "Üsküdar", "UEskuedar"),
            (55.60587, 13.00073, "Malmö", "Malmoe"),
        ] {
            let got = location_name_in(&cache, lat, lon).unwrap().unwrap();
            assert!(got.starts_with(want), "expected {want}, got {got}");
            assert!(!got.contains(mangled), "still ASCII-mangled: {got}");
        }
    }

    #[test]
    fn a_city_is_named_rather_than_one_of_its_administrative_slices() {
        // Reported against 0.15.5: a Bucharest cluster came back as
        // "Sector 1, RO". GeoNames lists Bucharest's six sectors as PPLX
        // entries, and a sector centroid can sit closer to the photos than the
        // city's own entry, so nearest-match picked the slice.
        let temp = tempfile::tempdir().unwrap();
        let got = location_name_in(&cache(&temp), 44.4897, 26.0884)
            .unwrap()
            .unwrap();
        assert!(
            got.starts_with("Bucharest"),
            "expected Bucharest, got {got}"
        );
    }

    #[test]
    fn numbered_administrative_slices_are_absent_from_the_data() {
        // Guards the generation step rather than one lookup: rebuilding the CSV
        // without the filter would put all twelve back, and only the Bucharest
        // coordinate above would notice.
        for slice in ["Sector 1", "Sector 6", "Ward 3", "Zona 179"] {
            assert!(
                !CITIES_CSV.contains(&format!(",{slice},")),
                "{slice} is back in the dataset; regenerate with the filter in data/README.md"
            );
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
