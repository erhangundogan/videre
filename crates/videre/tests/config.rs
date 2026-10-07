mod common;

use common::TestLibrary;

fn run(library: &TestLibrary, args: &[&str]) -> std::process::Output {
    library.cmd().args(args).output().unwrap()
}

fn config_text(library: &TestLibrary) -> String {
    std::fs::read_to_string(library.root.join(".videre/config.toml")).unwrap()
}

#[test]
fn config_show_reports_local_fixed_paths_without_creating_state() {
    let library = TestLibrary::new();
    let output = run(&library, &["config"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&library.root.display().to_string()),
        "{stdout}"
    );
    assert!(
        stdout.contains("selected by:   invocation directory"),
        "{stdout}"
    );
    assert!(stdout.contains(".videre/hashes.db"), "{stdout}");
    assert!(stdout.contains(".videre/hashes.jsonl"), "{stdout}");
    assert!(
        stdout.contains(videre_core::embeddings::DEFAULT_MODEL_ID),
        "{stdout}"
    );
    assert!(!library.root.join(".videre").exists());
}

#[test]
fn explicit_config_edits_only_the_selected_library() {
    let library = TestLibrary::new();
    let launch = TestLibrary::new();
    let output = library
        .from(&launch.root)
        .args(["config", "set", "model", "owner/model-224"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(config_text(&library).contains("default_model = \"owner/model-224\""));
    assert!(!launch.root.join(".videre").exists());

    let shown = library.from(&launch.root).arg("config").output().unwrap();
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(stdout.contains("selected by:   --library"), "{stdout}");
    assert!(stdout.contains("owner/model-224"), "{stdout}");
}

#[test]
fn model_set_and_unset_preserve_other_settings() {
    let library = TestLibrary::new();
    for args in [
        ["config", "set", "xmp", "file"],
        ["config", "set", "model", "owner/model-224"],
    ] {
        let output = run(&library, &args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(run(&library, &["config", "unset", "model"])
        .status
        .success());
    let text = config_text(&library);
    assert!(!text.contains("default_model"), "{text}");
    assert!(text.contains("xmp_precedence = \"file\""), "{text}");
}

#[test]
fn invalid_model_is_rejected_without_writing_it() {
    let library = TestLibrary::new();
    let output = run(
        &library,
        &["config", "set", "model", "siglip-base-patch16-224"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("owner/name"));
    assert!(!library.root.join(".videre/config.toml").exists());
}

#[test]
fn model_flags_reject_malformed_ids_before_unconverted_dispatch() {
    let library = TestLibrary::new();
    for command in ["embed", "search", "classify"] {
        let mut process = library.cmd();
        process.arg(command);
        if command == "search" {
            process.arg("anything");
        }
        let output = process.args(["--model", "foo"]).output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("owner/name"), "{command}: {stderr}");
        assert!(!stderr.contains("panicked"), "{command}: {stderr}");
    }
}

#[test]
fn read_rate_roundtrips_and_invalid_values_preserve_prior_bytes() {
    let library = TestLibrary::new();
    assert!(run(&library, &["config", "set", "read-rate", "50"])
        .status
        .success());
    let before = config_text(&library);
    assert!(before.contains("min_read_rate_mb_s = 50"), "{before}");

    for value in ["0", "fast"] {
        assert!(!run(&library, &["config", "set", "read-rate", value])
            .status
            .success());
        assert_eq!(config_text(&library), before);
    }
    assert!(run(&library, &["config", "unset", "read-rate"])
        .status
        .success());
    assert!(!config_text(&library).contains("min_read_rate_mb_s"));
}

#[test]
fn io_workers_roundtrips_and_invalid_values_preserve_prior_bytes() {
    let library = TestLibrary::new();
    assert!(run(&library, &["config", "set", "io-workers", "1"])
        .status
        .success());
    let before = config_text(&library);
    assert!(before.contains("max_io_workers = 1"), "{before}");
    let shown = run(&library, &["config"]);
    assert!(String::from_utf8_lossy(&shown.stdout).contains("io-workers:    1"));
    for value in ["0", "257", "many"] {
        assert!(!run(&library, &["config", "set", "io-workers", value])
            .status
            .success());
        assert_eq!(config_text(&library), before);
    }
    assert!(run(&library, &["config", "unset", "io-workers"])
        .status
        .success());
    assert!(!config_text(&library).contains("max_io_workers"));
}

#[test]
fn watch_debounce_ms_roundtrips_and_invalid_values_preserve_prior_bytes() {
    let library = TestLibrary::new();
    assert!(
        run(&library, &["config", "set", "watch-debounce-ms", "250"])
            .status
            .success()
    );
    let before = config_text(&library);
    assert!(before.contains("watch_debounce_ms = 250"), "{before}");

    for value in ["0", "fast"] {
        assert!(
            !run(&library, &["config", "set", "watch-debounce-ms", value])
                .status
                .success()
        );
        assert_eq!(config_text(&library), before);
    }
    assert!(run(&library, &["config", "unset", "watch-debounce-ms"])
        .status
        .success());
    assert!(!config_text(&library).contains("watch_debounce_ms"));
}

#[test]
fn xmp_and_watch_export_settings_roundtrip() {
    let library = TestLibrary::new();
    assert!(run(&library, &["config", "set", "xmp", "newest"])
        .status
        .success());
    assert!(
        run(&library, &["config", "set", "export-xmp-on-watch", "true"])
            .status
            .success()
    );
    let shown = run(&library, &["config"]);
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(stdout.contains("xmp:           newest"), "{stdout}");
    assert!(stdout.contains("export-xmp-on-watch: on"), "{stdout}");
}

#[test]
fn the_gallery_watch_setting_is_on_by_default_and_can_be_turned_off() {
    let library = TestLibrary::new();
    let shown = run(&library, &["config"]);
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(stdout.contains("gallery-starts-watch: on"), "{stdout}");
    assert!(run(
        &library,
        &["config", "set", "gallery-starts-watch", "false"]
    )
    .status
    .success());
    let shown = run(&library, &["config"]);
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(stdout.contains("gallery-starts-watch: off"), "{stdout}");
    assert!(config_text(&library).contains("gallery_starts_watch = false"));
    let bad = run(
        &library,
        &["config", "set", "gallery-starts-watch", "sometimes"],
    );
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("true or false"));
}

#[test]
fn turning_street_detail_off_deletes_the_downloaded_street_map() {
    let library = TestLibrary::new();
    let shown = run(&library, &["config"]);
    assert!(String::from_utf8_lossy(&shown.stdout).contains("street-detail: off"));
    assert!(run(&library, &["config", "set", "street-detail", "true"])
        .status
        .success());
    assert!(config_text(&library).contains("street_detail = true"));

    // Stand-ins for a finished download.
    let dir = library.root.join(".videre").join("basemap");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("detail.pmtiles"), b"sokak").unwrap();
    std::fs::write(dir.join("detail.json"), b"{}").unwrap();

    let off = run(&library, &["config", "set", "street-detail", "false"]);
    assert!(off.status.success());
    assert!(!dir.join("detail.pmtiles").exists());
    assert!(!dir.join("detail.json").exists());
    assert!(
        String::from_utf8_lossy(&off.stderr).contains("deleted"),
        "{off:?}"
    );

    // Unset is off too.
    std::fs::write(dir.join("detail.pmtiles"), b"sokak").unwrap();
    assert!(run(&library, &["config", "unset", "street-detail"])
        .status
        .success());
    assert!(!dir.join("detail.pmtiles").exists());
}

#[test]
fn removed_and_unknown_config_keys_are_rejected() {
    for key in ["db", "path", "jsonl", "nope"] {
        let library = TestLibrary::new();
        let output = run(&library, &["config", "set", key, "value"]);
        assert!(!output.status.success(), "{key} must be rejected");
        assert!(!library.root.join(".videre").exists());
    }
}

#[test]
fn config_and_scan_report_the_same_fixed_database() {
    let library = TestLibrary::new();
    let shown = run(&library, &["config"]);
    let stdout = String::from_utf8_lossy(&shown.stdout);
    assert!(
        stdout.contains(&library.db().display().to_string()),
        "{stdout}"
    );

    library.copy_fixture("tiny.jpg", "image.jpg");
    let scanned = run(&library, &["scan", "--silent", "--json"]);
    assert!(scanned.status.success());
    let json: serde_json::Value = serde_json::from_slice(&scanned.stdout).unwrap();
    assert_eq!(
        json["output"]["path"],
        library.context().paths.db.display().to_string()
    );
}

#[test]
fn search_cutoffs_are_set_shown_and_checked() {
    let library = TestLibrary::new();
    let shown = String::from_utf8_lossy(&run(&library, &["config"]).stdout).to_string();
    assert!(shown.contains("search-min-match: 0 (default)"), "{shown}");
    assert!(
        shown.contains("similar-min-score: none (default)"),
        "{shown}"
    );

    for (key, value) in [("search-min-match", "0.25"), ("similar-min-score", "0.6")] {
        let output = run(&library, &["config", "set", key, value]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = config_text(&library);
    assert!(text.contains("search_min_match = 0.25"), "{text}");
    assert!(text.contains("similar_min_score = 0.6"), "{text}");
    let shown = String::from_utf8_lossy(&run(&library, &["config"]).stdout).to_string();
    assert!(shown.contains("search-min-match: 0.25\n"), "{shown}");
    assert!(shown.contains("similar-min-score: 0.6\n"), "{shown}");

    for (key, value) in [("search-min-match", "high"), ("search-min-match", "1.5")] {
        let output = run(&library, &["config", "set", key, value]);
        assert!(!output.status.success(), "{key} {value}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("search") && stderr.contains("must be"),
            "{stderr}"
        );
    }
    assert!(config_text(&library).contains("search_min_match = 0.25"));
}
