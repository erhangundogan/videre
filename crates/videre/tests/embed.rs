mod common;
use common::{shared_cache_guard, siglip_cached, skip_without_models, TestLibrary};

/// Open the per-model embedding store for this library directly.
fn model_store(lib: &TestLibrary) -> rusqlite::Connection {
    let path = videre_core::embeddings_db::db_path_in(
        &lib.context(),
        videre_core::embeddings::DEFAULT_MODEL_ID,
    )
    .unwrap();
    rusqlite::Connection::open(path).expect("videre embed should have created the model database")
}

#[test]
#[cfg(target_os = "macos")]
fn embed_produces_an_embeddings_row_for_a_real_video() {
    if skip_without_models("embed", siglip_cached()) {
        return;
    }
    let _serial = shared_cache_guard();
    let lib = TestLibrary::new();
    lib.copy_fixture("red_1s.mp4", "clip.mp4");
    lib.scan();

    let embed = lib
        .cmd()
        .args(["embed", "--silent"])
        .status()
        .expect("failed to run videre embed");
    assert!(embed.success());

    let conn = model_store(&lib);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 1,
        "the video's content hash should have an embeddings row"
    );

    // Sanity-check the embedding itself, not just that a row exists. Asserted
    // against DEFAULT_MODEL_ID rather than a hardcoded id/size so switching
    // models doesn't fail this test for the wrong reason.
    let (model_id, blob_len): (String, i64) = conn
        .query_row(
            "SELECT model_id, LENGTH(embedding) FROM embeddings LIMIT 1",
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
    if skip_without_models("embed", siglip_cached()) {
        return;
    }
    let _serial = shared_cache_guard();
    // Regression guard for a 20s-per-run cost: qlmanage hangs rather than
    // failing on a container with no video track, and nothing marks the file
    // permanently unembeddable, so the wait recurs on every run.
    let lib = TestLibrary::new();
    lib.copy_fixture("audio_only.mov", "audio_only.mov");
    lib.scan();

    let started = std::time::Instant::now();
    let out = lib
        .cmd()
        .arg("embed")
        .output()
        .expect("failed to run videre embed");
    let elapsed = started.elapsed();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no video track"),
        "expected an explicit skip reason, got: {stderr}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(300),
        "embed took {elapsed:?}, suspiciously close to a qlmanage timeout"
    );
}
