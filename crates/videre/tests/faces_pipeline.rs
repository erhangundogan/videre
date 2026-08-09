mod common;
use common::{shared_cache_guard, videre_bin as bin};
use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn make_db(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);",
    )
    .unwrap();
    videre_core::db::ensure_file_hashes_columns(&conn);
    db
}

#[test]
fn exits_zero_on_empty_db() {
    let _serial = shared_cache_guard();
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let status = Command::new(bin())
        .arg("faces")
        .arg("--db")
        .arg(&db)
        .arg("--silent")
        .status()
        .expect("failed to run videre faces");
    assert!(status.success());
}

#[test]
fn creates_faces_table() {
    let _serial = shared_cache_guard();
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    Command::new(bin())
        .arg("faces")
        .arg("--db")
        .arg(&db)
        .arg("--silent")
        .status()
        .unwrap();
    let conn = Connection::open(&db).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='faces'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

/// Regression guard: `videre faces` on a database with nothing to process must
/// not touch the model cache.
///
/// This is what lets the two tests above stay ungated while every other
/// model-touching test checks `face_models_cached()` first. `commands/faces.rs`
/// returns at the `to_process.is_empty()` branch before loading anything, so
/// they cost nothing on a cold machine. Nothing enforced that: moving the
/// model load earlier would silently turn this file into a ~200 MB download on
/// every CI runner with a cold cache, with all tests still passing.
///
/// Points the child's `HF_HOME` at a fresh temp directory via `.env()`, which
/// overrides the inherited value, so a real download would land there and be
/// visible rather than being absorbed by the developer's warm cache.
#[test]
fn faces_on_an_empty_db_touches_no_model_cache() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let hf_home = tempdir().unwrap();

    let status = Command::new(bin())
        .arg("faces")
        .arg("--db")
        .arg(&db)
        .arg("--silent")
        .env("HF_HOME", hf_home.path())
        .status()
        .expect("failed to run videre faces");
    assert!(status.success());

    let mut found = Vec::new();
    let mut stack = vec![hf_home.path().to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                found.push(p);
            }
        }
    }
    assert!(
        found.is_empty(),
        "videre faces downloaded into a cold cache on an empty db: {found:?}"
    );
}
