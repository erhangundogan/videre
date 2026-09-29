mod common;

/// Scan one real fixture into a fresh directory-local library.
fn library_with_one_file() -> common::TestLibrary {
    let lib = common::TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();
    lib
}

fn seed_mismatches(lib: &common::TestLibrary, count: usize) -> Vec<String> {
    let conn = lib.init_db();
    (0..count)
        .map(|i| {
            let path = lib
                .root
                .canonicalize()
                .unwrap()
                .join(format!("mismatch-{i:02}.png"))
                .to_string_lossy()
                .into_owned();
            conn.execute(
                "INSERT INTO file_hashes (path, hash, ext, mime)
                 VALUES (?1, ?2, 'png', 'image/jpeg')",
                rusqlite::params![path, format!("hash-{i}")],
            )
            .unwrap();
            path
        })
        .collect()
}

#[test]
fn stats_mismatches_zero_is_explicit() {
    let lib = library_with_one_file();
    let text = lib.cmd().arg("stats").output().unwrap();
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Mismatched files: 0"), "{text}");

    let json = lib.cmd().args(["stats", "--json"]).output().unwrap();
    assert!(json.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(doc["mismatches"]["count"], 0, "{doc}");
    assert_eq!(doc["mismatches"]["files"], serde_json::json!([]));
    assert_eq!(doc["mismatches"]["truncated"], false);
}

#[test]
fn stats_mismatches_are_bounded_unless_requested() {
    let lib = common::TestLibrary::new();
    let paths = seed_mismatches(&lib, 12);

    let text = lib.cmd().arg("stats").output().unwrap();
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("By type:\n  png      image/jpeg"), "{text}");
    assert!(text.contains("Mismatched files: 12"), "{text}");
    assert!(text.contains("... and 2 more"), "{text}");
    for path in &paths[..10] {
        assert!(text.contains(path), "missing {path} in {text}");
    }
    for path in &paths[10..] {
        assert!(!text.contains(path), "unexpected {path} in {text}");
    }

    let json = lib.cmd().args(["stats", "--json"]).output().unwrap();
    assert!(json.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(doc["mismatches"]["count"], 12);
    assert_eq!(doc["mismatches"]["truncated"], true);
    let files = doc["mismatches"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 10);
    for (file, path) in files.iter().zip(&paths) {
        assert_eq!(&file["path"], path);
        assert_eq!(file["ext"], "png");
        assert_eq!(file["mime"], "image/jpeg");
    }

    let text = lib.cmd().args(["stats", "--mismatched"]).output().unwrap();
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(!text.contains("... and"), "{text}");
    for path in &paths {
        assert!(text.contains(path), "missing {path} in {text}");
    }
    let json = lib
        .cmd()
        .args(["stats", "--mismatched", "--json"])
        .output()
        .unwrap();
    assert!(json.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(doc["mismatches"]["count"], 12);
    assert_eq!(doc["mismatches"]["files"].as_array().unwrap().len(), 12);
    assert_eq!(doc["mismatches"]["truncated"], false);
}

#[test]
fn stats_mismatch_path_is_escaped_in_text() {
    let lib = common::TestLibrary::new();
    let path = lib
        .root
        .canonicalize()
        .unwrap()
        .join("strange\n\u{1b}[31m\u{202e}gnp.jpg.png")
        .to_string_lossy()
        .into_owned();
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, mime)
             VALUES (?1, 'strange-hash', 'png', 'image/jpeg')",
            [&path],
        )
        .unwrap();

    let text = lib.cmd().arg("stats").output().unwrap();
    assert!(
        text.status.success(),
        "{}",
        String::from_utf8_lossy(&text.stderr)
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("\\n"), "{text}");
    assert!(text.contains("\\u{1b}"), "{text}");
    assert!(!text.contains('\u{1b}'), "{text}");
    // A right-to-left override would make the name display reversed.
    assert!(text.contains("\\u{202e}"), "{text}");
    assert!(!text.contains('\u{202e}'), "{text}");
    assert!(!text.lines().any(|line| line.starts_with("[31m")), "{text}");

    let json = lib.cmd().args(["stats", "--json"]).output().unwrap();
    assert!(json.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(doc["mismatches"]["files"][0]["path"], path);
}

#[test]
fn stats_mismatch_path_keeps_printable_combining_marks() {
    let lib = common::TestLibrary::new();
    let path = lib
        .root
        .canonicalize()
        .unwrap()
        .join("Fotog\u{306}raf_I\u{307}zmir.png")
        .to_string_lossy()
        .into_owned();
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, ext, mime)
             VALUES (?1, 'turkish-hash', 'png', 'image/jpeg')",
            [&path],
        )
        .unwrap();

    let output = lib.cmd().arg("stats").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(&path), "printable path changed: {text}");
    assert!(!text.contains("\\u{306}"), "combining mark escaped: {text}");
    assert!(!text.contains("\\u{307}"), "combining mark escaped: {text}");
}

#[test]
fn stats_reports_library_totals_without_pipeline_status() {
    // Pipeline run health is operational state, and it lives in `videre
    // status` now; `stats` is pure inventory, so the two surfaces cannot
    // drift into telling different stories about the same runs.
    let lib = library_with_one_file();
    let out = lib
        .cmd()
        .arg("stats")
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Library: 1 file(s)"), "{stdout}");
    assert!(
        !stdout.contains("Pipeline status"),
        "stats must not report pipeline status, {stdout}"
    );
}

#[test]
fn stats_json_has_no_pipelines_field() {
    let lib = library_with_one_file();
    let out = lib
        .cmd()
        .args(["stats", "--json"])
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["library"]["total_files"], 1);
    assert!(
        doc["pipelines"].is_null(),
        "pipeline state moved to videre status --json"
    );
}

#[test]
fn stats_prints_disk_use_once_and_counts_logs() {
    let lib = library_with_one_file();
    let out = lib
        .cmd()
        .arg("stats")
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.matches("Disk use:").count(), 1, "{stdout}");
    // The scan above wrote its log, so the logs directory is not empty.
    assert!(
        stdout.lines().any(|l| l.trim_start().starts_with("logs ")),
        "{stdout}"
    );
}

#[test]
fn stats_json_carries_types_and_disk_use() {
    let lib = library_with_one_file();
    let out = lib
        .cmd()
        .args(["stats", "--json"])
        .output()
        .expect("failed to run videre stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let types = doc["by_type"].as_array().expect("by_type is an array");
    assert_eq!(types.len(), 1, "{doc}");
    assert_eq!(types[0]["ext"], "jpg");
    assert_eq!(types[0]["files"], 1);
    let disk = doc["disk_use"].as_array().expect("disk_use is an array");
    let labels: Vec<&str> = disk.iter().filter_map(|u| u["label"].as_str()).collect();
    assert!(labels.contains(&"database"), "{doc}");
    assert!(labels.contains(&"logs"), "{doc}");
    for u in disk {
        assert!(u["bytes"].as_u64().is_some_and(|b| b > 0), "{u}");
        assert!(u["rebuildable"].is_boolean(), "{u}");
    }
}

#[test]
fn stats_check_flag_is_no_longer_accepted() {
    // --check moved to `videre status` along with the pipeline block; stats
    // is a command that never fails a script, so the flag must be gone
    // rather than silently accepted and ignored.
    let lib = library_with_one_file();
    let out = lib
        .cmd()
        .args(["stats", "--check"])
        .output()
        .expect("failed to run videre stats --check");
    assert!(
        !out.status.success(),
        "--check must not be silently accepted on stats"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unexpected argument"), "{stderr}");
}

#[test]
fn status_tracks_prune_runs() {
    let lib = library_with_one_file();
    // The run is seeded directly here: this test is about status reporting a
    // tracked command's last outcome, not about running prune.
    lib.conn()
        .execute(
            "INSERT OR REPLACE INTO pipeline_runs
                 (command, started_at, finished_at, status, duration_ms, summary)
             VALUES ('prune', '2026-01-01 00:00:00', '2026-01-01 00:00:01', 'success', 5, NULL)",
            [],
        )
        .unwrap();

    let out = lib
        .cmd()
        .args(["status", "--json"])
        .output()
        .expect("failed to run videre status");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pipelines = doc["report"]["pipelines"].as_array().unwrap();
    let prune_entry = pipelines.iter().find(|p| p["command"] == "prune").unwrap();
    assert_eq!(prune_entry["status"], "success");
}

#[test]
fn status_tracks_locations_runs() {
    let lib = common::TestLibrary::new();
    // A GPS row so locations has something to cluster, seeded directly rather
    // than scanned so the coordinate is the fixture. The path must sit under the
    // canonical root or the row-containment guard rejects the whole database.
    let path = lib.context().paths.root.join("a.jpg");
    lib.init_db()
        .execute(
            "INSERT INTO file_hashes (path, hash, gps_lat, gps_lon)
             VALUES (?1, 'h1', 41.0082, 28.9784)",
            [path.to_string_lossy().as_ref()],
        )
        .unwrap();

    let status = lib
        .cmd()
        .args(["locations", "--silent"])
        .status()
        .expect("failed to run videre locations");
    assert!(status.success());

    let out = lib
        .cmd()
        .args(["status", "--json"])
        .output()
        .expect("failed to run videre status");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pipelines = doc["report"]["pipelines"].as_array().unwrap();
    let entry = pipelines
        .iter()
        .find(|p| p["command"] == "locations")
        .unwrap();
    assert_eq!(entry["status"], "success");
}

#[test]
fn stats_errors_cleanly_on_missing_db_without_creating_one() {
    // A library that was never initialized: no .videre database.
    let lib = common::TestLibrary::new();

    let out = lib
        .cmd()
        .arg("stats")
        .output()
        .expect("failed to run videre stats");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not initialized"), "{stderr}");
    assert!(!lib.db().exists(), "stats must not create a database file");
}
