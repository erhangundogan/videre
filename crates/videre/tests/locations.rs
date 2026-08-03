use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

/// Points `VIDERE_HOME` at a throwaway directory for this whole test binary.
/// Spawned `videre` child processes inherit the environment, so their lock
/// files land there instead of the developer's real `~/.videre/locks` - locks
/// live under the videre home now rather than beside the database, so without
/// this every run would leave permanent litter in the real home (test database
/// names are random, so the files would accumulate rather than be reused).
///
/// Called from `videre_bin()` so it covers every spawn site automatically. The
/// `set_var` runs inside `get_or_init` so it happens exactly once: tests share
/// a process and run in parallel, and calling `set_var` from several threads
/// would otherwise race every concurrent `getenv`. Tests that set their own
/// `VIDERE_HOME` per-command still win, since `.env()` overrides what's
/// inherited.
fn isolated_home() {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("videre-it-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create isolated test home");
        std::env::set_var("VIDERE_HOME", &dir);
        dir
    });
}

fn videre_bin() -> std::path::PathBuf {
    isolated_home();
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

#[test]
fn file_hashes_location_cluster_id_is_assigned_and_cleared_on_rerun() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             gps_lat REAL, gps_lon REAL);
         INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon) VALUES
             ('/tmp/a.jpg', 'ha', 'jpg', 48.8566, 2.3522),
             ('/tmp/b.jpg', 'hb', 'jpg', 48.8606, 2.3376);",
    )
    .unwrap();
    drop(conn);

    let status = Command::new(videre_bin())
        .arg("locations")
        .arg("--db")
        .arg(&db)
        .arg("--radius")
        .arg("50")
        .arg("--silent")
        .status()
        .expect("failed to run videre locations");
    assert!(status.success());

    let conn = Connection::open(&db).unwrap();
    let (cluster_a, cluster_b): (Option<i64>, Option<i64>) = (
        conn.query_row(
            "SELECT location_cluster_id FROM file_hashes WHERE path = '/tmp/a.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap(),
        conn.query_row(
            "SELECT location_cluster_id FROM file_hashes WHERE path = '/tmp/b.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap(),
    );
    assert!(cluster_a.is_some(), "expected a.jpg to get a location_cluster_id");
    assert_eq!(cluster_a, cluster_b, "both nearby photos must land in the same cluster");
    drop(conn);

    // Remove b's GPS so a rerun reclusters it out of any cluster - the
    // NULL-clearing step (locations.rs's "clears file_hashes.location_cluster_id")
    // must actually take effect, not just leave the previous run's value in place.
    let conn = Connection::open(&db).unwrap();
    conn.execute("UPDATE file_hashes SET gps_lat = NULL, gps_lon = NULL WHERE path = '/tmp/b.jpg'", [])
        .unwrap();
    drop(conn);

    let status = Command::new(videre_bin())
        .arg("locations")
        .arg("--db")
        .arg(&db)
        .arg("--radius")
        .arg("50")
        .arg("--silent")
        .status()
        .expect("failed to run videre locations");
    assert!(status.success());

    let conn = Connection::open(&db).unwrap();
    let cluster_b_after: Option<i64> = conn
        .query_row(
            "SELECT location_cluster_id FROM file_hashes WHERE path = '/tmp/b.jpg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        cluster_b_after, None,
        "b.jpg's stale location_cluster_id must be cleared once it no longer has GPS"
    );
}
