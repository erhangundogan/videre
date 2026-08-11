mod common;
use common::videre_bin;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::{tempdir, TempDir};

/// Two dated files and nothing else: enough to prove a date predicate narrows.
/// Built by hand rather than by scanning, so the dates are the fixture rather
/// than whatever mtime the filesystem happened to give the temp files.
fn fixture_db_with_dates() -> (TempDir, PathBuf) {
    let dir = tempdir().unwrap();
    let db = dir.path().join("t.db");
    let conn = videre_core::db::open_wal(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_hashes (
            path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
            created_at TEXT, modified_at TEXT, ext TEXT, mime TEXT, phash INTEGER,
            exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
         INSERT INTO file_hashes (path, hash, size_bytes, modified_at, exif_date, ext, mime)
         VALUES ('/may.jpg','h1',10,'2025-05-14T10:00:00','2025-05-14T10:00:00','jpg','image/jpeg'),
                ('/jun.jpg','h2',10,'2025-06-14T10:00:00','2025-06-14T10:00:00','jpg','image/jpeg');
         CREATE TABLE IF NOT EXISTS classifications (
            model_id TEXT NOT NULL, hash TEXT NOT NULL, category TEXT NOT NULL,
            confidence REAL NOT NULL, classified_at TEXT NOT NULL,
            PRIMARY KEY (model_id, hash));",
    )
    .unwrap();
    (dir, db)
}

/// The dated fixture plus two confirmed faces, both labelled Alice.
fn fixture_db_with_people() -> (TempDir, PathBuf) {
    let (dir, db) = fixture_db_with_dates();
    let conn = videre_core::db::open_wal(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
            bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL, cluster_id INTEGER,
            person_label TEXT, confirmed INTEGER DEFAULT 0, is_primary INTEGER DEFAULT 0);
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
         VALUES ('h1','[]',x'00','Alice',1), ('h2','[]',x'00','Alice',1);",
    )
    .unwrap();
    (dir, db)
}

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

#[test]
fn date_filter_narrows_results() {
    let (_home, db) = fixture_db_with_dates();
    let out = Command::new(videre_bin())
        .args(["search", "--db", db.to_str().unwrap(), "--date", "2025-05"])
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        doc["count"], 1,
        "only the May 2025 file should match: {doc}"
    );
    assert!(doc["results"][0]["path"]
        .as_str()
        .unwrap()
        .ends_with("may.jpg"));
}

#[test]
fn filters_compose_and_narrow_further() {
    let (_home, db) = fixture_db_with_dates();
    let out = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--category",
            "document",
            "--date",
            "2025-05",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["count"], 0, "no document in May 2025 in the fixture");
}

#[test]
fn person_and_date_compose() {
    let (_home, db) = fixture_db_with_people();
    let alone = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--person",
            "Alice",
            "--json",
        ])
        .output()
        .unwrap();
    let alone: serde_json::Value = serde_json::from_slice(&alone.stdout).unwrap();
    assert_eq!(alone["count"], 2, "{alone}");

    let composed = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--person",
            "Alice",
            "--date",
            "2025-05",
            "--json",
        ])
        .output()
        .unwrap();
    let composed: serde_json::Value = serde_json::from_slice(&composed.stdout).unwrap();
    assert_eq!(
        composed["count"], 1,
        "the date must narrow Alice: {composed}"
    );
}

#[test]
fn top_k_now_applies_to_person_search() {
    let (_home, db) = fixture_db_with_people();
    let out = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--person",
            "Alice",
            "-k",
            "1",
            "--json",
        ])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["results"].as_array().unwrap().len(), 1, "{doc}");
}

#[test]
fn hits_carry_the_effective_date() {
    let (_home, db) = fixture_db_with_dates();
    let out = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--date",
            "2025-05",
            "--json",
        ])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        doc["results"][0]["date"], "2025-05-14T10:00:00",
        "every hit should report its effective date: {doc}"
    );
}

#[test]
fn explicit_sort_reorders_and_scores_prepend_the_primary_key() {
    let (_home, db) = fixture_db_with_dates();
    let newest = Command::new(videre_bin())
        .args(["search", "--db", db.to_str().unwrap(), "--date", "2025"])
        .output()
        .unwrap();
    let newest = String::from_utf8_lossy(&newest.stdout);
    assert_eq!(
        newest.lines().collect::<Vec<_>>(),
        vec!["/jun.jpg", "/may.jpg"],
        "date descending is the default without a query"
    );

    let oldest = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--date",
            "2025",
            "--sort",
            "date:asc",
            "--scores",
        ])
        .output()
        .unwrap();
    let oldest = String::from_utf8_lossy(&oldest.stdout);
    assert_eq!(
        oldest.lines().collect::<Vec<_>>(),
        vec![
            "2025-05-14T10:00:00\t/may.jpg",
            "2025-06-14T10:00:00\t/jun.jpg"
        ],
        "--scores prepends the primary sort key, here the date"
    );
}

#[test]
fn sort_distance_without_location_is_rejected() {
    let (_home, db) = fixture_db_with_dates();
    let out = Command::new(videre_bin())
        .args(["search", "--db", db.to_str().unwrap(), "--sort", "distance"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--location"),
        "the error must say what is missing: {err}"
    );

    // Under --json the single JSON object on stdout is the only channel, so
    // the same message has to be reachable there too.
    let json = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--sort",
            "distance",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!json.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(
        doc["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--location"),
        "{doc}"
    );
}

#[test]
fn sort_relevance_without_a_query_is_rejected() {
    let (_home, db) = fixture_db_with_dates();
    let out = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--date",
            "2025",
            "--sort",
            "relevance",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--image"), "{err}");
}

#[test]
fn a_bad_sort_spec_fails_before_any_query_work() {
    let (_home, db) = fixture_db_with_dates();
    let out = Command::new(videre_bin())
        .args([
            "search",
            "--db",
            db.to_str().unwrap(),
            "--date",
            "2025",
            "--sort",
            "bogus",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("relevance"), "{err}");
}

#[test]
fn a_bad_date_is_rejected_with_the_accepted_forms() {
    let (_home, db) = fixture_db_with_dates();
    let out = Command::new(videre_bin())
        .args(["search", "--db", db.to_str().unwrap(), "--date", "May 2025"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("YYYY"), "{err}");
}
