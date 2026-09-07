//! End-to-end coverage for the per-model embedding split: several models in
//! one library, and several libraries not treading on each other.

mod common;
use common::TestLibrary;
use rusqlite::params;

const MODEL_A: &str = "google/siglip2-base-patch16-384";
const MODEL_B: &str = "google/siglip-base-patch16-224";

#[test]
fn model_stores_follow_the_library_not_the_launch_directory() {
    let a = common::TestLibrary::new();
    let b = common::TestLibrary::new();
    let ca = a.context();
    let cb = b.context();
    let conn = videre_core::library_db::initialize(&ca).unwrap();
    let model = videre_core::embeddings::DEFAULT_MODEL_ID;
    videre_core::embeddings_db::attach_in(&conn, &ca, model, true).unwrap();
    let pa = videre_core::embeddings_db::db_path_in(&ca, model).unwrap();
    let pb = videre_core::embeddings_db::db_path_in(&cb, model).unwrap();
    assert_eq!(
        pa,
        ca.paths
            .embeddings
            .join("google--siglip-base-patch16-224.db")
    );
    assert!(pa.exists());
    assert!(!pb.exists());
    assert_ne!(pa, pb);
}

#[test]
fn feature_fixture_seeds_divergent_libraries_isolated_by_root() {
    use common::feature_fixture::{snapshot_database, FeatureFixture, MODEL_A, MODEL_B};

    let fx = FeatureFixture::build();
    let a = snapshot_database(&fx.a.db());
    let b_before = snapshot_database(&fx.b.db());

    // The shared content hash carries different marks, tags and confirmed
    // people in each library.
    assert_ne!(a["marks"], b_before["marks"]);
    assert_ne!(a["photo_tags"], b_before["photo_tags"]);
    assert_ne!(a["people"], b_before["people"]);

    // A has two model stores with different counts; B has the one its config
    // chose.
    let a_ctx = fx.a.context();
    let b_ctx = fx.b.context();
    assert_eq!(
        videre_core::embeddings_db::list_models_in(&a_ctx).unwrap(),
        vec![MODEL_A.to_string(), MODEL_B.to_string()]
    );
    assert_eq!(
        videre_core::embeddings_db::list_models_in(&b_ctx).unwrap(),
        vec![MODEL_B.to_string()]
    );
    let a_counts = videre_core::embeddings_db::counts_by_model_in(&a_ctx).unwrap();
    assert_eq!(a_counts.iter().map(|c| c.count).sum::<i64>(), 3);

    // A mutation confined to A must leave B's logical state byte-for-byte equal.
    fx.a.conn()
        .execute("UPDATE marks SET rating = 1", [])
        .unwrap();
    let b_after = snapshot_database(&fx.b.db());
    assert_eq!(b_before, b_after);
}

/// A scanned library with one synthetic embedding in each of two models.
///
/// Synthetic vectors on purpose: this covers routing and cleanup, not
/// embedding quality, and loading real SigLIP weights would make it slow and
/// network-dependent. One real file is scanned so the row exists for prune to
/// reason about.
fn seeded_library() -> TestLibrary {
    let lib = TestLibrary::new();
    lib.copy_fixture("sample_with_exif.jpg", "a.jpg");
    lib.scan();

    let ctx = lib.context();
    let conn = lib.conn();
    let hash: String = conn
        .query_row("SELECT hash FROM file_hashes LIMIT 1", [], |r| r.get(0))
        .unwrap();
    for model in [MODEL_A, MODEL_B] {
        videre_core::embeddings_db::attach_in(&conn, &ctx, model, true).unwrap();
        conn.execute(
            "INSERT INTO emb.embeddings (hash, model_id, embedding, embedded_at)
             VALUES (?1, ?2, zeroblob(1536), '2026-08-05T00:00:00')",
            params![hash, model],
        )
        .unwrap();
        videre_core::embeddings_db::detach(&conn).unwrap();
    }
    lib
}

fn count_embeddings(lib: &TestLibrary, model: &str) -> i64 {
    let path = videre_core::embeddings_db::db_path_in(&lib.context(), model).unwrap();
    if !path.exists() {
        return 0;
    }
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .unwrap_or(0)
}

#[test]
fn two_models_coexist_in_one_library() {
    let lib = seeded_library();
    assert_eq!(count_embeddings(&lib, MODEL_A), 1);
    assert_eq!(count_embeddings(&lib, MODEL_B), 1);
    let ctx = lib.context();
    assert_ne!(
        videre_core::embeddings_db::db_path_in(&ctx, MODEL_A).unwrap(),
        videre_core::embeddings_db::db_path_in(&ctx, MODEL_B).unwrap(),
        "each model must own a distinct file"
    );
}

#[test]
fn stats_reports_every_model_separately() {
    let lib = seeded_library();
    let out = lib
        .cmd()
        .args(["stats", "--json"])
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

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
    let lib = seeded_library();
    let out = lib
        .cmd()
        .args(["search", "anything", "--model", "google/does-not-exist-384"])
        .output()
        .expect("failed to run videre search");

    assert!(
        !out.status.success(),
        "must exit non-zero rather than return zero hits"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no embeddings for google/does-not-exist-384"),
        "{stderr}"
    );
    assert!(
        stderr.contains(MODEL_A) && stderr.contains(MODEL_B),
        "the error must list what IS available: {stderr}"
    );
}

// The two prune tests below exercise `videre prune`, whose directory-local
// entry point lands in a later task; they stay red until then.
#[test]
fn prune_removes_orphans_from_every_model_database() {
    let lib = seeded_library();
    std::fs::remove_file(lib.root.join("a.jpg")).unwrap();
    let out = lib
        .cmd()
        .arg("prune")
        .output()
        .expect("failed to run videre prune");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    for model in [MODEL_A, MODEL_B] {
        assert_eq!(
            count_embeddings(&lib, model),
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
    let lib_a = seeded_library();
    let lib_b = seeded_library();

    assert_eq!(
        count_embeddings(&lib_b, MODEL_A),
        1,
        "library B must start with an embedding for this to prove anything"
    );

    std::fs::remove_file(lib_a.root.join("a.jpg")).unwrap();
    let out = lib_a
        .cmd()
        .arg("prune")
        .output()
        .expect("failed to run videre prune");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(count_embeddings(&lib_a, MODEL_A), 0, "library A was pruned");
    assert_eq!(
        count_embeddings(&lib_b, MODEL_A),
        1,
        "library B must be untouched"
    );
}
