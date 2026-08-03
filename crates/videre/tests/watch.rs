use rusqlite::Connection;
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
    path.pop();
    path.pop();
    path.push("videre");
    path
}

#[test]
fn scan_stage_populates_file_hashes() {
    let dir = tempdir().unwrap();
    let pics = dir.path().join("pics");
    std::fs::create_dir(&pics).unwrap();
    std::fs::write(pics.join("a.jpg"), b"dummy-bytes").unwrap();
    let db = dir.path().join("test.db");

    // Run one cycle directly via a very short interval, then kill after
    // giving it time for exactly one cycle.
    let mut child = Command::new(videre_bin()).arg("watch")
        .arg(&pics)
        .arg("--output-sqlite").arg(&db)
        .arg("--scan")
        .arg("--interval").arg("3600") // long enough we only observe one cycle
        .arg("--silent")
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().ok();
    child.wait().ok();

    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "expected the scan stage to have inserted the one file");
}

#[test]
fn faces_stage_skips_hashes_already_processed() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL, bbox TEXT NOT NULL,
             landmark TEXT, embedding BLOB NOT NULL, cluster_id INTEGER, person_label TEXT,
             confirmed INTEGER DEFAULT 0, is_primary INTEGER DEFAULT 0);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/a.jpg', 'h1', 'jpg');
         INSERT INTO faces (hash, bbox, embedding) VALUES ('h1', '0,0,10,10', X'0000');",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(videre_bin()).arg("watch")
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--faces")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().ok();
    child.wait().ok();
    assert!(still_running, "videre watch --faces should not have crashed on an already-processed hash");
}

#[test]
fn heic_stage_writes_no_cache_file_for_non_heic_hashes() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/a.jpg', 'hjpg', 'jpg');",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(videre_bin()).arg("watch")
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--heic")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    child.kill().ok();
    child.wait().ok();

    assert!(!videre_core::thumb_cache::thumb_exists("hjpg", 240), "non-HEIC hash must not get a cached thumbnail");
}

#[test]
fn faces_stage_against_fresh_database_does_not_crash_or_hang() {
    let dir = tempdir().unwrap();
    // No db file exists yet, and no --scan flag either - simulates a user
    // running `videre watch --faces` before any `videre scan`/`videre watch --scan`
    // run has ever created file_hashes.
    let db = dir.path().join("fresh.db");

    let mut child = Command::new(videre_bin()).arg("watch")
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--faces")
        .arg("--interval").arg("3600")
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().ok();
    child.wait().ok();
    assert!(still_running, "videre watch --faces against a fresh database should not crash");
}

#[test]
fn location_stage_populates_location_name_for_gps_rows() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             gps_lat REAL, gps_lon REAL, location_name TEXT);
         INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
             VALUES ('/tmp/paris.jpg', 'hparis', 'jpg', 48.8566, 2.3522);",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(videre_bin()).arg("watch")
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--location")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn videre watch");
    // Real reverse-geocode lookup + network under whatever contention the
    // rest of this file's tests add running in parallel - a bit more
    // headroom than the other stage tests' fixed sleep.
    std::thread::sleep(std::time::Duration::from_millis(3000));
    child.kill().ok();
    child.wait().ok();

    let conn = Connection::open(&db).unwrap();
    let name: Option<String> = conn
        .query_row("SELECT location_name FROM file_hashes WHERE hash = 'hparis'", [], |r| r.get(0))
        .unwrap();
    assert!(name.is_some(), "expected the location stage to have resolved and cached a name");
}

#[test]
fn prune_stage_removes_stale_rows() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             size_bytes INTEGER, created_at TEXT, modified_at TEXT, phash INTEGER,
             exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/does-not-exist.jpg', 'hgone', 'jpg');",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(videre_bin()).arg("watch")
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--prune")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().ok();
    child.wait().ok();

    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 0, "the --prune stage should have removed the row for a file that no longer exists");
}

#[test]
fn default_stages_do_not_include_prune() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             size_bytes INTEGER, created_at TEXT, modified_at TEXT, phash INTEGER,
             exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/does-not-exist.jpg', 'hgone', 'jpg');",
    ).unwrap();
    drop(conn);

    // No stage flags at all: scan/faces/heic/location default on, but prune
    // must stay opt-in only so existing `videre watch` invocations keep
    // their current behavior unchanged.
    let mut child = Command::new(videre_bin()).arg("watch")
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().ok();
    child.wait().ok();

    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "prune must not run unless --prune is passed explicitly");
}

#[test]
fn bare_watch_writes_default_sqlite_db() {
    let dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    let pics = dir.path().join("pics");
    std::fs::create_dir(&pics).unwrap();
    std::fs::write(pics.join("a.jpg"), b"dummy-bytes").unwrap();

    // Run one cycle directly via a very short interval, then kill after
    // giving it time for exactly one cycle.
    let mut child = Command::new(videre_bin()).arg("watch")
        .arg(&pics)
        .arg("--scan")
        .arg("--interval").arg("3600") // long enough we only observe one cycle
        .arg("--silent")
        .env("VIDERE_HOME", home.path())
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().ok();
    child.wait().ok();

    let db = home.path().join("hashes.db");
    assert!(db.exists(), "bare watch must create the default db");
    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "expected the scan stage to have inserted the one file");
}

#[test]
fn config_path_supplies_watch_directory() {
    let dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    let pics = dir.path().join("pics");
    std::fs::create_dir(&pics).unwrap();
    std::fs::write(pics.join("a.jpg"), b"dummy-bytes").unwrap();

    let set = Command::new(videre_bin())
        .arg("config").arg("set").arg("path").arg(&pics)
        .env("VIDERE_HOME", home.path())
        .status()
        .expect("failed to run videre config set");
    assert!(set.success());

    // No directory argument: watch must pick it up from config.
    let mut child = Command::new(videre_bin()).arg("watch")
        .arg("--scan")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .env("VIDERE_HOME", home.path())
        .spawn()
        .expect("failed to spawn videre watch");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().ok();
    child.wait().ok();

    let db = home.path().join("hashes.db");
    assert!(db.exists(), "watch must create the default db from the configured path");
    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "expected the scan stage to have inserted the one file");
}
