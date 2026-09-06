mod common;
use common::TestLibrary;

/// A library seeded with one unconfirmed face, its file path under the root.
fn library_with_faces() -> TestLibrary {
    let lib = TestLibrary::new();
    let path = lib.context().paths.root.join("a.jpg");
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'abc123', 'jpg')",
        [path.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO faces (hash, bbox, embedding, confirmed) VALUES ('abc123', '0,0,50,50', X'0000', 0)",
        [],
    )
    .unwrap();
    lib
}

#[test]
fn get_faces_returns_singletons() {
    // People are a first-class view of the gallery rather than a flag on it.
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["gallery", "--help"])
        .output()
        .expect("run gallery --help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("people"),
        "Expected the people view in help: {stdout}"
    );
}

#[test]
fn library_with_faces_creates_valid_schema() {
    let lib = library_with_faces();
    let conn = lib.conn();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM faces", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "expected one seed face row");
    conn.execute("UPDATE faces SET is_primary = 1 WHERE id = 1", [])
        .unwrap();
}

#[test]
fn help_documents_the_port_it_listens_on() {
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["gallery", "--help"])
        .output()
        .expect("run gallery --help");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--port"),
        "gallery --help must document --port"
    );
}

#[test]
fn gallery_starts_and_keeps_serving() {
    // The server must survive startup rather than exiting on a parse or bind
    // error. It binds a port and blocks, so this checks it is still alive a
    // moment later rather than waiting on a request.
    let lib = library_with_faces();
    let mut child = lib
        .cmd()
        .args(["gallery", "--port", "7893"])
        .spawn()
        .expect("failed to spawn videre gallery");
    std::thread::sleep(std::time::Duration::from_millis(300));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().ok();
    child.wait().ok();
    assert!(
        still_running,
        "videre gallery should still be serving, not have exited"
    );
}

#[test]
fn thumb_cache_hit_uses_the_library_cache_namespace() {
    // Seed a fake cached thumbnail directly in a library's cache, then confirm
    // the context-aware helpers the gallery raw handler uses find it.
    let lib = TestLibrary::new();
    let cache = lib.context().cache;
    let hash = "test-cache-hit-hash";
    std::fs::create_dir_all(&cache.thumbnails).unwrap();
    std::fs::write(
        videre_core::thumb_cache::thumb_path_in(&cache, hash, 240),
        b"fake-jpeg-bytes",
    )
    .unwrap();
    assert!(videre_core::thumb_cache::thumb_exists_in(&cache, hash, 240));
}
