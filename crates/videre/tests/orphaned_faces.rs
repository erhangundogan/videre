//! Faces never outlive the content they were detected on.

mod common;
use common::TestLibrary;

fn hash_of(lib: &TestLibrary, name: &str) -> String {
    // Scan records canonical paths (a macOS temp dir sits behind /private).
    let path = lib.root.canonicalize().unwrap().join(name);
    lib.conn()
        .query_row(
            "SELECT hash FROM file_hashes WHERE path = ?1",
            [path.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .unwrap()
}

fn add_labeled_face(lib: &TestLibrary, hash: &str) {
    let conn = lib.conn();
    conn.execute_batch("INSERT OR IGNORE INTO people (name, full_name) VALUES ('çağla', 'Çağla');")
        .unwrap();
    conn.execute(
        "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
         VALUES (?1, '0,0,9,9', X'0000', 'çağla', 1)",
        [hash],
    )
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO faces_scanned (hash) VALUES (?1)",
        [hash],
    )
    .unwrap();
}

fn faces_for(lib: &TestLibrary, hash: &str) -> i64 {
    lib.conn()
        .query_row("SELECT COUNT(*) FROM faces WHERE hash = ?1", [hash], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn a_file_changed_elsewhere_drops_its_old_faces_and_says_so() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "Arşiv/çağla.jpg");
    lib.scan();
    let old = hash_of(&lib, "Arşiv/çağla.jpg");
    add_labeled_face(&lib, &old);

    // Another application rewrites the photo.
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sample_with_exif.jpg"),
        lib.root.join("Arşiv/çağla.jpg"),
    )
    .unwrap();
    let out = lib.cmd().args(["scan"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_ne!(hash_of(&lib, "Arşiv/çağla.jpg"), old);
    assert_eq!(faces_for(&lib, &old), 0, "the old content's faces are gone");
    let stderr = common::stderr_without_library_noise(&String::from_utf8_lossy(&out.stderr));
    assert!(
        stderr.contains("çağla.jpg changed; its 1 labeled face(s) will be detected again"),
        "{stderr}"
    );
}

#[test]
fn a_changed_copy_leaves_the_other_copys_faces() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.copy_fixture("tiny.jpg", "b.jpg");
    lib.scan();
    let shared = hash_of(&lib, "a.jpg");
    add_labeled_face(&lib, &shared);

    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sample_with_exif.jpg"),
        lib.root.join("a.jpg"),
    )
    .unwrap();
    lib.scan();

    assert_eq!(hash_of(&lib, "b.jpg"), shared);
    assert_eq!(faces_for(&lib, &shared), 1, "b.jpg still has that content");
}
