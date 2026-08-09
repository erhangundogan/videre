mod common;
use common::videre_bin;
use std::process::Command;
use tempfile::tempdir;

/// Runs `scan <dir> --output-sqlite <db> --silent [extra_scan_args...]`, then
/// returns the db path. Fails the test via panic if the scan itself fails.
fn scan_into_db(dir: &std::path::Path, db: &std::path::Path, extra: &[&str]) {
    let mut cmd = Command::new(videre_bin());
    cmd.arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(db);
    for a in extra {
        cmd.arg(a);
    }
    cmd.arg(dir);
    let status = cmd.status().expect("failed to run videre scan");
    assert!(status.success(), "scan step failed");
}

#[test]
fn dedupe_prints_remove_paths_for_exact_duplicates() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    let db = home.path().join("hashes.db");

    std::fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    std::fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();
    std::fs::write(scan_dir.path().join("c.jpg"), b"different").unwrap();

    scan_into_db(scan_dir.path(), &db, &[]);

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--db")
        .arg(&db)
        .output()
        .expect("failed to run videre dedupe");
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
    let out = Command::new(videre_bin())
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
fn dedupe_explicit_db_must_exist() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--db")
        .arg(home.path().join("nope.db"))
        .output()
        .expect("failed to run videre dedupe");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no database found at"), "{stderr}");
    assert!(stderr.contains("videre scan"), "{stderr}");
}

#[test]
fn dedupe_similar_reports_empty_when_no_phash_data() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    let db = home.path().join("hashes.db");

    std::fs::write(scan_dir.path().join("a.jpg"), b"content one").unwrap();
    std::fs::write(scan_dir.path().join("b.jpg"), b"content two").unwrap();

    // scanned WITHOUT --similar: no phash data in the db
    scan_into_db(scan_dir.path(), &db, &[]);

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--db")
        .arg(&db)
        .arg("--similar")
        .arg("--json")
        .output()
        .expect("failed to run videre dedupe");
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
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    let db = home.path().join("hashes.db");

    std::fs::copy(
        "tests/fixtures/testsrc_1s.mp4",
        scan_dir.path().join("a.mp4"),
    )
    .unwrap();
    std::fs::copy(
        "tests/fixtures/testsrc_1s_recompressed.mp4",
        scan_dir.path().join("b.mp4"),
    )
    .unwrap();

    scan_into_db(scan_dir.path(), &db, &["--similar"]);

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--db")
        .arg(&db)
        .arg("--similar")
        .arg("--json")
        .output()
        .expect("failed to run videre dedupe");
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
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    let db = home.path().join("hashes.db");

    std::fs::copy("tests/fixtures/red_1s.mp4", scan_dir.path().join("a.mp4")).unwrap();
    std::fs::copy(
        "tests/fixtures/testsrc_1s.mp4",
        scan_dir.path().join("b.mp4"),
    )
    .unwrap();

    scan_into_db(scan_dir.path(), &db, &["--similar"]);

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--db")
        .arg(&db)
        .arg("--similar")
        .arg("--json")
        .output()
        .expect("failed to run videre dedupe");
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
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    let db = home.path().join("hashes.db");

    std::fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    std::fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();
    std::fs::write(scan_dir.path().join("c.jpg"), b"different").unwrap();

    scan_into_db(scan_dir.path(), &db, &[]);

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--db")
        .arg(&db)
        .arg("--json")
        .output()
        .expect("failed to run videre dedupe");

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
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    let db = home.path().join("hashes.db");

    // Not decodable as images, so no phash -> similar_groups is present but empty
    std::fs::write(scan_dir.path().join("a.jpg"), b"content one").unwrap();
    std::fs::write(scan_dir.path().join("b.jpg"), b"content two").unwrap();

    scan_into_db(scan_dir.path(), &db, &["--similar"]);

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--db")
        .arg(&db)
        .arg("--similar")
        .arg("--json")
        .output()
        .expect("failed to run videre dedupe");

    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let similar = doc["similar_groups"]
        .as_array()
        .expect("similar_groups key must be present (an array) with --similar");
    assert!(similar.is_empty());
}

#[test]
fn dedupe_json_matches_mcp_find_duplicates_shape() {
    // Build a db the same way tests/mcp.rs's make_db does, so both surfaces
    // can be exercised against identical data without cross-test-binary imports.
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);
         INSERT INTO file_hashes (path, hash, size_bytes, modified_at, ext) VALUES
           ('/tmp/alice1.jpg', 'hash1', 10, '2020-01-01T00:00:00+00:00', 'jpg'),
           ('/tmp/alice1_copy.jpg', 'hash1', 10, '2024-01-01T00:00:00+00:00', 'jpg'),
           ('/tmp/alice2.jpg', 'hash2', 10, '2021-01-01T00:00:00+00:00', 'jpg');",
    )
    .unwrap();
    videre_core::db::ensure_file_hashes_columns(&conn);
    drop(conn);

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--db")
        .arg(&db)
        .arg("--json")
        .output()
        .expect("failed to run videre dedupe");
    assert!(out.status.success());
    let dedupe_doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let mcp_out = mcp_find_duplicates(&db);

    assert_eq!(
        dedupe_doc, mcp_out,
        "dedupe --json and the MCP find_duplicates tool must produce byte-identical documents"
    );
}

/// Minimal raw JSON-RPC call to `videre mcp --db <db>`'s find_duplicates tool,
/// returning the structuredContent value. Mirrors tests/mcp.rs's McpClient at
/// the minimum needed for one call (that file's harness is not importable
/// from a separate integration test binary).
fn mcp_find_duplicates(db: &std::path::Path) -> serde_json::Value {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    let mut child = Command::new(videre_bin())
        .arg("mcp")
        .arg("--db")
        .arg(db)
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
