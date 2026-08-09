mod common;
use common::{shared_cache_guard, videre_bin};
use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[test]
#[cfg(target_os = "macos")]
fn embed_produces_an_embeddings_row_for_a_real_video() {
    let _serial = shared_cache_guard();
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::copy(
        "tests/fixtures/red_1s.mp4",
        scan_dir.path().join("clip.mp4"),
    )
    .unwrap();

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

    // Embeddings live in a per-model database under VIDERE_HOME now, not in
    // the main library file, so open that instead of `db_path`.
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    videre_core::embeddings_db::attach(
        &conn,
        &db_path,
        videre_core::embeddings::DEFAULT_MODEL_ID,
        false,
    )
    .expect("videre embed should have created the model database");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM emb.embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "the video's content hash should have an embeddings row"
    );

    // Sanity-check the embedding itself, not just that a row exists, this is
    // the sole integration-level proof this feature works, so it's worth
    // catching an empty/garbage/wrong-dimension blob slipping through the
    // video decode path specifically. Asserted against DEFAULT_MODEL_ID rather
    // than a hardcoded id/size so switching models doesn't fail this test for
    // the wrong reason; the point is "a full-width embedding was written", not
    // "it was this particular model".
    let (model_id, blob_len): (String, i64) = conn
        .query_row(
            "SELECT model_id, LENGTH(embedding) FROM emb.embeddings LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(model_id, videre_core::embeddings::DEFAULT_MODEL_ID);
    assert!(
        blob_len >= 1024 && blob_len % 2 == 0,
        "expected a plausible f16 embedding blob (2 bytes per dim), got {blob_len} bytes"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn embed_skips_an_audio_only_video_without_calling_quicklook() {
    let _serial = shared_cache_guard();
    // Regression guard for a 20s-per-run cost: qlmanage hangs rather than
    // failing on a container with no video track, and nothing marks the file
    // permanently unembeddable, so the wait recurs on every run.
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");
    fs::copy(
        "tests/fixtures/audio_only.mov",
        scan_dir.path().join("audio_only.mov"),
    )
    .unwrap();

    let scan = Command::new(videre_bin())
        .args(["scan", "--silent", "--output-sqlite"])
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre scan");
    assert!(scan.success());

    let started = std::time::Instant::now();
    let out = Command::new(videre_bin())
        .args(["embed", "--db"])
        .arg(&db_path)
        .output()
        .expect("failed to run videre embed");
    let elapsed = started.elapsed();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no video track"),
        "expected an explicit skip reason, got: {stderr}"
    );
    // Generous: the point is that it does not sit through the 20s qlmanage
    // timeout. Model loading dominates the rest, so this is not a tight bound.
    assert!(
        elapsed < std::time::Duration::from_secs(300),
        "embed took {elapsed:?}, suspiciously close to a qlmanage timeout"
    );
}
