use std::fs;
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
#[cfg(target_os = "macos")]
fn embed_produces_an_embeddings_row_for_a_real_video() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::copy("tests/fixtures/red_1s.mp4", scan_dir.path().join("clip.mp4")).unwrap();

    let scan_status = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");
    assert!(scan_status.success());

    let embed_status = Command::new(videre_bin())
        .arg("embed")
        .arg("--db")
        .arg(&db_path)
        .arg("--silent")
        .status()
        .expect("failed to run videre embed");
    assert!(embed_status.success());

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "the video's content hash should have an embeddings row");

    // Sanity-check the embedding itself, not just that a row exists - this is
    // the sole integration-level proof this feature works, so it's worth
    // catching an empty/garbage/wrong-dimension blob slipping through the
    // video decode path specifically. 1152-dim f16 = 2304 bytes (SigLIP
    // so400m/14-384's embedding size, see videre_core::embeddings::DEFAULT_MODEL_ID).
    let (model_id, blob_len): (String, i64) = conn
        .query_row(
            "SELECT model_id, LENGTH(embedding) FROM embeddings LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(model_id, "google/siglip-so400m-patch14-384");
    assert_eq!(blob_len, 2304, "expected a 1152-dim f16 embedding, got {blob_len} bytes");
}
