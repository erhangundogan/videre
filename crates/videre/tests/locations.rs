use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn videre_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

#[test]
fn clusters_nearby_gps_rows_and_prints_json_summary() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             gps_lat REAL, gps_lon REAL);
         INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon) VALUES
             ('/tmp/a.jpg', 'ha', 'jpg', 48.8566, 2.3522),
             ('/tmp/b.jpg', 'hb', 'jpg', 48.8606, 2.3376),
             ('/tmp/c.jpg', 'hc', 'jpg', 51.5074, -0.1278);",
    )
    .unwrap();
    drop(conn);

    let out = Command::new(videre_bin())
        .arg("locations")
        .arg("--db")
        .arg(&db)
        .arg("--radius")
        .arg("50")
        .arg("--json")
        .output()
        .expect("failed to run videre locations");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["schema_version"], 1);
    let clusters = doc["clusters"].as_array().unwrap();
    assert_eq!(clusters.len(), 2, "{doc}");
    let mut counts: Vec<i64> = clusters.iter().map(|c| c["photo_count"].as_i64().unwrap()).collect();
    counts.sort();
    assert_eq!(counts, vec![1, 2]);
}

#[test]
fn zero_gps_rows_prints_empty_and_exits_zero() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             gps_lat REAL, gps_lon REAL);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/a.jpg', 'ha', 'jpg');",
    )
    .unwrap();
    drop(conn);

    let out = Command::new(videre_bin())
        .arg("locations")
        .arg("--db")
        .arg(&db)
        .arg("--json")
        .output()
        .expect("failed to run videre locations");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["clusters"].as_array().unwrap().len(), 0);
}

#[test]
fn geojson_output_is_a_feature_collection_with_lon_lat_order() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             gps_lat REAL, gps_lon REAL);
         INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon) VALUES
             ('/tmp/a.jpg', 'ha', 'jpg', 48.8566, 2.3522);",
    )
    .unwrap();
    drop(conn);

    let out = Command::new(videre_bin())
        .arg("locations")
        .arg("--db")
        .arg(&db)
        .arg("--geojson")
        .output()
        .expect("failed to run videre locations");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["type"], "FeatureCollection");
    let features = doc["features"].as_array().unwrap();
    assert_eq!(features.len(), 1);
    assert_eq!(features[0]["geometry"]["coordinates"][0], 2.3522);
    assert_eq!(features[0]["geometry"]["coordinates"][1], 48.8566);
}

#[test]
fn json_and_geojson_are_mutually_exclusive() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");

    let out = Command::new(videre_bin())
        .arg("locations")
        .arg("--db")
        .arg(&db)
        .arg("--json")
        .arg("--geojson")
        .output()
        .expect("failed to run videre locations");
    assert!(!out.status.success());
}

#[test]
fn rerun_replaces_previous_clusters_not_appends() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             gps_lat REAL, gps_lon REAL);
         INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon) VALUES
             ('/tmp/a.jpg', 'ha', 'jpg', 48.8566, 2.3522);",
    )
    .unwrap();
    drop(conn);

    for _ in 0..2 {
        let status = Command::new(videre_bin())
            .arg("locations")
            .arg("--db")
            .arg(&db)
            .arg("--silent")
            .status()
            .expect("failed to run videre locations");
        assert!(status.success());
    }

    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM location_clusters", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "rerunning must replace, not accumulate, clusters");
}
