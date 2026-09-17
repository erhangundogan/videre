mod common;
use common::TestLibrary;

/// A library with one labeled face, one person record, a scanned marker,
/// and one image on disk.
fn seeded() -> TestLibrary {
    let lib = TestLibrary::new();
    // Real image bytes, so the post-wipe rebuild can actually detect faces.
    // The stored path uses the context root's spelling: copy_fixture returns
    // a canonicalized /private/... path whose prefix fails the library's
    // root-identity recheck.
    lib.copy_fixture("sample_with_exif.jpg", "a.jpg");
    let path = lib.context().paths.root.join("a.jpg");
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'abc123', 'jpg')",
        [path.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO faces (hash, bbox, embedding, confirmed, person_label)
         VALUES ('abc123', '0,0,50,50', X'0000', 1, 'elena')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO people (name, full_name) VALUES ('elena', 'Elena')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO faces_scanned (hash) VALUES ('abc123')", [])
        .unwrap();
    drop(conn);
    lib
}

#[test]
fn reset_without_yes_on_noninteractive_stdin_refuses_and_changes_nothing() {
    let lib = seeded();
    let out = lib
        .cmd()
        .args(["faces", "--reset"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a non-interactive reset must refuse rather than wipe silently"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
    let n: i64 = lib
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "refusal must leave every row in place");
}

#[test]
fn reset_on_a_library_where_faces_never_ran_bails_out() {
    let lib = TestLibrary::new();
    drop(lib.init_db());
    let out = lib
        .cmd()
        .args(["faces", "--reset", "--yes"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "nothing to reset must be an error, not a silent success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nothing to reset"), "{stderr}");
}

#[test]
fn reset_with_yes_wipes_and_starts_over() {
    let lib = seeded();
    if common::skip_without_models("faces reset rebuild", common::face_models_cached()) {
        return;
    }
    let out = lib
        .cmd()
        .args([
            "faces",
            "--reset",
            "--yes",
            "--min-cluster-size",
            "1",
            "--silent",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The receipt prints even under --silent: one line naming what was
    // wiped before the rebuild's quiet output.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("reset wiped 1 face row(s)") && stderr.contains("1 labeled"),
        "the reset receipt must name the wipe: {stderr}"
    );
    let conn = lib.conn();
    let labeled: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(labeled, 0, "the rebuild must not resurrect the wiped label");
    let scanned: i64 = conn
        .query_row("SELECT COUNT(*) FROM faces_scanned", [], |r| r.get(0))
        .unwrap();
    assert!(scanned >= 1, "the rebuild re-marks the image as scanned");
}

#[test]
fn the_legacy_alias_reaches_the_same_refusal() {
    let lib = seeded();
    let out = lib
        .cmd()
        .args(["faces", "--reprocess"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the alias must parse and refuse identically"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
}
