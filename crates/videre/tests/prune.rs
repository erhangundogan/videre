mod common;
use common::TestLibrary;
use std::path::PathBuf;

const MODEL: &str = videre_core::embeddings::DEFAULT_MODEL_ID;

/// A library with two real files and one phantom path (never created on disk),
/// all under the root. Returns (library, path_a, path_b, phantom_path).
fn fixture_library() -> (TestLibrary, PathBuf, PathBuf, String) {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let a = root.join("a.jpg");
    let b = root.join("b.jpg");
    std::fs::write(&a, b"img_a").unwrap();
    std::fs::write(&b, b"img_b").unwrap();
    let phantom = root.join("gone.jpg").to_string_lossy().into_owned();

    let conn = lib.init_db();
    for (path, hash) in [
        (a.to_string_lossy().into_owned(), "haaa"),
        (b.to_string_lossy().into_owned(), "hbbb"),
        (phantom.clone(), "hphantom"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, modified_at) VALUES (?1, ?2, '2020-01-01T00:00:00+00:00')",
            rusqlite::params![path, hash],
        )
        .unwrap();
    }
    (lib, a, b, phantom)
}

/// Seed embeddings into this library's per-model store.
fn add_embeddings(lib: &TestLibrary, hashes: &[&str]) {
    let conn = lib.conn();
    videre_core::embeddings_db::attach_in(&conn, &lib.context(), MODEL, true).unwrap();
    for hash in hashes {
        conn.execute(
            "INSERT OR IGNORE INTO emb.embeddings VALUES (?1, ?2, X'0000', 'now')",
            rusqlite::params![hash, MODEL],
        )
        .unwrap();
    }
}

fn row_exists(lib: &TestLibrary, path: &str) -> bool {
    lib.conn()
        .query_row(
            "SELECT COUNT(*) FROM file_hashes WHERE path = ?1",
            [path],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0
}

fn get_modified_at(lib: &TestLibrary, path: &str) -> Option<String> {
    lib.conn()
        .query_row(
            "SELECT modified_at FROM file_hashes WHERE path = ?1",
            [path],
            |r| r.get(0),
        )
        .ok()
}

fn embedding_exists(lib: &TestLibrary, hash: &str) -> bool {
    let path = videre_core::embeddings_db::db_path_in(&lib.context(), MODEL).unwrap();
    if !path.exists() {
        return false;
    }
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE hash = ?1",
        [hash],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

fn run_prune(lib: &TestLibrary, dry_run: bool) {
    let mut cmd = lib.cmd();
    cmd.arg("prune").arg("--silent");
    if dry_run {
        cmd.arg("--dry-run");
    }
    assert!(cmd.status().expect("failed to run videre prune").success());
}

#[test]
fn pruning_a_does_not_remove_b_models_or_cache() {
    let a = TestLibrary::new();
    let b = TestLibrary::new();
    a.copy_fixture("tiny.jpg", "gone.jpg");
    a.scan();
    b.copy_fixture("tiny.jpg", "keep.jpg");
    b.scan();

    let cb = b.context();
    std::fs::create_dir_all(&cb.cache.thumbnails).unwrap();
    let cache = cb
        .cache
        .thumbnails
        .join(format!("{}_240.jpg", "b".repeat(64)));
    std::fs::write(&cache, b"untouched").unwrap();
    let before = common::feature_fixture::snapshot_database(&b.db());

    std::fs::remove_file(a.root.join("gone.jpg")).unwrap();
    let out = a.cmd().arg("prune").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(common::feature_fixture::snapshot_database(&b.db()), before);
    assert_eq!(std::fs::read(&cache).unwrap(), b"untouched");
}

#[test]
fn missing_default_db_prints_friendly_error() {
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["prune", "--dry-run"])
        .output()
        .expect("failed to run videre prune");
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not initialized"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn removes_row_for_missing_file() {
    let (lib, _, _, phantom) = fixture_library();
    assert!(row_exists(&lib, &phantom));
    run_prune(&lib, false);
    assert!(!row_exists(&lib, &phantom), "phantom row should be removed");
}

#[test]
fn preserves_rows_for_existing_files() {
    let (lib, a, b, _) = fixture_library();
    run_prune(&lib, false);
    assert!(
        row_exists(&lib, a.to_str().unwrap()),
        "a.jpg should be kept"
    );
    assert!(
        row_exists(&lib, b.to_str().unwrap()),
        "b.jpg should be kept"
    );
}

#[test]
fn syncs_modified_at_for_existing_files() {
    let (lib, a, _, _) = fixture_library();
    run_prune(&lib, false);
    let new_val = get_modified_at(&lib, a.to_str().unwrap()).unwrap();
    assert_ne!(
        new_val, "2020-01-01T00:00:00+00:00",
        "modified_at should be refreshed"
    );
}

#[test]
fn dry_run_makes_no_changes() {
    let (lib, a, _, phantom) = fixture_library();
    let original_mtime = get_modified_at(&lib, a.to_str().unwrap());
    run_prune(&lib, true);
    assert!(
        row_exists(&lib, &phantom),
        "dry-run must not remove phantom row"
    );
    assert_eq!(
        get_modified_at(&lib, a.to_str().unwrap()),
        original_mtime,
        "dry-run must not update modified_at"
    );
}

#[test]
fn removes_orphan_embeddings_after_pruning() {
    let (lib, _, _, _) = fixture_library();
    add_embeddings(&lib, &["hphantom", "haaa"]);
    run_prune(&lib, false);
    assert!(
        !embedding_exists(&lib, "hphantom"),
        "orphan embedding should be removed"
    );
    assert!(
        embedding_exists(&lib, "haaa"),
        "embedding for surviving file should be kept"
    );
}

#[test]
fn preserves_embedding_when_hash_shared_with_surviving_file() {
    let (lib, _, _, _) = fixture_library();
    lib.conn()
        .execute(
            "UPDATE file_hashes SET hash = 'haaa' WHERE path LIKE '%gone%'",
            [],
        )
        .unwrap();
    add_embeddings(&lib, &["haaa"]);
    run_prune(&lib, false);
    let gone = lib.context().paths.root.join("gone.jpg");
    assert!(!row_exists(&lib, gone.to_str().unwrap()));
    assert!(
        embedding_exists(&lib, "haaa"),
        "shared-hash embedding must not be pruned"
    );
}

/// A real BLAKE3-length (64 hex char) hash, so cache filenames parse.
fn cache_test_hash(seed: char) -> String {
    seed.to_string().repeat(64)
}

#[test]
fn removes_orphan_cache_files_after_pruning() {
    let (lib, _, _, _) = fixture_library();
    let live_hash = cache_test_hash('1');
    let orphan_hash = cache_test_hash('9');

    lib.conn()
        .execute(
            "UPDATE file_hashes SET hash = ?1 WHERE path LIKE '%a.jpg'",
            [live_hash.as_str()],
        )
        .unwrap();

    let cache_dir = lib.context().cache.thumbnails;
    std::fs::create_dir_all(&cache_dir).unwrap();
    let live_thumb = cache_dir.join(format!("{live_hash}_240.jpg"));
    let orphan_thumb = cache_dir.join(format!("{orphan_hash}_240.jpg"));
    let orphan_original = cache_dir.join(format!("{orphan_hash}_original.jpg"));
    let orphan_tmp = cache_dir.join(format!("{orphan_hash}_original.tmp1234"));
    for f in [&live_thumb, &orphan_thumb, &orphan_original, &orphan_tmp] {
        std::fs::write(f, b"x").unwrap();
    }

    run_prune(&lib, false);

    assert!(
        live_thumb.exists(),
        "cache entry for a surviving hash must be kept"
    );
    assert!(!orphan_thumb.exists(), "orphan thumbnail must be removed");
    assert!(
        !orphan_original.exists(),
        "orphan original-cache entry must be removed"
    );
    assert!(
        orphan_tmp.exists(),
        "in-flight .tmp files must never be touched by prune"
    );
}

#[test]
fn dry_run_does_not_remove_orphan_cache_files() {
    let (lib, _, _, _) = fixture_library();
    let orphan_hash = cache_test_hash('9');
    let cache_dir = lib.context().cache.thumbnails;
    std::fs::create_dir_all(&cache_dir).unwrap();
    let orphan_thumb = cache_dir.join(format!("{orphan_hash}_240.jpg"));
    std::fs::write(&orphan_thumb, b"x").unwrap();

    run_prune(&lib, true);
    assert!(
        orphan_thumb.exists(),
        "dry-run must not delete any cache file"
    );
}

/// The regression test for the unmounted-volume bug: an absent directory keeps
/// its rows and their expensive embeddings.
#[test]
fn an_unreachable_directory_does_not_delete_rows_or_embeddings() {
    let lib = TestLibrary::new();
    let sub = lib.context().paths.root.join("on_the_drive");
    std::fs::create_dir_all(&sub).unwrap();
    let a = sub.join("a.jpg");
    let b = sub.join("b.jpg");
    std::fs::write(&a, b"img_a").unwrap();
    std::fs::write(&b, b"img_b").unwrap();

    let conn = lib.init_db();
    for (p, h) in [(&a, "hdrive_a"), (&b, "hdrive_b")] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, modified_at) VALUES (?1, ?2, '2020-01-01T00:00:00+00:00')",
            rusqlite::params![p.to_str().unwrap(), h],
        )
        .unwrap();
    }
    drop(conn);
    add_embeddings(&lib, &["hdrive_a", "hdrive_b"]);

    std::fs::remove_dir_all(&sub).unwrap();
    run_prune(&lib, false);

    assert!(
        row_exists(&lib, a.to_str().unwrap()),
        "row for an unreachable volume must survive"
    );
    assert!(row_exists(&lib, b.to_str().unwrap()), "ditto");
    assert!(
        embedding_exists(&lib, "hdrive_a"),
        "embedding must survive, hours to recompute"
    );
    assert!(embedding_exists(&lib, "hdrive_b"), "ditto");
}

#[test]
fn a_deleted_file_in_a_present_directory_is_still_pruned() {
    let (lib, a, _, _) = fixture_library();
    std::fs::remove_file(&a).unwrap();
    run_prune(&lib, false);
    assert!(
        !row_exists(&lib, a.to_str().unwrap()),
        "a genuinely deleted file must still be pruned"
    );
}

#[test]
fn prune_unreachable_removes_what_the_guard_skipped() {
    let lib = TestLibrary::new();
    let sub = lib.context().paths.root.join("gone_for_good");
    std::fs::create_dir_all(&sub).unwrap();
    let a = sub.join("a.jpg");
    std::fs::write(&a, b"img").unwrap();

    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash) VALUES (?1, 'hgone')",
            [a.to_str().unwrap()],
        )
        .unwrap();

    std::fs::remove_dir_all(&sub).unwrap();

    run_prune(&lib, false);
    assert!(row_exists(&lib, a.to_str().unwrap()), "kept by default");

    let status = lib
        .cmd()
        .args(["prune", "--silent", "--prune-unreachable"])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        !row_exists(&lib, a.to_str().unwrap()),
        "--prune-unreachable must remove it"
    );
}

#[test]
fn the_bulk_guard_does_not_block_a_small_library() {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    for i in 0..5 {
        let p = root.join(format!("f{i}.jpg"));
        if i < 2 {
            std::fs::write(&p, b"x").unwrap();
        }
        conn.execute(
            "INSERT INTO file_hashes (path, hash) VALUES (?1, ?2)",
            rusqlite::params![p.to_str().unwrap(), format!("h{i}")],
        )
        .unwrap();
    }
    drop(conn);

    run_prune(&lib, false);

    let left: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 2, "60% removal under the row floor must still prune");
}

/// The summary's synced count from a non-silent run.
fn prune_synced_count(lib: &TestLibrary, dry_run: bool) -> usize {
    let mut cmd = lib.cmd();
    cmd.arg("prune");
    if dry_run {
        cmd.arg("--dry-run");
    }
    let out = cmd.output().expect("failed to run videre prune");
    assert!(out.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let line = text
        .lines()
        .find(|l| l.contains("row(s) checked"))
        .unwrap_or_else(|| panic!("no summary line in prune output:\n{text}"))
        .to_string();
    let (before, _) = line
        .as_str()
        .split_once(" synced")
        .expect("summary names a sync count");
    before
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("could not read a sync count from: {line}"))
}

#[test]
fn prune_syncs_nothing_on_a_second_pass() {
    let (lib, a, _, _) = fixture_library();

    let first = prune_synced_count(&lib, false);
    assert!(
        first > 0,
        "the fixture's stale timestamps should give the first pass work"
    );

    assert_eq!(
        prune_synced_count(&lib, false),
        0,
        "an unchanged pass must sync nothing"
    );
    assert_eq!(
        prune_synced_count(&lib, true),
        0,
        "a dry run must agree there is no work"
    );

    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&a, b"img_a_changed").unwrap();
    assert_eq!(
        prune_synced_count(&lib, false),
        1,
        "exactly the touched file should sync"
    );
    assert_eq!(
        prune_synced_count(&lib, false),
        0,
        "and it must converge again afterwards"
    );
}
