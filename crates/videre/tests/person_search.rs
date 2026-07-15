use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    p.pop(); // debug/
    p.push("videre");
    p
}

fn make_db(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
         bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
         cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
         is_primary INTEGER DEFAULT 0);
         CREATE TABLE embeddings (hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
         embedding BLOB NOT NULL, embedded_at TEXT NOT NULL);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/alice1.jpg', 'hash1', 'jpg');
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/alice2.jpg', 'hash2', 'jpg');
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/bob.jpg', 'hash3', 'jpg');
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
           VALUES ('hash1', '0,0,50,50', X'0000', 'Alice', 1);
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
           VALUES ('hash2', '0,0,50,50', X'0000', 'Alice', 1);
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
           VALUES ('hash3', '0,0,50,50', X'0000', 'Bob', 1);",
    )
    .unwrap();
    db
}

#[test]
fn person_search_prints_confirmed_paths() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let out = Command::new(bin())
        .arg("search")
        .arg(&db)
        .arg("--person")
        .arg("Alice")
        .output()
        .expect("failed to run videre search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.contains("/tmp/alice1.jpg"),
        "Expected alice1 in output:\n{stdout}"
    );
    assert!(
        stdout.contains("/tmp/alice2.jpg"),
        "Expected alice2 in output:\n{stdout}"
    );
    assert!(
        !stdout.contains("/tmp/bob.jpg"),
        "Expected bob not in output:\n{stdout}"
    );
}

#[test]
fn person_search_empty_for_unknown_name() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let out = Command::new(bin())
        .arg("search")
        .arg(&db)
        .arg("--person")
        .arg("Unknown")
        .output()
        .expect("failed to run videre search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.trim().is_empty(),
        "Expected empty stdout:\n{stdout}"
    );
}

#[test]
fn person_search_unconfirmed_not_returned() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test2.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
         bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
         cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
         is_primary INTEGER DEFAULT 0);
         CREATE TABLE embeddings (hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
         embedding BLOB NOT NULL, embedded_at TEXT NOT NULL);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/carol.jpg', 'hash4', 'jpg');
         INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
           VALUES ('hash4', '0,0,50,50', X'0000', 'Carol', 0);",
    )
    .unwrap();
    let out = Command::new(bin())
        .arg("search")
        .arg(&db)
        .arg("--person")
        .arg("Carol")
        .output()
        .expect("failed to run videre search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.trim().is_empty(),
        "Unconfirmed faces should not be returned:\n{stdout}"
    );
}

#[test]
fn person_search_json_outputs_document() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let out = Command::new(bin())
        .arg("search")
        .arg(&db)
        .arg("--person")
        .arg("Alice")
        .arg("--json")
        .output()
        .expect("failed to run videre search");
    assert!(out.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["query"]["kind"], "person");
    assert_eq!(doc["query"]["value"], "Alice");
    assert_eq!(doc["count"], 2);
    let results = doc["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for r in results {
        assert!(r["path"].as_str().unwrap().contains("alice"));
        assert!(r.get("hash").is_none(), "person hits omit hash: {r}");
        assert!(r.get("score").is_none(), "person hits omit score: {r}");
    }
}

#[test]
fn person_search_json_scores_flag_is_silent_noop() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let plain = Command::new(bin())
        .arg("search").arg(&db).arg("--person").arg("Alice").arg("--json")
        .output().expect("failed to run videre search");
    let with_scores = Command::new(bin())
        .arg("search").arg(&db).arg("--person").arg("Alice").arg("--json").arg("--scores")
        .output().expect("failed to run videre search");
    assert!(with_scores.status.success(), "--scores with --json must not be rejected");
    assert_eq!(plain.stdout, with_scores.stdout, "--scores must be a no-op under --json");
}

#[test]
fn search_json_error_is_json_object_on_stdout() {
    let dir = tempdir().unwrap();
    // Fresh DB with no tables: open_wal succeeds (SQLite creates the file),
    // then load_embeddings fails (no embeddings table): the reliable error trigger.
    let db = dir.path().join("empty.db");
    Connection::open(&db).unwrap();
    let out = Command::new(bin())
        .arg("search")
        .arg(&db)
        .arg("beach")
        .arg("--json")
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success(), "must exit nonzero");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("even on error, stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert!(doc["error"]["message"].as_str().is_some());
    assert!(doc.get("results").is_none());
}

#[test]
fn person_search_json_empty_is_silent_on_stderr() {
    // A clean agent invocation (--json) must not leak the human "No confirmed
    // photos" line to stderr; the empty result is already conveyed as count 0.
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let out = Command::new(bin())
        .arg("search")
        .arg(&db)
        .arg("--person")
        .arg("Unknown")
        .arg("--json")
        .output()
        .expect("failed to run videre search");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["count"], 0);
    assert!(doc["results"].as_array().unwrap().is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("No confirmed photos"),
        "--json must not print the human not-found line to stderr:\n{stderr}"
    );
}
