//! Forward geocoding: place name (e.g. "Berlin, Germany") -> (lat, lon), via
//! the free public Nominatim (OpenStreetMap) API, with a local cache so a
//! repeated query never repeats the network call. See
//! docs/superpowers/specs/2026-08-01-location-clustering-design.md.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

/// Idempotent: creates `geocode_cache` if it doesn't already exist.
/// `resolved_at` is descriptive only in this version - no expiry/refresh
/// logic is implemented; a cache hit is always used regardless of age (a
/// place name's coordinates essentially never change).
pub fn ensure_geocode_cache_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS geocode_cache (
            query       TEXT PRIMARY KEY,
            lat         REAL NOT NULL,
            lon         REAL NOT NULL,
            resolved_at TEXT NOT NULL
        );",
    )
}

fn normalize_query(query: &str) -> String {
    query.trim().to_lowercase()
}

/// Forward-geocodes `query` to `(lat, lon)`, checking `geocode_cache` first.
/// Only calls the network (`forward_geocode_network`) on a cache miss, then
/// writes the result back so the same query never repeats the network call.
pub fn forward_geocode_cached(conn: &Connection, query: &str) -> Result<(f64, f64)> {
    let key = normalize_query(query);
    let cached: Option<(f64, f64)> = conn
        .query_row(
            "SELECT lat, lon FROM geocode_cache WHERE query = ?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .context("querying geocode_cache")?;
    if let Some(coords) = cached {
        return Ok(coords);
    }

    let (lat, lon) = forward_geocode_network(query)?;
    conn.execute(
        "INSERT INTO geocode_cache (query, lat, lon, resolved_at) \
         VALUES (?1, ?2, ?3, datetime('now'))",
        params![key, lat, lon],
    )
    .context("writing geocode_cache")?;
    Ok((lat, lon))
}

/// Calls the Nominatim (OpenStreetMap) free public geocoding API. Not
/// covered by automated tests (a real network call) - verified manually
/// during implementation and via the `#[ignore]`d test below, same
/// treatment as this project's other real-network/model-download paths
/// (e.g. SigLIP/ArcFace weight downloads).
pub fn forward_geocode_network(query: &str) -> Result<(f64, f64)> {
    #[derive(serde::Deserialize)]
    struct NominatimResult {
        lat: String,
        lon: String,
    }

    let user_agent = concat!(
        "videre/",
        env!("CARGO_PKG_VERSION"),
        " (https://github.com/erhangundogan/videre)"
    );

    let results: Vec<NominatimResult> = ureq::get("https://nominatim.openstreetmap.org/search")
        .header("User-Agent", user_agent)
        .query("q", query)
        .query("format", "jsonv2")
        .query("limit", "1")
        .call()
        .with_context(|| format!("geocoding request for {query:?} failed"))?
        .body_mut()
        .read_json()
        .with_context(|| format!("parsing geocoding response for {query:?}"))?;

    let first = results
        .into_iter()
        .next()
        .with_context(|| format!("could not geocode {query:?}: no results found"))?;

    let lat: f64 = first
        .lat
        .parse()
        .with_context(|| format!("invalid latitude in geocoding response for {query:?}"))?;
    let lon: f64 = first
        .lon
        .parse()
        .with_context(|| format!("invalid longitude in geocoding response for {query:?}"))?;

    Ok((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_geocode_cache_table_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_geocode_cache_table(&conn).unwrap();
        ensure_geocode_cache_table(&conn).unwrap(); // second call must not error
    }

    #[test]
    fn normalize_query_trims_and_lowercases() {
        assert_eq!(normalize_query("  Berlin, Germany  "), "berlin, germany");
    }

    #[test]
    fn forward_geocode_cached_returns_cached_value_without_touching_network() {
        let conn = Connection::open_in_memory().unwrap();
        ensure_geocode_cache_table(&conn).unwrap();
        conn.execute(
            "INSERT INTO geocode_cache (query, lat, lon, resolved_at) \
             VALUES ('berlin, germany', 52.52, 13.405, '2026-01-01')",
            [],
        )
        .unwrap();

        // A passing, fast, offline test proves the cache-hit branch
        // returned before ever calling forward_geocode_network.
        let (lat, lon) = forward_geocode_cached(&conn, "Berlin, Germany").unwrap();
        assert!((lat - 52.52).abs() < 1e-9);
        assert!((lon - 13.405).abs() < 1e-9);
    }

    #[test]
    #[ignore = "hits the real Nominatim API - run manually with --ignored"]
    fn forward_geocode_network_resolves_a_real_place() {
        let (lat, lon) = forward_geocode_network("Berlin, Germany").unwrap();
        assert!((lat - 52.52).abs() < 0.1, "got lat {lat}");
        assert!((lon - 13.405).abs() < 0.1, "got lon {lon}");
    }
}
