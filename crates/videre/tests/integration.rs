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
    let home = tempdir().unwrap();
    let status = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("/nonexistent/path/abc123")
        .env("VIDERE_HOME", home.path())
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
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--json")
        .arg("/nonexistent/path/abc123")
        .env("VIDERE_HOME", home.path())
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

#[test]
fn bare_dedupe_writes_default_sqlite_db() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // REMOVE candidate still printed to stdout (pipe contract intact)
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);

    let db = home.path().join("hashes.db");
    assert!(db.exists(), "bare dedupe must create the default db");
    assert!(!home.path().join("hashes.jsonl").exists(), "no jsonl by default");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn bare_output_flag_writes_default_jsonl() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    // The bare --output must come AFTER the directory: clap's optional-value
    // arg would otherwise consume the directory as the flag's value.
    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg(scan_dir.path())
        .arg("--output")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let jsonl = home.path().join("hashes.jsonl");
    assert!(jsonl.exists(), "bare --output must target the default jsonl");
    assert_eq!(fs::read_to_string(&jsonl).unwrap().lines().count(), 1);
    assert!(!home.path().join("hashes.db").exists(), "no sqlite db when --output used");
}

#[test]
fn bare_dedupe_without_directory_or_config_path_errors() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("videre config set path"), "{stderr}");

    // --json mode: the same failure arrives as the JSON error object
    let out2 = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .arg("--json")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(!out2.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out2.stdout)
        .expect("stdout must be one valid JSON object even on error");
    assert!(
        doc["error"]["message"].as_str().unwrap().contains("config set path"),
        "{doc}"
    );
}

#[test]
fn config_path_supplies_dedupe_directory() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();

    let set = Command::new(videre_bin())
        .arg("config").arg("set").arg("path").arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .status()
        .unwrap();
    assert!(set.success());

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // REMOVE candidate printed; db created in the default location
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    assert!(home.path().join("hashes.db").exists());
}

#[test]
fn first_explicit_dedupe_adopts_directory_as_default_path() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("saved"), "expected an adoption note: {stderr}");
    assert!(stderr.contains("videre config set path"), "{stderr}");

    // a later BARE dedupe (no directory) now resolves it from config
    let out2 = Command::new(videre_bin())
        .arg("dedupe")
        .arg("--silent")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out2.status.success(), "{}", String::from_utf8_lossy(&out2.stderr));
}

#[test]
fn second_explicit_dedupe_does_not_overwrite_adopted_default_path() {
    let first_dir = tempdir().unwrap();
    let second_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(first_dir.path().join("a.jpg"), b"content").unwrap();
    fs::write(second_dir.path().join("b.jpg"), b"other content").unwrap();

    Command::new(videre_bin())
        .arg("dedupe").arg("--silent").arg(first_dir.path())
        .env("VIDERE_HOME", home.path())
        .status().unwrap();

    let out = Command::new(videre_bin())
        .arg("dedupe").arg("--silent").arg(second_dir.path())
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(out.status.success());
    // --silent suppresses the note even on the first (adopting) run; confirm
    // the SECOND run produced no note either (nothing to adopt: already set)
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty()
        || !String::from_utf8_lossy(&out.stderr).contains("saved"));

    let config = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    let stdout = String::from_utf8_lossy(&config.stdout);
    assert!(
        stdout.contains(&first_dir.path().display().to_string()),
        "default_path must still be the FIRST directory, not overwritten: {stdout}"
    );
}

#[test]
fn silent_flag_suppresses_the_adoption_note() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let out = Command::new(videre_bin())
        .arg("dedupe").arg("--silent").arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty(), "{}", String::from_utf8_lossy(&out.stderr));

    // but the path WAS adopted (silent only suppresses the note, not the effect)
    let config = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(String::from_utf8_lossy(&config.stdout)
        .contains(&scan_dir.path().display().to_string()));
}

#[test]
fn json_mode_adopts_default_path_without_polluting_stdout() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let out = Command::new(videre_bin())
        .arg("dedupe").arg("--silent").arg("--json").arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(out.status.success());
    // stdout must still be exactly one valid JSON document
    let _doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("stdout must remain pure JSON even when adopting a default path");

    let config = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(String::from_utf8_lossy(&config.stdout)
        .contains(&scan_dir.path().display().to_string()));
}
