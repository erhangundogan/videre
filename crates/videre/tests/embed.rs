mod common;
use common::TestLibrary;
// The model-backed helpers are only used by the macOS-gated embed tests below.
#[cfg(target_os = "macos")]
use common::{model_test_support::skip_unless_model_tests_enabled, shared_cache_guard};
use videre_core::decode_failures;

/// A hash the embed decode has already failed on FAILURE_THRESHOLD times is
/// dropped from the pending set, so `videre embed` never re-attempts it. Because
/// that is the only file, the pending set is empty and embed loads no model,
/// which is why this test needs no weights and is not macOS-gated: it proves the
/// wiring (the skip), not the embedding. It also asserts the stronger half of
/// that wiring: a run with nothing to do must not create the model database
/// either, so a failed or unnecessary embed never leaves an empty store for
/// `stats` to list.
#[test]
fn embed_skips_a_hash_recorded_as_failed_and_loads_no_model() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photo.jpg");
    lib.scan();

    // The scanned file's content hash, then record two decode failures for it
    // (FAILURE_THRESHOLD), the state a genuinely-undecodable file reaches.
    {
        let conn = lib.conn();
        let hash: String = conn
            .query_row("SELECT hash FROM file_hashes LIMIT 1", [], |r| r.get(0))
            .expect("the scanned photo has a hash");
        decode_failures::ensure_table(&conn).unwrap();
        decode_failures::record(&conn, &hash, decode_failures::STAGE_EMBED, "timed out").unwrap();
        decode_failures::record(&conn, &hash, decode_failures::STAGE_EMBED, "timed out").unwrap();
    }

    let embed = lib
        .cmd()
        .args(["embed", "--silent"])
        .status()
        .expect("failed to run videre embed");
    assert!(
        embed.success(),
        "embed should exit cleanly with nothing to do"
    );

    // Nothing was embedded, and a never-embedded library has no store at all:
    // creating one for a run that wrote nothing is the bug this asserts gone.
    let store_path = videre_core::embeddings_db::db_path_in(
        &lib.context(),
        videre_core::embeddings::DEFAULT_MODEL_ID,
    )
    .unwrap();
    assert!(
        !store_path.exists(),
        "a zero-work embed must not create the model database"
    );
}

/// `videre embed --reprocess` is the retry hatch: it clears this stage's
/// recorded decode failures so a previously-skipped file is eligible again.
///
/// macOS-gated with the model guard because `--reprocess` re-embeds every
/// eligible file, so it loads SigLIP; running it unguarded would download
/// weights on a cold Linux CI cache, which the suite forbids.
#[test]
#[cfg(target_os = "macos")]
fn embed_reprocess_clears_recorded_decode_failures() {
    if skip_unless_model_tests_enabled("embed reprocess") {
        return;
    }
    let _serial = shared_cache_guard();
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photo.jpg");
    lib.scan();

    let hash: String = {
        let conn = lib.conn();
        let hash: String = conn
            .query_row("SELECT hash FROM file_hashes LIMIT 1", [], |r| r.get(0))
            .unwrap();
        decode_failures::ensure_table(&conn).unwrap();
        decode_failures::record(&conn, &hash, decode_failures::STAGE_EMBED, "x").unwrap();
        decode_failures::record(&conn, &hash, decode_failures::STAGE_EMBED, "x").unwrap();
        hash
    };

    let embed = lib
        .model_cmd(&_serial)
        .args(["embed", "--reprocess", "--silent"])
        .status()
        .expect("failed to run videre embed --reprocess");
    assert!(embed.success());

    let conn = lib.conn();
    assert_eq!(
        decode_failures::fail_count(&conn, &hash, decode_failures::STAGE_EMBED).unwrap(),
        0,
        "--reprocess must clear the recorded decode failure for this stage"
    );
}

/// The record side, end to end: a file that decodes-fails on a real run gets a
/// strike each time, and once it reaches the two-strike threshold it stops being
/// attempted. Uses a corrupt JPEG (valid magic bytes so it is pending, no scan
/// data so the image crate cannot decode it), which fails on the CPU decode path
/// with no QuickLook involved.
///
/// macOS-gated with the model guard: the file is pending, so the run loads
/// SigLIP before the decode is attempted.
#[test]
#[cfg(target_os = "macos")]
fn embed_records_and_then_skips_a_repeatedly_undecodable_file() {
    if skip_unless_model_tests_enabled("embed decode failures") {
        return;
    }
    let _serial = shared_cache_guard();
    let lib = TestLibrary::new();
    lib.copy_fixture("corrupt.jpg", "broken.jpg");
    lib.scan();

    let hash: String = lib
        .conn()
        .query_row("SELECT hash FROM file_hashes LIMIT 1", [], |r| r.get(0))
        .expect("the corrupt file was scanned");

    let run = || {
        lib.model_cmd(&_serial)
            .args(["embed", "--silent"])
            .status()
            .expect("failed to run videre embed")
    };

    // First run: decode fails, one strike, still below the threshold so the file
    // is not yet skipped.
    assert!(run().success());
    assert_eq!(
        decode_failures::fail_count(&lib.conn(), &hash, decode_failures::STAGE_EMBED).unwrap(),
        1,
        "the first decode failure records one strike"
    );

    // Second run: fails again, reaching the two-strike threshold.
    assert!(run().success());
    assert_eq!(
        decode_failures::fail_count(&lib.conn(), &hash, decode_failures::STAGE_EMBED).unwrap(),
        2,
        "the second decode failure reaches the threshold"
    );

    // It was never embedded, and now that it is at the threshold it will be
    // skipped on every later run.
    let embedded: i64 = model_store(&lib)
        .query_row(
            "SELECT COUNT(*) FROM embeddings WHERE hash = ?1",
            [&hash],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(embedded, 0, "an undecodable file is never embedded");
}

/// Open the per-model embedding store for this library directly.
///
/// Only the macOS-gated tests below still open the store (the offline tests
/// assert its absence instead), so the helper carries the same gate; without
/// it the Linux build sees an unused function.
#[cfg(target_os = "macos")]
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
    if skip_unless_model_tests_enabled("embed video") {
        return;
    }
    let _serial = shared_cache_guard();
    let lib = TestLibrary::new();
    lib.copy_fixture("red_1s.mp4", "clip.mp4");
    lib.scan();

    let embed = lib
        .model_cmd(&_serial)
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
    if skip_unless_model_tests_enabled("embed audio-only video") {
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
        .model_cmd(&_serial)
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

/// A model id that passes the owner/name shape but cannot load must fail the
/// run and leave no model database behind: the store is only created once a
/// model has loaded and there is work to write. HF_ENDPOINT is pinned to the
/// discard port, where the connect fails immediately and retries burn about a
/// second; nothing is downloaded and no real cache is reachable because the
/// test library's private HF_HOME is empty.
#[test]
fn embed_with_a_model_that_cannot_load_leaves_no_model_database() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photo.jpg");
    lib.scan();

    let embed = lib
        .cmd()
        .args(["embed", "--model", "videre-tests/no-such-model", "--silent"])
        .env("HF_ENDPOINT", "http://127.0.0.1:9")
        .output()
        .expect("failed to run videre embed");
    assert!(
        !embed.status.success(),
        "a model that cannot load must fail the run"
    );

    let embeddings_dir = lib.root.join(".videre/embeddings");
    let left_behind = std::fs::read_dir(&embeddings_dir)
        .map(|entries| entries.filter_map(|e| e.ok()).count())
        .unwrap_or(0);
    assert_eq!(
        left_behind,
        0,
        "a failed load must leave no model database in {}",
        embeddings_dir.display()
    );

    // `stats` reads its model list straight from the directory, so the store's
    // absence is only confirmed when the listing has no models either.
    let stats = lib.cmd().arg("stats").output().unwrap();
    assert!(stats.status.success(), "stats runs cleanly");
    let stdout = String::from_utf8_lossy(&stats.stdout);
    assert!(
        stdout.contains("none; run"),
        "stats must list no models after the failed embed:\n{stdout}"
    );
}

/// A store that already exists is not embed's to remove: a zero-work run must
/// leave it exactly as it was. Removing empty stores is prune's job, and this
/// test pins that boundary so cleanup never drifts into embed.
#[test]
fn embed_with_zero_work_keeps_an_existing_model_database() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photo.jpg");
    lib.scan();

    // The store as a previous successful embed would have left it: schema
    // only, no rows.
    let ctx = lib.context();
    {
        let conn = lib.conn();
        videre_core::embeddings_db::attach_in(
            &conn,
            &ctx,
            videre_core::embeddings::DEFAULT_MODEL_ID,
            true,
        )
        .unwrap();
        videre_core::embeddings_db::detach(&conn).unwrap();
    }
    let store_path =
        videre_core::embeddings_db::db_path_in(&ctx, videre_core::embeddings::DEFAULT_MODEL_ID)
            .unwrap();
    assert!(store_path.exists(), "the test created the store");

    // Push the photo past the decode-failure threshold so the run has zero
    // work (the same trick as the test above).
    let hash: String = {
        let conn = lib.conn();
        conn.query_row("SELECT hash FROM file_hashes LIMIT 1", [], |r| r.get(0))
            .expect("the scanned photo has a hash")
    };
    {
        let conn = lib.conn();
        decode_failures::ensure_table(&conn).unwrap();
        decode_failures::record(&conn, &hash, decode_failures::STAGE_EMBED, "timed out").unwrap();
        decode_failures::record(&conn, &hash, decode_failures::STAGE_EMBED, "timed out").unwrap();
    }

    let embed = lib
        .cmd()
        .args(["embed", "--silent"])
        .status()
        .expect("failed to run videre embed");
    assert!(
        embed.success(),
        "embed should exit cleanly with nothing to do"
    );
    assert!(
        store_path.exists(),
        "a zero-work embed must not remove an existing model database"
    );
}

/// An already-embedded file without a fingerprint gets one from a decode-only
/// pass. The model is never loaded: the child's private HF cache stays absent,
/// and in CI a load would also fail against the closed HF_ENDPOINT. Model-free,
/// so it uses `cmd()` and runs on every platform without the opt-in.
#[test]
fn embed_backfills_a_missing_fingerprint_without_loading_the_model() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photo.jpg");
    lib.scan();
    {
        let conn = lib.conn();
        let hash: String = conn
            .query_row("SELECT hash FROM file_hashes LIMIT 1", [], |r| r.get(0))
            .unwrap();
        videre_core::embeddings_db::attach_in(
            &conn,
            &lib.context(),
            videre_core::embeddings::DEFAULT_MODEL_ID,
            true,
        )
        .unwrap();
        videre_core::embeddings::insert_embeddings(
            &conn,
            videre_core::embeddings::DEFAULT_MODEL_ID,
            &[(hash, vec![0u8; 4])],
        )
        .unwrap();
    }

    let out = lib.cmd().arg("embed").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let phash: Option<i64> = lib
        .conn()
        .query_row("SELECT phash FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert!(
        phash.is_some(),
        "the decode-only pass must store a fingerprint"
    );
    assert!(
        !lib.home.join(".cache/huggingface").exists(),
        "a fingerprint-only run must not load the model"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn embed_stores_a_fingerprint_with_each_embedding() {
    if skip_unless_model_tests_enabled("embed fingerprint") {
        return;
    }
    let guard = shared_cache_guard();
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photo.jpg");
    lib.scan();
    assert!(lib
        .model_cmd(&guard)
        .args(["embed", "--silent"])
        .status()
        .unwrap()
        .success());
    let phash: Option<i64> = lib
        .conn()
        .query_row("SELECT phash FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert!(phash.is_some());
}
