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
