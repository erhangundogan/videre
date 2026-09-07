mod common;
use common::TestLibrary;
use std::process::Command;

/// Scan this library, having written its files under the root. `extra` passes
/// through additional scan flags (e.g. `--similar`).
fn scan(lib: &TestLibrary, extra: &[&str]) {
    let status = lib
        .cmd()
        .args(["scan", "--silent"])
        .args(extra)
        .status()
        .expect("failed to run videre scan");
    assert!(status.success(), "scan step failed");
}

/// Run `dedupe` in this library with the given extra flags, returning its output.
fn dedupe(lib: &TestLibrary, extra: &[&str]) -> std::process::Output {
    lib.cmd()
        .args(["dedupe", "--silent"])
        .args(extra)
        .output()
        .expect("failed to run videre dedupe")
}

#[test]
fn dedupe_prints_remove_paths_for_exact_duplicates() {
    let lib = TestLibrary::new();
    std::fs::write(lib.root.join("a.jpg"), b"same content").unwrap();
    std::fs::write(lib.root.join("b.jpg"), b"same content").unwrap();
    std::fs::write(lib.root.join("c.jpg"), b"different").unwrap();
    scan(&lib, &[]);

    let out = dedupe(&lib, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one REMOVE candidate expected: {stdout}"
    );
    assert!(lines[0].ends_with("a.jpg") || lines[0].ends_with("b.jpg"));
}

#[test]
fn dedupe_rejects_a_directory_positional() {
    let lib = TestLibrary::new();
    let out = lib
        .cmd()
        .arg("dedupe")
        .arg("/some/directory")
        .output()
        .expect("failed to run videre dedupe");
    assert!(
        !out.status.success(),
        "dedupe must not accept a directory argument"
    );
}

#[test]
fn dedupe_on_an_uninitialized_library_prints_a_friendly_error() {
    // No scan has run, so the library has no database yet. dedupe must say so
    // rather than serve or create an empty one.
    let lib = TestLibrary::new();
    let out = dedupe(&lib, &[]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not initialized"), "{stderr}");
}

#[test]
fn dedupe_similar_reports_empty_when_no_phash_data() {
    let lib = TestLibrary::new();
    std::fs::write(lib.root.join("a.jpg"), b"content one").unwrap();
    std::fs::write(lib.root.join("b.jpg"), b"content two").unwrap();
    // scanned WITHOUT --similar: no phash data in the db
    scan(&lib, &[]);

    let out = dedupe(&lib, &["--similar", "--json"]);
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let similar = doc["similar_groups"]
        .as_array()
        .expect("similar_groups key must be present (an array) with --similar");
    assert!(similar.is_empty());
}

#[test]
#[cfg(target_os = "macos")]
fn dedupe_similar_groups_a_video_and_its_recompressed_variant() {
    // Real near-duplicate scenario: same source content, different bytes/bitrate
    // (a genuine re-encode), not byte-identical files. testsrc_1s.mp4 uses a
    // structured test pattern (gradients + a moving box) rather than a flat
    // color, so its dHash actually has bits to compare, a flat-color frame's
    // dHash is degenerately all-zero (no pixel has a "left > right" edge
    // anywhere), which would make this test pass even for an implementation
    // that couldn't discriminate content at all. See
    // tests/fixtures/testsrc_1s.mp4.txt for the measured Hamming distances
    // that justify this fixture choice.
    let lib = TestLibrary::new();
    lib.copy_fixture("testsrc_1s.mp4", "a.mp4");
    lib.copy_fixture("testsrc_1s_recompressed.mp4", "b.mp4");
    scan(&lib, &["--similar"]);

    let out = dedupe(&lib, &["--similar", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    // The two files have different BLAKE3 hashes (different bitrate/bytes), so
    // this must NOT be caught by exact-duplicate detection, only by --similar.
    let duplicate_groups = doc["duplicate_groups"]
        .as_array()
        .expect("duplicate_groups key must always be present");
    assert!(
        duplicate_groups.is_empty(),
        "a recompressed variant must not be a BLAKE3 exact duplicate: {doc}"
    );

    let similar = doc["similar_groups"]
        .as_array()
        .expect("similar_groups key must be present with --similar");
    assert_eq!(
        similar.len(),
        1,
        "expected exactly one similar group for a video and its recompressed variant: {doc}"
    );
    let files = similar[0]["files"]
        .as_array()
        .expect("a similar group must carry a files array");
    assert_eq!(
        files.len(),
        2,
        "both videos must land in the one similar group: {doc}"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn dedupe_similar_does_not_group_two_visually_different_videos() {
    // Negative case for the positive test above: a flat solid-red clip and a
    // structured test-pattern clip must NOT be grouped as near-duplicates.
    // Their poster-frames are genuinely different (measured Hamming distance
    // 23, well over the clustering threshold of 10; see
    // tests/fixtures/testsrc_1s.mp4.txt). Without this test, an
    // implementation that grouped every video together (e.g. a bug that
    // always returned the same dHash) would still pass the positive test.
    let lib = TestLibrary::new();
    lib.copy_fixture("red_1s.mp4", "a.mp4");
    lib.copy_fixture("testsrc_1s.mp4", "b.mp4");
    scan(&lib, &["--similar"]);

    let out = dedupe(&lib, &["--similar", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let similar = doc["similar_groups"]
        .as_array()
        .expect("similar_groups key must be present with --similar");
    assert!(
        similar.is_empty(),
        "a flat-color clip and a structured-pattern clip must not be grouped as similar: {doc}"
    );
}

#[test]
fn json_output_reports_duplicate_groups() {
    let lib = TestLibrary::new();
    std::fs::write(lib.root.join("a.jpg"), b"same content").unwrap();
    std::fs::write(lib.root.join("b.jpg"), b"same content").unwrap();
    std::fs::write(lib.root.join("c.jpg"), b"different").unwrap();
    scan(&lib, &[]);

    let out = dedupe(&lib, &["--json"]);
    assert!(out.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["total_files"], 3);

    let groups = doc["duplicate_groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "one exact-duplicate group expected");
    let keep = groups[0]["keep"]["path"].as_str().unwrap();
    let remove = groups[0]["remove"].as_array().unwrap();
    assert_eq!(remove.len(), 1);
    let removed = remove[0]["path"].as_str().unwrap();

    let mut pair = vec![keep.to_string(), removed.to_string()];
    pair.sort();
    assert!(
        pair[0].ends_with("a.jpg") && pair[1].ends_with("b.jpg"),
        "keep+remove must be exactly the identical pair, got {pair:?}"
    );
    assert!(keep != removed);

    assert!(
        doc.get("similar_groups").is_none(),
        "similar_groups key must be absent without --similar"
    );
}

#[test]
fn json_with_similar_flag_includes_similar_groups_key() {
    let lib = TestLibrary::new();
    // Not decodable as images, so no phash -> similar_groups is present but empty
    std::fs::write(lib.root.join("a.jpg"), b"content one").unwrap();
    std::fs::write(lib.root.join("b.jpg"), b"content two").unwrap();
    scan(&lib, &["--similar"]);

    let out = dedupe(&lib, &["--similar", "--json"]);
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let similar = doc["similar_groups"]
        .as_array()
        .expect("similar_groups key must be present (an array) with --similar");
    assert!(similar.is_empty());
}

#[test]
fn dedupe_json_matches_mcp_find_duplicates_shape() {
    // Seed a library database directly with a duplicate pair, then exercise
    // both surfaces against it. All paths sit under the library root so the
    // row-containment guard accepts the database.
    let lib = TestLibrary::new();
    let conn = lib.init_db();
    let a1 = lib.context().paths.root.join("alice1.jpg");
    let a1_copy = lib.context().paths.root.join("alice1_copy.jpg");
    let a2 = lib.context().paths.root.join("alice2.jpg");
    conn.execute(
        "INSERT INTO file_hashes (path, hash, size_bytes, modified_at, ext) VALUES
           (?1, 'hash1', 10, '2020-01-01T00:00:00+00:00', 'jpg'),
           (?2, 'hash1', 10, '2024-01-01T00:00:00+00:00', 'jpg'),
           (?3, 'hash2', 10, '2021-01-01T00:00:00+00:00', 'jpg')",
        rusqlite::params![
            a1.to_string_lossy(),
            a1_copy.to_string_lossy(),
            a2.to_string_lossy()
        ],
    )
    .unwrap();
    drop(conn);

    let out = dedupe(&lib, &["--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dedupe_doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let mcp_out = mcp_find_duplicates(&lib);

    assert_eq!(
        dedupe_doc, mcp_out,
        "dedupe --json and the MCP find_duplicates tool must produce byte-identical documents"
    );
}

/// Minimal raw JSON-RPC call to this library's `videre mcp` find_duplicates
/// tool, returning the structuredContent value. Mirrors tests/mcp.rs's
/// McpClient at the minimum needed for one call (that file's harness is not
/// importable from a separate integration test binary).
fn mcp_find_duplicates(lib: &TestLibrary) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let mut child = lib
        .cmd()
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn videre mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    let mut send = |msg: serde_json::Value| {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = || -> serde_json::Value {
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line).expect("read from server");
            assert!(n > 0, "server closed stdout unexpectedly");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return serde_json::from_str(trimmed).expect("each stdout line must be valid JSON");
        }
    };

    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "integration-test", "version": "0"}
        }
    }));
    recv();
    send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "find_duplicates", "arguments": {}}
    }));
    let resp = recv();

    drop(stdin);
    let _ = child.wait();

    resp["result"]["structuredContent"].clone()
}
