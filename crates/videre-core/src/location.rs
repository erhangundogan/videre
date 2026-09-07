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
