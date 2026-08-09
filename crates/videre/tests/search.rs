mod common;
use common::videre_bin;
use std::fs;
use std::process::Command;
use tempfile::tempdir;

// Preflight check: `videre search` on a scanned-but-not-embedded db must fail
// fast with a "run videre embed first" message rather than loading the SigLIP
// model or silently returning zero results.
#[test]
fn text_search_errors_with_run_embed_first_when_no_embeddings_exist() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let status = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");
    assert!(status.success());

    let out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("sunset on beach")
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    // The message now names the model and the exact command to run, rather
    // than a bare "run videre embed first".
    assert!(stderr.contains("no embeddings"), "{stderr}");
    assert!(stderr.contains("videre embed --model"), "{stderr}");

    let json_out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("--json")
        .arg("sunset on beach")
        .output()
        .expect("failed to run videre search --json");
    assert!(!json_out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json_out.stdout)
        .expect("stdout must be one valid JSON error object");
    let msg = doc["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("no embeddings"), "{doc}");
    assert!(msg.contains("videre embed --model"), "{doc}");
}

#[test]
fn location_search_returns_nearby_photos_sorted_by_distance() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let status = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");
    assert!(status.success());

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE file_hashes SET gps_lat = 48.8566, gps_lon = 2.3522 WHERE path LIKE '%a.jpg'",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS geocode_cache (query TEXT PRIMARY KEY, lat REAL NOT NULL, lon REAL NOT NULL, resolved_at TEXT NOT NULL);
         INSERT INTO geocode_cache (query, lat, lon, resolved_at) VALUES ('paris, france', 48.8566, 2.3522, '2026-01-01');",
    )
    .unwrap();
    drop(conn);

    let out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("--location")
        .arg("Paris, France")
        .arg("--json")
        .output()
        .expect("failed to run videre search --location");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = doc["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0]["distance_km"].as_f64().unwrap() < 1.0);
}

#[test]
fn location_search_excludes_photos_outside_radius() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    // Tokyo, far from Paris.
    conn.execute(
        "UPDATE file_hashes SET gps_lat = 35.6762, gps_lon = 139.6503 WHERE path LIKE '%a.jpg'",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS geocode_cache (query TEXT PRIMARY KEY, lat REAL NOT NULL, lon REAL NOT NULL, resolved_at TEXT NOT NULL);
         INSERT INTO geocode_cache (query, lat, lon, resolved_at) VALUES ('paris, france', 48.8566, 2.3522, '2026-01-01');",
    )
    .unwrap();
    drop(conn);

    let out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("--location")
        .arg("Paris, France")
        .arg("--radius")
        .arg("50")
        .arg("--json")
        .output()
        .expect("failed to run videre search --location");
    assert!(out.status.success());

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["results"].as_array().unwrap().len(), 0);
}

#[test]
fn location_and_radius_conflict_with_other_search_modes() {
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    let out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("--location")
        .arg("Berlin, Germany")
        .arg("--person")
        .arg("Alice")
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success());

    let out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("--radius")
        .arg("10")
        .output()
        .expect("failed to run videre search");
    assert!(
        !out.status.success(),
        "--radius without --location must be rejected"
    );
}

#[test]
fn location_search_truncates_to_top_k_closest() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content a").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"content b").unwrap();
    fs::write(scan_dir.path().join("c.jpg"), b"content c").unwrap();

    Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    // Three points at increasing distance from the geocoded "Paris, France"
    // center (48.8566, 2.3522): a is closest, c is furthest, all within the
    // default 20km radius.
    conn.execute(
        "UPDATE file_hashes SET gps_lat = 48.8566, gps_lon = 2.3522 WHERE path LIKE '%a.jpg'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE file_hashes SET gps_lat = 48.9000, gps_lon = 2.3522 WHERE path LIKE '%b.jpg'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE file_hashes SET gps_lat = 49.0000, gps_lon = 2.3522 WHERE path LIKE '%c.jpg'",
        [],
    )
    .unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS geocode_cache (query TEXT PRIMARY KEY, lat REAL NOT NULL, lon REAL NOT NULL, resolved_at TEXT NOT NULL);
         INSERT INTO geocode_cache (query, lat, lon, resolved_at) VALUES ('paris, france', 48.8566, 2.3522, '2026-01-01');",
    )
    .unwrap();
    drop(conn);

    let out = Command::new(videre_bin())
        .arg("search")
        .arg("--db")
        .arg(&db_path)
        .arg("--location")
        .arg("Paris, France")
        .arg("-k")
        .arg("2")
        .arg("--json")
        .output()
        .expect("failed to run videre search --location");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let results = doc["results"].as_array().unwrap();
    assert_eq!(
        results.len(),
        2,
        "-k 2 must truncate 3 in-radius matches down to 2"
    );
    assert!(
        results[0]["path"].as_str().unwrap().ends_with("a.jpg"),
        "{doc}"
    );
    assert!(
        results[1]["path"].as_str().unwrap().ends_with("b.jpg"),
        "{doc}"
    );
}
