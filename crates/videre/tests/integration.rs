use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn videre_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

#[test]
fn exact_duplicates_appear_in_output_file() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    // Two files with identical content
    fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("c.jpg"), b"different").unwrap();

    let status = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--output")
        .arg(&output)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre");

    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    let lines: Vec<_> = content.lines().collect();
    assert_eq!(lines.len(), 3); // 3 records written

    // Both identical files have the same hash (order is non-deterministic due to rayon)
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut hash_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in &records {
        *hash_counts.entry(r["hash"].as_str().unwrap().to_string()).or_insert(0) += 1;
    }
    assert!(hash_counts.values().any(|&c| c >= 2), "expected at least one hash to appear twice");
}

#[test]
fn missing_directory_exits_nonzero() {
    let status = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("/nonexistent/path/abc123")
        .status()
        .expect("failed to run videre");
    assert!(!status.success());
}

#[test]
fn exif_fields_populated_for_jpeg_with_exif() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    fs::copy(
        "tests/fixtures/sample_with_exif.jpg",
        scan_dir.path().join("photo.jpg"),
    )
    .unwrap();

    let status = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--output")
        .arg(&output)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre");

    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();

    assert_eq!(record["exif_date"], "2021-08-10T19:34:03");
    assert!(record["gps_lat"].as_f64().is_some());
    assert!(record["gps_lon"].as_f64().is_some());
    assert_eq!(record["width"], 4032);
    assert_eq!(record["height"], 3024);
}

#[test]
fn sqlite_output_writes_records_to_db() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content alpha").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"content beta").unwrap();

    let status = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre");

    assert!(status.success());
    assert!(db_path.exists());

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn sqlite_output_upserts_on_repeated_run() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("photo.jpg"), b"original content").unwrap();

    // First run
    Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre")
        .success()
        .then_some(())
        .expect("first run failed");

    // Second run with same folder — should overwrite, not append
    Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre")
        .success()
        .then_some(())
        .expect("second run failed");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "upsert should not duplicate records");
}

#[test]
fn sqlite_and_output_flags_conflict() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();

    let status = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--output")
        .arg(out_dir.path().join("hashes"))
        .arg("--output-sqlite")
        .arg(out_dir.path().join("hashes.db"))
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre");

    assert!(!status.success(), "should fail when both --output and --output-sqlite are given");
}

#[test]
fn json_output_reports_duplicate_groups() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("c.jpg"), b"different").unwrap();

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--output")
        .arg(&output)
        .arg("--json")
        .arg(scan_dir.path())
        .output()
        .expect("failed to run videre");

    assert!(out.status.success());
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["scanned"], 3);

    let groups = doc["duplicate_groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "one exact-duplicate group expected");
    let keep = groups[0]["keep"]["path"].as_str().unwrap();
    let remove = groups[0]["remove"].as_array().unwrap();
    assert_eq!(remove.len(), 1);
    let removed = remove[0]["path"].as_str().unwrap();

    // a.jpg and b.jpg are the identical pair; which is KEEP is date-tie dependent
    let mut pair = vec![keep.to_string(), removed.to_string()];
    pair.sort();
    assert!(pair[0].ends_with("a.jpg") && pair[1].ends_with("b.jpg"),
        "keep+remove must be exactly the identical pair, got {pair:?}");
    assert!(keep != removed);

    assert!(doc.get("similar_groups").is_none(),
        "similar_groups key must be absent without --similar");
}

#[test]
fn json_with_similar_flag_includes_similar_groups_key() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    // Not decodable as images, so no phash -> similar_groups is present but empty
    fs::write(scan_dir.path().join("a.jpg"), b"content one").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"content two").unwrap();

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--output")
        .arg(&output)
        .arg("--similar")
        .arg("--json")
        .arg(scan_dir.path())
        .output()
        .expect("failed to run videre");

    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let similar = doc["similar_groups"]
        .as_array()
        .expect("similar_groups key must be present (an array) with --similar");
    assert!(similar.is_empty());
}

#[test]
fn json_error_object_for_missing_directory() {
    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--json")
        .arg("/nonexistent/path/abc123")
        .output()
        .expect("failed to run videre");

    assert!(!out.status.success(), "must exit nonzero");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("even on error, stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    let msg = doc["error"]["message"].as_str().unwrap();
    assert!(msg.contains("does not exist"), "unexpected message: {msg}");
    assert!(doc.get("duplicate_groups").is_none());
}
