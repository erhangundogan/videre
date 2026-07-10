use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn report_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("dupe-report");
    path
}

/// Fixture: two duplicates (hash hdup), one singular (hsing), one video (hvid).
/// Creates real files on disk so the existence filter in query_all_files passes.
/// Returns (db_path, [path_a, path_b, path_c, path_d]).
fn fixture_db(
    dir: &std::path::Path,
    with_embeddings: bool,
) -> (std::path::PathBuf, [std::path::PathBuf; 4]) {
    let pics = dir.join("pics");
    std::fs::create_dir(&pics).unwrap();
    let files = [
        pics.join("a.jpg"),
        pics.join("b.jpg"),
        pics.join("c.jpg"),
        pics.join("d.mov"),
    ];
    for f in &files {
        std::fs::write(f, b"dummy").unwrap();
    }

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
    for (path, hash, ext) in [
        (files[0].to_str().unwrap(), "hdup", "jpg"),
        (files[1].to_str().unwrap(), "hdup", "jpg"),
        (files[2].to_str().unwrap(), "hsing", "jpg"),
        (files[3].to_str().unwrap(), "hvid", "mov"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext) VALUES (?1, ?2, 100, ?3)",
            rusqlite::params![path, hash, ext],
        )
        .unwrap();
    }
    if with_embeddings {
        conn.execute_batch(
            "CREATE TABLE embeddings (
                hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
                embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
            );",
        )
        .unwrap();
        let v1 = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let v2 = dupe_core::vectors::to_f16_bytes(&[0.0, 1.0]);
        for (hash, v) in [("hdup", v1), ("hsing", v2)] {
            conn.execute(
                "INSERT INTO embeddings VALUES (?1, ?2, ?3, 'now')",
                rusqlite::params![hash, dupe_core::embeddings::DEFAULT_MODEL_ID, v],
            )
            .unwrap();
        }
    }
    (db, files)
}

fn run_report(db: &std::path::Path, all: bool) -> String {
    let out = db.with_extension("html");
    let mut cmd = Command::new(report_bin());
    cmd.arg(db).arg("-o").arg(&out);
    if all {
        cmd.arg("--all");
    }
    let status = cmd.status().expect("failed to run dupe-report");
    assert!(status.success());
    std::fs::read_to_string(&out).unwrap()
}

#[test]
fn without_all_flag_no_gallery_or_vectors() {
    let dir = tempdir().unwrap();
    let (db, _) = fixture_db(dir.path(), true);
    let html = run_report(&db, false);
    assert!(!html.contains("var VEC_B64="));
    assert!(!html.contains("var ALLFILES="));
    assert!(!html.contains("id=\"gallery\""));
    assert!(!html.contains("id=\"results\""));
}

#[test]
fn all_flag_emits_gallery_and_vectors() {
    let dir = tempdir().unwrap();
    let (db, files) = fixture_db(dir.path(), true);
    let html = run_report(&db, true);
    assert!(html.contains("var ALLFILES="));
    // All four files present, including the singular and the video
    assert!(html.contains(files[2].to_str().unwrap()), "c.jpg missing");
    assert!(html.contains(files[3].to_str().unwrap()), "d.mov missing");
    assert!(html.contains("var VEC_B64=\""));
    assert!(html.contains("var VEC_HASHES=[\"hdup\",\"hsing\"];"));
    assert!(html.contains("var VEC_DIM=2;"));
    assert!(html.contains("id=\"gallery\""));
    assert!(html.contains("id=\"results\""));
}

#[test]
fn all_flag_without_embeddings_renders_gallery_only() {
    let dir = tempdir().unwrap();
    let (db, _) = fixture_db(dir.path(), false);
    let html = run_report(&db, true);
    assert!(html.contains("var ALLFILES="));
    assert!(html.contains("id=\"gallery\""));
    assert!(html.contains("var VEC_B64=\"\";"));
    // JS must guard on empty vectors: constants exist but empty
    assert!(html.contains("var VEC_HASHES=[];"));
    assert!(html.contains("var VEC_DIM=0;"));
}

#[test]
fn all_flag_page_contains_similarity_js() {
    let dir = tempdir().unwrap();
    let (db, _) = fixture_db(dir.path(), true);
    let html = run_report(&db, true);
    for marker in [
        "function decodeVecs(",
        "function findSimilar(",
        "function renderResults(",
        "function clearResults(",
        "function buildCard(",
        "function showMoreGallery(",
        "data-similar=",
        ".results-panel",
        ".gallery{",
    ] {
        assert!(html.contains(marker), "missing marker: {marker}");
    }
}

#[test]
fn without_all_flag_no_similarity_side_effects() {
    let dir = tempdir().unwrap();
    let (db, _) = fixture_db(dir.path(), true);
    let html = run_report(&db, false);
    // Shared JS may define functions, but no gallery containers may exist
    assert!(!html.contains("id=\"gallery\""));
    assert!(!html.contains("id=\"results\""));
}

#[test]
fn all_flag_excludes_files_deleted_after_scan() {
    let dir = tempdir().unwrap();
    let (db, files) = fixture_db(dir.path(), false);
    // Delete c.jpg after it was recorded in the database
    std::fs::remove_file(&files[2]).unwrap();
    let html = run_report(&db, true);
    // Deleted file must not appear in the gallery
    assert!(!html.contains(files[2].to_str().unwrap()), "deleted file appears in gallery");
    // The other files still on disk must appear
    assert!(html.contains(files[0].to_str().unwrap()), "a.jpg missing");
    assert!(html.contains(files[3].to_str().unwrap()), "d.mov missing");
}
