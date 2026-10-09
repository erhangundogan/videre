mod common;
use common::TestLibrary;

/// Seed one file_hashes row (and optional extra SQL) with an in-root path.
fn seed(lib: &TestLibrary, columns: &str, values_tail: &str) {
    let path = lib.context().paths.root.join("a.jpg");
    lib.init_db()
        .execute(
            &format!("INSERT INTO file_hashes (path, {columns}) VALUES (?1, {values_tail})"),
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();
}

/// One bounded pass: VIDERE_WATCH_ONCE makes `watch` run the startup
/// reconcile and exit 0, so every per-stage behaviour is testable by
/// running the process to completion instead of spawn-sleep-kill.
fn watch_once(lib: &TestLibrary, flags: &[&str]) -> std::process::Output {
    let out = lib
        .cmd()
        .arg("watch")
        .arg("--silent")
        .args(flags)
        .env("VIDERE_WATCH_ONCE", "1")
        .output()
        .expect("run videre watch");
    assert!(
        out.status.success(),
        "watch (once) failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn scan_stage_populates_file_hashes() {
    let lib = TestLibrary::new();
    std::fs::create_dir(lib.root.join("pics")).unwrap();
    std::fs::write(lib.root.join("pics/a.jpg"), b"dummy-bytes").unwrap();

    watch_once(&lib, &["--scan"]);

    let count: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "the scan stage should have inserted the one file");
}

#[test]
fn faces_stage_skips_hashes_already_processed() {
    let lib = TestLibrary::new();
    seed(&lib, "hash, ext", "'h1', 'jpg'");
    lib.conn()
        .execute(
            "INSERT INTO faces (hash, bbox, embedding) VALUES ('h1', '0,0,10,10', X'0000')",
            [],
        )
        .unwrap();

    // Success is the assertion: a crash on the already-processed hash fails
    // the once-run.
    watch_once(&lib, &["--faces"]);
}

#[test]
fn heic_stage_writes_no_cache_file_for_non_heic_hashes() {
    let lib = TestLibrary::new();
    seed(&lib, "hash, ext", "'hjpg', 'jpg'");

    watch_once(&lib, &["--heic"]);

    assert!(
        !videre_core::thumb_cache::thumb_exists_in(&lib.context().cache, "hjpg", 240),
        "non-HEIC hash must not get a cached thumbnail"
    );
}

#[test]
fn faces_stage_against_fresh_database_does_not_crash_or_hang() {
    // The library is initialized (watch initializes on startup) but has no
    // scanned rows: the faces stage must find nothing and exit cleanly.
    let lib = TestLibrary::new();
    watch_once(&lib, &["--faces"]);
}

#[test]
fn location_stage_populates_location_name_for_gps_rows() {
    let lib = TestLibrary::new();
    seed(
        &lib,
        "hash, ext, gps_lat, gps_lon",
        "'hparis', 'jpg', 48.8566, 2.3522",
    );

    watch_once(&lib, &["--location"]);

    let name: Option<String> = lib
        .conn()
        .query_row(
            "SELECT location_name FROM file_hashes WHERE hash = 'hparis'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        name.is_some(),
        "the location stage should have resolved a name"
    );
}

#[test]
fn location_stage_writes_a_terminal_location_names_pipeline_row() {
    // The stage resolves names (work that used to happen invisibly); now it
    // must also record itself in pipeline_runs so stats/status stop claiming
    // it never ran. The label is `location-names`, deliberately distinct from
    // `locations`, whose row means the standalone clustering recompute.
    let lib = TestLibrary::new();
    seed(
        &lib,
        "hash, ext, gps_lat, gps_lon",
        "'hparis', 'jpg', 48.8566, 2.3522",
    );

    watch_once(&lib, &["--location"]);

    let status: String = lib
        .conn()
        .query_row(
            "SELECT status FROM pipeline_runs WHERE command = 'location-names'",
            [],
            |r| r.get(0),
        )
        .expect("the location stage must write a location-names run row");
    assert_eq!(
        status, "success",
        "the stage must finish cleanly, not just record any terminal row"
    );
}

#[test]
fn prune_stage_removes_stale_rows() {
    let lib = TestLibrary::new();
    seed(&lib, "hash, ext", "'hgone', 'jpg'"); // a.jpg is never created on disk

    watch_once(&lib, &["--prune"]);

    let count: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "the --prune stage should remove the row for a missing file"
    );
}

#[test]
fn watch_prune_uses_same_face_sweep() {
    let lib = TestLibrary::new();
    seed(&lib, "hash, ext", "'hgone', 'jpg'");
    let conn = lib.conn();
    conn.execute(
        "INSERT INTO faces(hash,bbox,embedding) VALUES('hgone','0,0,1,1',X'00')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO faces_scanned(hash) VALUES('hgone')", [])
        .unwrap();
    drop(conn);
    watch_once(&lib, &["--prune"]);
    let conn = lib.conn();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM faces", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM faces_scanned", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn default_stages_include_prune_and_no_prune_turns_it_off() {
    let rows = |lib: &TestLibrary| -> i64 {
        lib.conn()
            .query_row(
                "SELECT COUNT(*) FROM file_hashes WHERE hash = 'hgone'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };

    let lib = TestLibrary::new();
    seed(&lib, "hash, ext", "'hgone', 'jpg'"); // a.jpg does not exist on disk
    watch_once(&lib, &["--no-prune"]);
    assert_eq!(rows(&lib), 1, "--no-prune keeps the missing file's row");

    // No stage flags: every stage, prune included, so a file deleted outside
    // videre leaves the library without a manual prune.
    watch_once(&lib, &[]);
    assert_eq!(rows(&lib), 0, "a default pass prunes");
}

#[test]
fn bare_watch_scan_creates_and_populates_the_library_database() {
    let lib = TestLibrary::new();
    std::fs::create_dir(lib.root.join("pics")).unwrap();
    std::fs::write(lib.root.join("pics/a.jpg"), b"dummy-bytes").unwrap();

    watch_once(&lib, &["--scan"]);

    assert!(
        lib.db().exists(),
        "watch --scan must create the library database"
    );
    let count: i64 = lib
        .conn()
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "the scan stage should have inserted the one file");
}
