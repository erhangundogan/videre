mod common;
use common::TestLibrary;
use std::time::Duration;

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

/// Spawn `videre watch` in this library with the given stage flags and a long
/// interval (so only one cycle is observed), run it briefly, then kill it.
fn run_one_cycle(lib: &TestLibrary, flags: &[&str], millis: u64) -> bool {
    let mut child = lib
        .cmd()
        .arg("watch")
        .args(flags)
        .args(["--interval", "3600", "--silent"])
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(Duration::from_millis(millis));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().ok();
    child.wait().ok();
    still_running
}

#[test]
fn scan_stage_populates_file_hashes() {
    let lib = TestLibrary::new();
    std::fs::create_dir(lib.root.join("pics")).unwrap();
    std::fs::write(lib.root.join("pics/a.jpg"), b"dummy-bytes").unwrap();

    run_one_cycle(&lib, &["--scan"], 1500);

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

    let still_running = run_one_cycle(&lib, &["--faces"], 800);
    assert!(
        still_running,
        "watch --faces must not crash on an already-processed hash"
    );
}

#[test]
fn heic_stage_writes_no_cache_file_for_non_heic_hashes() {
    let lib = TestLibrary::new();
    seed(&lib, "hash, ext", "'hjpg', 'jpg'");

    run_one_cycle(&lib, &["--heic"], 800);

    assert!(
        !videre_core::thumb_cache::thumb_exists_in(&lib.context().cache, "hjpg", 240),
        "non-HEIC hash must not get a cached thumbnail"
    );
}

#[test]
fn faces_stage_against_fresh_database_does_not_crash_or_hang() {
    // The library is initialized (watch initializes on startup) but has no
    // scanned rows: the faces stage must find nothing and keep serving.
    let lib = TestLibrary::new();
    let still_running = run_one_cycle(&lib, &["--faces"], 800);
    assert!(
        still_running,
        "watch --faces against a fresh database must not crash"
    );
}

#[test]
fn location_stage_populates_location_name_for_gps_rows() {
    let lib = TestLibrary::new();
    seed(
        &lib,
        "hash, ext, gps_lat, gps_lon",
        "'hparis', 'jpg', 48.8566, 2.3522",
    );

    run_one_cycle(&lib, &["--location"], 3000);

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
fn prune_stage_removes_stale_rows() {
    let lib = TestLibrary::new();
    seed(&lib, "hash, ext", "'hgone', 'jpg'"); // a.jpg is never created on disk

    run_one_cycle(&lib, &["--prune"], 1500);

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
fn default_stages_do_not_include_prune() {
    let lib = TestLibrary::new();
    seed(&lib, "hash, ext", "'hgone', 'jpg'"); // a.jpg does not exist on disk

    // No stage flags: scan/faces/heic/location default on, but prune stays
    // opt-in, so the missing file's row must survive a default cycle.
    run_one_cycle(&lib, &[], 1500);

    let count: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM file_hashes WHERE hash = 'hgone'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "prune must not run unless --prune is passed explicitly"
    );
}

#[test]
fn bare_watch_scan_creates_and_populates_the_library_database() {
    let lib = TestLibrary::new();
    std::fs::create_dir(lib.root.join("pics")).unwrap();
    std::fs::write(lib.root.join("pics/a.jpg"), b"dummy-bytes").unwrap();

    run_one_cycle(&lib, &["--scan"], 1500);

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
