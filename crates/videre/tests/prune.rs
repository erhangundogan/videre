use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn prune_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

/// Fixture with two real files and one phantom path (never created on disk).
/// Returns (db_path, path_a, path_b, phantom_path).
fn fixture_db(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf, String) {
    let a = dir.join("a.jpg");
    let b = dir.join("b.jpg");
    std::fs::write(&a, b"img_a").unwrap();
    std::fs::write(&b, b"img_b").unwrap();
    let phantom = dir.join("gone.jpg").to_str().unwrap().to_string();

    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (
            path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
            created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
            exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER
        );",
    )
    .unwrap();
    for (path, hash) in [
        (a.to_str().unwrap(), "haaa"),
        (b.to_str().unwrap(), "hbbb"),
        (phantom.as_str(),    "hphantom"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, modified_at) VALUES (?1, ?2, '2020-01-01T00:00:00+00:00')",
            rusqlite::params![path, hash],
        )
        .unwrap();
    }
    (db, a, b, phantom)
}

fn add_embeddings(db: &std::path::Path, hashes: &[&str]) {
    let conn = Connection::open(db).unwrap();
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embeddings (
            hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
            embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
        );",
    )
    .unwrap();
    for hash in hashes {
        conn.execute(
            "INSERT OR IGNORE INTO embeddings VALUES (?1, 'test-model', X'0000', 'now')",
            rusqlite::params![hash],
        )
        .unwrap();
    }
}

fn row_exists(db: &std::path::Path, path: &str) -> bool {
    let conn = Connection::open(db).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM file_hashes WHERE path = ?1",
        rusqlite::params![path],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn get_modified_at(db: &std::path::Path, path: &str) -> Option<String> {
    let conn = Connection::open(db).unwrap();
    conn.query_row(
        "SELECT modified_at FROM file_hashes WHERE path = ?1",
        rusqlite::params![path],
        |r| r.get(0),
    )
    .ok()
}

fn embedding_exists(db: &std::path::Path, hash: &str) -> bool {
    let conn = Connection::open(db).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE hash = ?1",
        rusqlite::params![hash],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

#[test]
fn missing_default_db_prints_friendly_error() {
    let home = tempdir().unwrap();
    let out = Command::new(prune_bin())
        .arg("prune")
        .arg("--dry-run")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre prune");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no database found at"), "{stderr}");
    assert!(stderr.contains("videre dedupe"), "{stderr}");
}

fn run_prune(db: &std::path::Path, dry_run: bool) {
    let mut cmd = Command::new(prune_bin());
    cmd.arg("prune").arg("--db").arg(db).arg("--silent");
    if dry_run {
        cmd.arg("--dry-run");
    }
    let status = cmd.status().expect("failed to run videre prune");
    assert!(status.success());
}

#[test]
fn removes_row_for_missing_file() {
    let dir = tempdir().unwrap();
    let (db, _, _, phantom) = fixture_db(dir.path());
    assert!(row_exists(&db, &phantom));
    run_prune(&db, false);
    assert!(!row_exists(&db, &phantom), "phantom row should be removed");
}

#[test]
fn preserves_rows_for_existing_files() {
    let dir = tempdir().unwrap();
    let (db, a, b, _) = fixture_db(dir.path());
    run_prune(&db, false);
    assert!(row_exists(&db, a.to_str().unwrap()), "a.jpg should be kept");
    assert!(row_exists(&db, b.to_str().unwrap()), "b.jpg should be kept");
}

#[test]
fn syncs_modified_at_for_existing_files() {
    let dir = tempdir().unwrap();
    let (db, a, _, _) = fixture_db(dir.path());
    // DB has a stale '2020-01-01T00:00:00+00:00'; actual mtime is now
    run_prune(&db, false);
    let new_val = get_modified_at(&db, a.to_str().unwrap()).unwrap();
    assert_ne!(new_val, "2020-01-01T00:00:00+00:00", "modified_at should be refreshed");
}

#[test]
fn dry_run_makes_no_changes() {
    let dir = tempdir().unwrap();
    let (db, a, _, phantom) = fixture_db(dir.path());
    let original_mtime = get_modified_at(&db, a.to_str().unwrap());
    run_prune(&db, true);
    assert!(row_exists(&db, &phantom), "dry-run must not remove phantom row");
    assert_eq!(
        get_modified_at(&db, a.to_str().unwrap()),
        original_mtime,
        "dry-run must not update modified_at"
    );
}

#[test]
fn removes_orphan_embeddings_after_pruning() {
    let dir = tempdir().unwrap();
    let (db, _, _, _) = fixture_db(dir.path());
    // hphantom has an embedding; haaa and hbbb do not
    add_embeddings(&db, &["hphantom", "haaa"]);
    run_prune(&db, false);
    assert!(!embedding_exists(&db, "hphantom"), "orphan embedding should be removed");
    assert!(embedding_exists(&db, "haaa"), "embedding for surviving file should be kept");
}

#[test]
fn preserves_embedding_when_hash_shared_with_surviving_file() {
    let dir = tempdir().unwrap();
    let (db, a, _, _) = fixture_db(dir.path());
    // Give 'gone.jpg' the same hash as a.jpg so the hash still has a surviving row
    let conn = Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE file_hashes SET hash = 'haaa' WHERE path LIKE '%gone%'",
        [],
    ).unwrap();
    drop(conn);
    add_embeddings(&db, &["haaa"]);
    run_prune(&db, false);
    // gone.jpg row is removed, but haaa embedding must stay (a.jpg still uses it)
    assert!(!row_exists(&db, &dir.path().join("gone.jpg").to_str().unwrap().to_string()));
    assert!(embedding_exists(&db, "haaa"), "shared-hash embedding must not be pruned");
    let _ = a;
}
