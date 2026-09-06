mod common;

use common::TestLibrary;

/// A library seeded with three confirmed faces (two Alice, one Bob), each file
/// path under the canonical root so the row-containment guard accepts it.
fn make_library() -> TestLibrary {
    let lib = TestLibrary::new();
    let root = lib.context().paths.root;
    let conn = lib.init_db();
    for (rel, hash) in [
        ("alice1.jpg", "hash1"),
        ("alice2.jpg", "hash2"),
        ("bob.jpg", "hash3"),
    ] {
        let path = root.join(rel);
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, 'jpg')",
            rusqlite::params![path.to_string_lossy().as_ref(), hash],
        )
        .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
           VALUES ('hash1', '0,0,50,50', X'0000', 'alice', 1),
                  ('hash2', '0,0,50,50', X'0000', 'alice', 1),
                  ('hash3', '0,0,50,50', X'0000', 'bob', 1);",
    )
    .unwrap();
    lib
}

#[test]
fn person_search_prints_confirmed_paths() {
    let lib = make_library();
    let out = lib
        .cmd()
        .args(["search", "--person", "Alice"])
        .output()
        .expect("failed to run videre search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("alice1.jpg"),
        "Expected alice1 in output:\n{stdout}"
    );
    assert!(
        stdout.contains("alice2.jpg"),
        "Expected alice2 in output:\n{stdout}"
    );
    assert!(
        !stdout.contains("bob.jpg"),
        "Expected bob not in output:\n{stdout}"
    );
}

#[test]
fn person_search_empty_for_unknown_name() {
    let lib = make_library();
    let out = lib
        .cmd()
        .args(["search", "--person", "Unknown"])
        .output()
        .expect("failed to run videre search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.trim().is_empty(), "Expected empty stdout:\n{stdout}");
}

#[test]
fn person_search_unconfirmed_not_returned() {
    let lib = TestLibrary::new();
    let path = lib.context().paths.root.join("carol.jpg");
    let conn = lib.init_db();
    conn.execute(
        "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'hash4', 'jpg')",
        [path.to_string_lossy().as_ref()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO faces (hash, bbox, embedding, person_label, confirmed)
           VALUES ('hash4', '0,0,50,50', X'0000', 'Carol', 0)",
        [],
    )
    .unwrap();

    let out = lib
        .cmd()
        .args(["search", "--person", "Carol"])
        .output()
        .expect("failed to run videre search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(
        stdout.trim().is_empty(),
        "Unconfirmed faces should not be returned:\n{stdout}"
    );
}

#[test]
fn person_search_json_outputs_document() {
    let lib = make_library();
    let out = lib
        .cmd()
        .args(["search", "--person", "Alice", "--json"])
        .output()
        .expect("failed to run videre search");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["query"]["kind"], "person");
    assert_eq!(doc["query"]["value"], "Alice");
    assert_eq!(doc["count"], 2);
    let results = doc["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    for r in results {
        assert!(r["path"].as_str().unwrap().contains("alice"));
        assert!(r.get("hash").is_none(), "person hits omit hash: {r}");
        assert!(r.get("score").is_none(), "person hits omit score: {r}");
    }
}

#[test]
fn person_search_json_scores_flag_is_silent_noop() {
    let lib = make_library();
    let plain = lib
        .cmd()
        .args(["search", "--person", "Alice", "--json"])
        .output()
        .expect("failed to run videre search");
    let with_scores = lib
        .cmd()
        .args(["search", "--person", "Alice", "--json", "--scores"])
        .output()
        .expect("failed to run videre search");
    assert!(
        with_scores.status.success(),
        "--scores with --json must not be rejected"
    );
    assert_eq!(
        plain.stdout, with_scores.stdout,
        "--scores must be a no-op under --json"
    );
}

#[test]
fn search_json_error_is_json_object_on_stdout() {
    // A library with a database but no embeddings: a text search fails when it
    // cannot attach a model store, and the error must arrive as one JSON object
    // on stdout rather than a bare stderr line.
    let lib = TestLibrary::new();
    lib.init_db();
    let out = lib
        .cmd()
        .args(["search", "beach", "--json"])
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success(), "must exit nonzero");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("even on error, stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert!(doc["error"]["message"].as_str().is_some());
    assert!(doc.get("results").is_none());
}

#[test]
fn person_search_json_empty_is_silent_on_stderr() {
    // A clean agent invocation (--json) must not leak the human "No confirmed
    // photos" line to stderr; the empty result is already conveyed as count 0.
    let lib = make_library();
    let out = lib
        .cmd()
        .args(["search", "--person", "Unknown", "--json"])
        .output()
        .expect("failed to run videre search");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["count"], 0);
    assert!(doc["results"].as_array().unwrap().is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("No confirmed photos"),
        "--json must not print the human not-found line to stderr:\n{stderr}"
    );
}

#[test]
fn search_json_on_uninitialized_library_yields_json_error() {
    // No scan has run, so the selected library has no database. A --json search
    // still emits exactly one JSON error object on stdout.
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .args(["search", "beach", "--json"])
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("stdout must be one valid JSON object even on error");
    assert_eq!(doc["schema_version"], 1);
    let msg = doc["error"]["message"].as_str().unwrap();
    assert!(msg.contains("not initialized"), "{msg}");
}
