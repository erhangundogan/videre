//! End-to-end coverage for the per-model embedding split: several models in
//! one library, and several libraries not treading on each other.

use std::process::Command;
use tempfile::tempdir;

/// Points `VIDERE_HOME` at a throwaway directory for this whole test binary.
///
/// Load-bearing here: embeddings live at `<VIDERE_HOME>/embeddings/...`, so
/// without this the test process writes into the developer's real `~/.videre`
/// while the spawned binary reads an isolated one. Set inside `get_or_init`
/// so it happens exactly once, since tests share a process and run in
/// parallel and a per-test `set_var` would race every concurrent `getenv`.
fn isolated_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("videre-mm-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create isolated test home");
        std::env::set_var("VIDERE_HOME", &dir);
        dir
    })
}

fn videre_bin() -> std::path::PathBuf {
    isolated_home();
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

const MODEL_A: &str = "google/siglip2-base-patch16-384";
const MODEL_B: &str = "google/siglip-base-patch16-224";

/// A scanned library with one synthetic embedding in each of two models.
///
/// Synthetic vectors on purpose: this covers routing and cleanup, not
/// embedding quality, and loading real SigLIP weights would make it slow and
/// network-dependent.
fn seeded_library_in(
    root: &std::path::Path,
    name: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let scan_dir = root.join(format!("{name}-files"));
    std::fs::create_dir_all(&scan_dir).unwrap();
    let photo = scan_dir.join("a.jpg");
    std::fs::copy("tests/fixtures/sample_with_exif.jpg", &photo).unwrap();

    let db = root.join(format!("{name}.db"));
    let status = Command::new(videre_bin())
        .args(["scan", "--silent", "--output-sqlite"])
        .arg(&db)
        .arg(&scan_dir)
        .status()
        .expect("failed to run videre scan");
    assert!(status.success(), "scan failed for {name}");

    let conn = rusqlite::Connection::open(&db).unwrap();
    let hash: String = conn
        .query_row("SELECT hash FROM file_hashes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    for model in [MODEL_A, MODEL_B] {
        videre_core::embeddings_db::attach(&conn, &db, model, true).unwrap();
        conn.execute(
            "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
             VALUES (?1, ?2, zeroblob(1536), '2026-08-05T00:00:00')",
            rusqlite::params![hash, model],
        )
        .unwrap();
        videre_core::embeddings_db::detach(&conn).unwrap();
    }
    (db, photo)
}

fn count_embeddings(db: &std::path::Path, model: &str) -> i64 {
    isolated_home();
    let path = videre_core::embeddings_db::db_path(db, model).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .unwrap_or(0)
}

#[test]
fn two_models_coexist_in_one_library() {
    let dir = tempdir().unwrap();
    let (db, _photo) = seeded_library_in(dir.path(), "coexist");

    assert_eq!(count_embeddings(&db, MODEL_A), 1);
    assert_eq!(count_embeddings(&db, MODEL_B), 1);
    assert_ne!(
        videre_core::embeddings_db::db_path(&db, MODEL_A).unwrap(),
        videre_core::embeddings_db::db_path(&db, MODEL_B).unwrap(),
        "each model must own a distinct file"
    );
}

#[test]
fn stats_reports_every_model_separately() {
    let dir = tempdir().unwrap();
    let (db, _photo) = seeded_library_in(dir.path(), "statsmm");

    let out = Command::new(videre_bin())
        .args(["stats", "--json", "--db"])
        .arg(&db)
        .output()
        .expect("failed to run videre stats");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let embeddings = doc["library"]["embeddings"].as_array().unwrap();
    assert_eq!(embeddings.len(), 2, "{doc}");

    let ids: Vec<&str> = embeddings
        .iter()
        .map(|e| e["model_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&MODEL_A) && ids.contains(&MODEL_B), "{doc}");
    for e in embeddings {
        assert_eq!(e["count"], 1, "{e}");
        // 1536-byte f16 blob is 768 dimensions, derived rather than assumed.
        assert_eq!(e["dims"], 768, "{e}");
    }
}

#[test]
fn search_names_the_available_models_when_one_is_missing() {
    let dir = tempdir().unwrap();
    let (db, _photo) = seeded_library_in(dir.path(), "missingmodel");

    let out = Command::new(videre_bin())
        .args(["search", "anything", "--db"])
        .arg(&db)
        .args(["--model", "google/does-not-exist-384"])
        .output()
        .expect("failed to run videre search");

    assert!(
        !out.status.success(),
        "must exit non-zero rather than return zero hits"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no embeddings for google/does-not-exist-384"), "{stderr}");
    assert!(
        stderr.contains(MODEL_A) && stderr.contains(MODEL_B),
        "the error must list what IS available: {stderr}"
    );
}

#[test]
fn prune_removes_orphans_from_every_model_database() {
    let dir = tempdir().unwrap();
    let (db, photo) = seeded_library_in(dir.path(), "pruneall");

    std::fs::remove_file(&photo).unwrap();
    let out = Command::new(videre_bin())
        .args(["prune", "--db"])
        .arg(&db)
        .output()
        .expect("failed to run videre prune");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    for model in [MODEL_A, MODEL_B] {
        assert_eq!(
            count_embeddings(&db, model),
            0,
            "{model} still holds an orphan"
        );
    }
}

#[test]
fn pruning_one_library_leaves_another_librarys_embeddings_alone() {
    // The reason embeddings are per-library rather than global. A global
    // layout cannot see the other library's file_hashes, so this sweep would
    // delete vectors that are still in use, at hours of recompute to restore.
    let dir = tempdir().unwrap();
    let (db_a, photo_a) = seeded_library_in(dir.path(), "libA");
    let (db_b, _photo_b) = seeded_library_in(dir.path(), "libB");

    assert_eq!(
        count_embeddings(&db_b, MODEL_A),
        1,
        "library B must start with an embedding for this to prove anything"
    );

    std::fs::remove_file(&photo_a).unwrap();
    let out = Command::new(videre_bin())
        .args(["prune", "--db"])
        .arg(&db_a)
        .output()
        .expect("failed to run videre prune");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(count_embeddings(&db_a, MODEL_A), 0, "library A was pruned");
    assert_eq!(
        count_embeddings(&db_b, MODEL_A),
        1,
        "library B must be untouched"
    );
}
