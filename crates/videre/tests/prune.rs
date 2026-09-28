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

/// A schema-only per-model store, exactly what `videre embed` with a model
/// that failed to load used to leave behind.
fn add_empty_store(lib: &TestLibrary) -> std::path::PathBuf {
    let conn = lib.conn();
    videre_core::embeddings_db::attach_in(&conn, &lib.context(), MODEL, true).unwrap();
    videre_core::embeddings_db::detach(&conn).unwrap();
    videre_core::embeddings_db::db_path_in(&lib.context(), MODEL).unwrap()
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

fn seed_face(lib: &TestLibrary, hash: &str) -> i64 {
    let conn = lib.conn();
    conn.execute(
        "INSERT INTO people(name,full_name) VALUES('isil','Işıl') ON CONFLICT DO NOTHING",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO faces(hash,bbox,embedding,person_label,confirmed) VALUES(?1,'0,0,1,1',X'00','isil',1)", [hash]).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO faces_scanned(hash) VALUES(?1)",
        [hash],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[test]
fn prune_removes_orphan_faces_and_keeps_person_name() {
    let (lib, _, _, phantom) = fixture_library();
    let face_id = seed_face(&lib, "hphantom");
    let conn = lib.conn();
    conn.execute("INSERT INTO face_learning_events(id,action_kind,decision_kind,outcome,embedding_model_id,feature_schema_version,feature_snapshot_json,support_count)
        VALUES(1,'assign_face','membership','positive','test',1,'{}',0)", []).unwrap();
    conn.execute("INSERT INTO face_learning_event_faces(event_id,face_id,role,ordinal) VALUES(1,?1,'subject',0)", [face_id]).unwrap();
    drop(conn);
    let out = lib.cmd().arg("prune").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!row_exists(&lib, &phantom));
    let conn = lib.conn();
    assert!(videre_api::faces_list(&conn).unwrap().people.is_empty());
    let detail = videre_api::person_detail(&conn, "Işıl").unwrap();
    assert_eq!(detail.full_name, "Işıl");
    assert!(detail.faces.is_empty());
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM faces_scanned WHERE hash='hphantom'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT invalidation_reason FROM face_learning_events WHERE id=1",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "source_face_pruned"
    );
    let output = String::from_utf8_lossy(&out.stderr);
    assert!(output.contains("orphan face"), "{output}");
    assert!(!output.contains("Işıl"));
}

#[test]
fn prune_preserves_faces_while_a_duplicate_path_survives() {
    let (lib, a, _, phantom) = fixture_library();
    let conn = lib.conn();
    conn.execute(
        "UPDATE file_hashes SET hash='haaa' WHERE path=?1",
        [&phantom],
    )
    .unwrap();
    drop(conn);
    let face_id = seed_face(&lib, "haaa");
    // Learning that relied on the face, and a profile in use.
    let conn = lib.conn();
    conn.execute("INSERT INTO face_learning_events(id,action_kind,decision_kind,outcome,embedding_model_id,feature_schema_version,feature_snapshot_json,support_count)
        VALUES(1,'assign_face','membership','positive','test',1,'{}',0)", []).unwrap();
    conn.execute("INSERT INTO face_learning_event_faces(event_id,face_id,role,ordinal) VALUES(1,?1,'subject',0)", [face_id]).unwrap();
    conn.execute("INSERT INTO face_learning_profiles(id,artifact_version,embedding_model_id,feature_schema_version,model_kind,parameters,training_evidence_json,validation_report_json,stage,status)
        VALUES(5,1,'test',1,'logistic',X'00','{}','{}','suggestion','active')", []).unwrap();
    drop(conn);
    let learning = |lib: &TestLibrary| -> (i64, String) {
        lib.conn()
            .query_row(
                "SELECT (SELECT eligible FROM face_learning_events WHERE id=1),
                        (SELECT status FROM face_learning_profiles WHERE id=5)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };

    // One copy goes, one survives: faces, marker, evidence and profile stay.
    run_prune(&lib, false);
    assert_eq!(
        lib.conn()
            .query_row("SELECT count(*) FROM faces WHERE hash='haaa'", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        lib.conn()
            .query_row(
                "SELECT count(*) FROM faces_scanned WHERE hash='haaa'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(learning(&lib), (1, "active".to_owned()));

    // The last copy goes: so do the faces, and the evidence and profile with them.
    std::fs::remove_file(a).unwrap();
    run_prune(&lib, false);
    assert_eq!(
        lib.conn()
            .query_row("SELECT count(*) FROM faces WHERE hash='haaa'", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(learning(&lib), (0, "retired".to_owned()));
}

#[test]
fn dry_run_previews_face_lower_bound_without_writes() {
    let (lib, _, _, _) = fixture_library();
    // Orphaned only once the pending row deletion runs: not counted.
    seed_face(&lib, "hphantom");
    // Already orphaned (no indexed path): counted.
    seed_face(&lib, "hnopath");
    let db = lib.db();
    let before = common::feature_fixture::snapshot_database(&db);
    let out = lib.cmd().args(["prune", "--dry-run"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(common::feature_fixture::snapshot_database(&db), before);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("1 orphan face(s) (1 labeled)"), "{stderr}");
    assert!(stderr.contains("lower bound"), "{stderr}");
    assert!(!stderr.contains("retired"), "nothing was retired: {stderr}");

    let conn = lib.conn();
    conn.execute_batch("DROP TABLE faces;
        CREATE TABLE faces(id INTEGER PRIMARY KEY, hash TEXT NOT NULL, bbox TEXT NOT NULL,
        landmark TEXT, embedding BLOB NOT NULL, cluster_id INTEGER, person_label TEXT,
        confirmed INTEGER DEFAULT 0, is_primary INTEGER DEFAULT 0, det_score REAL, blur REAL, oriented INTEGER,
        FOREIGN KEY(person_label) REFERENCES people(name) ON DELETE RESTRICT ON UPDATE RESTRICT);
        PRAGMA user_version=3;
        PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    drop(conn);
    let before = std::fs::read(&db).unwrap();
    let out = lib.cmd().args(["prune", "--dry-run"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("videre scan"));
    assert_eq!(std::fs::read(&db).unwrap(), before);
}

#[test]
fn unreachable_and_bulk_guard_protect_faces() {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let gone_dir = root.join("unmounted");
    std::fs::create_dir(&gone_dir).unwrap();
    let unreachable = gone_dir.join("photo.jpg");
    let conn = lib.init_db();
    for id in 0..101 {
        let path = root.join(format!("missing-{id}.jpg"));
        conn.execute(
            "INSERT INTO file_hashes(path,hash) VALUES(?1,?2)",
            rusqlite::params![path.to_str().unwrap(), format!("h{id}")],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO file_hashes(path,hash) VALUES(?1,'protected')",
        [unreachable.to_str().unwrap()],
    )
    .unwrap();
    drop(conn);
    seed_face(&lib, "h0");
    seed_face(&lib, "protected");
    std::fs::remove_dir(&gone_dir).unwrap();
    let out = lib.cmd().args(["prune", "--silent"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        lib.conn()
            .query_row("SELECT count(*) FROM faces", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    let out = lib
        .cmd()
        .args(["prune", "--silent", "--force"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        lib.conn()
            .query_row("SELECT hash FROM faces", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "protected"
    );
    let out = lib
        .cmd()
        .args(["prune", "--silent", "--prune-unreachable"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        lib.conn()
            .query_row("SELECT count(*) FROM faces", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn failed_file_row_delete_protects_faces() {
    let (lib, _, _, phantom) = fixture_library();
    seed_face(&lib, "hphantom");
    lib.conn()
        .execute_batch(
            "CREATE TRIGGER abort_file_delete BEFORE DELETE ON file_hashes
        WHEN OLD.hash='hphantom' BEGIN SELECT RAISE(ABORT,'injected delete failure'); END;",
        )
        .unwrap();
    let out = lib.cmd().args(["prune", "--silent"]).output().unwrap();
    assert!(!out.status.success());
    assert!(row_exists(&lib, &phantom));
    assert_eq!(
        lib.conn()
            .query_row(
                "SELECT count(*) FROM faces WHERE hash='hphantom'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn prune_cleanup_error_exits_nonzero_then_recovers() {
    let (lib, _, _, phantom) = fixture_library();
    let face_id = seed_face(&lib, "hphantom");
    let conn = lib.conn();
    conn.execute("INSERT INTO face_learning_events(id,action_kind,decision_kind,outcome,embedding_model_id,feature_schema_version,feature_snapshot_json,support_count)
        VALUES(1,'assign_face','membership','positive','test',1,'{}',0)", []).unwrap();
    conn.execute("INSERT INTO face_learning_event_faces(event_id,face_id,role,ordinal) VALUES(1,?1,'subject',0)", [face_id]).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER abort_learning_change BEFORE UPDATE ON face_learning_state
        BEGIN SELECT RAISE(ABORT,'injected learning failure'); END;",
    )
    .unwrap();
    drop(conn);
    let out = lib.cmd().args(["prune", "--silent"]).output().unwrap();
    assert!(!out.status.success());
    assert!(!row_exists(&lib, &phantom));
    let conn = lib.conn();
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM faces WHERE hash='hphantom'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT eligible FROM face_learning_events", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    conn.execute_batch("DROP TRIGGER abort_learning_change")
        .unwrap();
    drop(conn);
    let out = lib.cmd().args(["prune", "--silent"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        lib.conn()
            .query_row(
                "SELECT count(*) FROM faces WHERE hash='hphantom'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
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

#[test]
fn prune_removes_a_schema_only_model_database() {
    let (lib, _a, _b, _phantom) = fixture_library();
    let store = add_empty_store(&lib);

    run_prune(&lib, false);

    assert!(!store.exists(), "an empty model database must be removed");
    // `stats` reads models straight from the directory, so removal is only
    // proven done when the listing shrinks too.
    let out = lib.cmd().arg("stats").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("none; run"),
        "stats must list no models after the sweep:\n{stdout}"
    );
    assert!(
        !stdout.contains(MODEL),
        "the removed model must not be listed:\n{stdout}"
    );
}

#[test]
fn prune_keeps_a_model_database_that_still_has_rows() {
    let (lib, _a, _b, _phantom) = fixture_library();
    // 'haaa' is a live file_hashes row in the fixture, so the embedding is
    // not an orphan and the sweep cannot empty the store.
    add_embeddings(&lib, &["haaa"]);
    let store = videre_core::embeddings_db::db_path_in(&lib.context(), MODEL).unwrap();

    run_prune(&lib, false);

    assert!(
        store.exists(),
        "a model database with rows must survive prune"
    );
    assert!(
        embedding_exists(&lib, "haaa"),
        "the live embedding must survive"
    );
}

#[test]
fn prune_leaves_a_corrupt_model_database_alone() {
    let (lib, _a, _b, _phantom) = fixture_library();
    let store = add_empty_store(&lib);
    std::fs::write(&store, b"definitely not a sqlite database").unwrap();

    let mut cmd = lib.cmd();
    cmd.args(["prune", "--silent"]);
    assert!(cmd.status().expect("failed to run videre prune").success());

    // A database prune cannot read is not provably empty, so it is never
    // deleted.
    assert!(
        store.exists(),
        "a corrupt model database must be left in place"
    );
}

#[test]
fn prune_dry_run_reports_empty_model_databases_without_removing_them() {
    let (lib, _a, _b, _phantom) = fixture_library();
    let store = add_empty_store(&lib);

    // Without --silent so the report is actually produced; both streams are
    // searched because the process log's stream is a subscriber detail.
    let out = lib.cmd().args(["prune", "--dry-run"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        printed.contains(MODEL),
        "the dry run must name the empty model database:\n{printed}"
    );

    assert!(store.exists(), "a dry run must not remove the database");
}
