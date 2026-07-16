# Split `dedupe` into `scan` (ingest) and `dedupe` (query) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split `videre dedupe` (which currently both scans a directory into the database AND reports duplicate groups) into `videre scan` (writer: filesystem to db) and `videre dedupe` (reader: db to duplicate report), matching the shape every other subcommand already has.

**Architecture:** `scan.rs` is a new file absorbing today's `dedupe.rs` writer body almost verbatim, minus duplicate reporting. `dedupe.rs` is rewritten to a thin reader like `report`/`search`/`faces`/`prune`. A new shared `FindDuplicatesJson` type and `build_find_duplicates` function (promoted from the MCP server's private copies) back both `dedupe --json` and the MCP `find_duplicates` tool, so the two surfaces stay byte-identical by construction. A new `resolve_reader_db_must_exist` helper in `commands/mod.rs` gives `dedupe` and `mcp` (both of which bind to one db with no fallback ingestion step) a stricter existence check than the other five readers, which keep today's looser per-command behavior unchanged.

**Tech Stack:** Rust, existing crates only (no new dependencies). Baseline: `cargo test --workspace` = 179 passing on `main` at `f06ab3b`.

**Behavior contract:** Explicit breaking change (the fourth in this project's history, all deliberate pre-1.0). `videre dedupe <dir>` no longer works; use `videre scan <dir>` then `videre dedupe`. Every other subcommand (`report`, `search`, `faces`, `prune`, `fix-dates`, `embed`, `config`, `watch`, `mcp`) is unaffected in behavior, except for the six error-hint strings in section "Task 5" that now say `scan` instead of `dedupe`. `watch --scan`'s internal stage is untouched code.

**House rules (mandatory):** never use the em dash character anywhere (code, comments, commit messages); no Co-Authored-By trailer or "Generated with" line; use the exact commit messages given.

**Branch:** work on a new branch `scan-dedupe-split` off `main`:

```bash
cd /Users/erhangundogan/projects/rust/videre
git checkout -b scan-dedupe-split
```

---

### Task 1: Shared JSON types (`FindDuplicatesJson`, `ScanJson`, `ScanOutputJson`)

Purely additive: new types alongside the existing ones in `videre::types`. `DedupeJson` is untouched in this task (still used by today's `dedupe.rs`; it gets deleted in Task 3 when its last caller disappears).

**Files:**
- Modify: `crates/videre/src/types.rs`

- [ ] **Step 1: Write the failing tests**

Append to the existing `#[cfg(test)] mod tests` block in `crates/videre/src/types.rs` (it already has a `rec(path, hash)` helper you can reuse):

```rust
    #[test]
    fn find_duplicates_json_omits_similar_groups_when_none() {
        let doc = FindDuplicatesJson {
            schema_version: SCHEMA_VERSION,
            total_files: 3,
            duplicate_groups: vec![],
            similar_groups: None,
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(!json.contains("similar_groups"));
    }

    #[test]
    fn find_duplicates_json_includes_similar_groups_when_some() {
        let doc = FindDuplicatesJson {
            schema_version: SCHEMA_VERSION,
            total_files: 2,
            duplicate_groups: vec![],
            similar_groups: Some(vec![SimilarGroupJson {
                hash: "phash:00000000000000ff".to_string(),
                files: vec![rec("/x.jpg", "111"), rec("/y.jpg", "222")],
            }]),
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"similar_groups\""));
        assert!(json.contains("\"files\""));
        assert!(!json.contains("\"keep\""), "similar groups are flat clusters, not keep/remove");
    }

    #[test]
    fn scan_json_reports_output_kind_and_path() {
        let doc = ScanJson {
            schema_version: SCHEMA_VERSION,
            total_files: 5,
            output: ScanOutputJson { kind: "sqlite", path: "/tmp/hashes.db".to_string() },
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(json.contains("\"total_files\":5"));
        assert!(json.contains("\"kind\":\"sqlite\""));
        assert!(json.contains("\"path\":\"/tmp/hashes.db\""));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --lib types::`
Expected: COMPILE ERROR (`FindDuplicatesJson`, `ScanJson`, `ScanOutputJson` not found).

- [ ] **Step 3: Implement**

Add to `crates/videre/src/types.rs`, after the existing `SimilarGroupJson`/`impl From<DuplicateGroup> for SimilarGroupJson` block and before `DedupeJson`:

```rust
/// Top-level document for `dedupe --json` and the MCP `find_duplicates` tool.
/// Shared by both so they cannot silently diverge in shape.
#[derive(Debug, Serialize)]
pub struct FindDuplicatesJson {
    pub schema_version: u32,
    pub total_files: usize,
    pub duplicate_groups: Vec<DupGroupJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similar_groups: Option<Vec<SimilarGroupJson>>,
}

/// Where `scan --json` wrote records: `"sqlite"` or `"jsonl"`, and the resolved path.
#[derive(Debug, Serialize)]
pub struct ScanOutputJson {
    pub kind: &'static str,
    pub path: String,
}

/// Top-level document for `scan --json`. Describes the scan itself (files
/// processed, where they were written), not duplicate data - that is
/// `dedupe`'s job, not scan's.
#[derive(Debug, Serialize)]
pub struct ScanJson {
    pub schema_version: u32,
    pub total_files: usize,
    pub output: ScanOutputJson,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --lib types::`
Expected: PASS (11 tests: 8 existing + 3 new).

Run: `cargo test --workspace`
Expected: PASS, 182 total (179 baseline + 3).

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/types.rs
git commit -m "feat: FindDuplicatesJson and ScanJson types shared between dedupe and mcp/scan"
```

---

### Task 2: New `videre scan` subcommand

Absorbs today's `dedupe.rs` writer body into a new file, dropping duplicate-group reporting. `dedupe.rs` is untouched in this task (still the old scan-and-report binary); both commands can scan a directory during this task, which is fine because Task 3 rewrites `dedupe.rs` next and removes the duplication.

**Files:**
- Create: `crates/videre/src/commands/scan.rs`
- Modify: `crates/videre/src/commands/mod.rs`, `crates/videre/src/main.rs`
- Test: `crates/videre/tests/scan.rs` (new)

- [ ] **Step 1: Write the failing tests**

Create `crates/videre/tests/scan.rs`:

```rust
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
fn jsonl_output_contains_all_scanned_records_with_correct_hashes() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("c.jpg"), b"different").unwrap();

    let status = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output")
        .arg(&output)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre");

    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    let lines: Vec<_> = content.lines().collect();
    assert_eq!(lines.len(), 3);

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
        .arg("scan")
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
        .arg("scan")
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
        .arg("scan")
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

    Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre")
        .success()
        .then_some(())
        .expect("first run failed");

    Command::new(videre_bin())
        .arg("scan")
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
        .arg("scan")
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
fn bare_scan_writes_default_sqlite_db() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"same content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"same content").unwrap();

    let out = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty(), "scan's stdout is always empty in text mode");

    let db = home.path().join("hashes.db");
    assert!(db.exists(), "bare scan must create the default db");
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

    let out = Command::new(videre_bin())
        .arg("scan")
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
fn bare_scan_without_directory_or_config_path_errors() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("videre config set path"), "{stderr}");

    let out2 = Command::new(videre_bin())
        .arg("scan")
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
fn config_path_supplies_scan_directory() {
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
        .arg("scan")
        .arg("--silent")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(home.path().join("hashes.db").exists());
}

#[test]
fn first_explicit_scan_adopts_directory_as_default_path() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let out = Command::new(videre_bin())
        .arg("scan")
        .arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("saved"), "expected an adoption note: {stderr}");
    assert!(stderr.contains("videre config set path"), "{stderr}");

    let out2 = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .env("VIDERE_HOME", home.path())
        .output()
        .expect("failed to run videre");
    assert!(out2.status.success(), "{}", String::from_utf8_lossy(&out2.stderr));
}

#[test]
fn second_explicit_scan_does_not_overwrite_adopted_default_path() {
    let first_dir = tempdir().unwrap();
    let second_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(first_dir.path().join("a.jpg"), b"content").unwrap();
    fs::write(second_dir.path().join("b.jpg"), b"other content").unwrap();

    Command::new(videre_bin())
        .arg("scan").arg("--silent").arg(first_dir.path())
        .env("VIDERE_HOME", home.path())
        .status().unwrap();

    let out = Command::new(videre_bin())
        .arg("scan").arg("--silent").arg(second_dir.path())
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(out.status.success());
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
        .arg("scan").arg("--silent").arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).trim().is_empty(), "{}", String::from_utf8_lossy(&out.stderr));

    let config = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(String::from_utf8_lossy(&config.stdout)
        .contains(&scan_dir.path().display().to_string()));
}

#[test]
fn json_error_object_for_missing_directory() {
    let home = tempdir().unwrap();
    let out = Command::new(videre_bin())
        .arg("scan")
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
}

#[test]
fn json_mode_reports_scan_shape_and_adopts_default_path() {
    let scan_dir = tempdir().unwrap();
    let home = tempdir().unwrap();
    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();
    fs::write(scan_dir.path().join("b.jpg"), b"other content").unwrap();

    let out = Command::new(videre_bin())
        .arg("scan").arg("--silent").arg("--json").arg(scan_dir.path())
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("stdout must remain pure JSON even when adopting a default path");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["total_files"], 2);
    assert_eq!(doc["output"]["kind"], "sqlite");
    let expected_db = home.path().join("hashes.db").display().to_string();
    assert_eq!(doc["output"]["path"], expected_db);

    let config = Command::new(videre_bin())
        .arg("config")
        .env("VIDERE_HOME", home.path())
        .output().unwrap();
    assert!(String::from_utf8_lossy(&config.stdout)
        .contains(&scan_dir.path().display().to_string()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --test scan`
Expected: FAIL to compile (`tests/scan.rs` exists but `cargo test` runs it as a new test binary; the `scan` subcommand doesn't exist yet, so every test that spawns it gets a clap "unrecognized subcommand" failure at runtime, and the file itself compiles fine since it only calls the `videre_bin()` binary, not any Rust API). Expected: all tests RUN and FAIL (nonzero-exit assertions fail where the test expects `.success()`, or the subcommand-not-found error surfaces on stderr instead of the expected messages).

- [ ] **Step 3: Implement**

Create `crates/videre/src/commands/scan.rs`:

```rust
use videre::{
    hasher, output, scanner, sqlite_output,
    types::{ErrorJson, ScanJson, ScanOutputJson, SCHEMA_VERSION},
};
use rayon::prelude::*;
use std::path::PathBuf;
use std::process;

#[derive(clap::Args)]
pub struct ScanArgs {
    /// Directory to scan recursively (default: 'path' from videre config)
    directory: Option<PathBuf>,

    /// JSONL output file (appended). Bare --output targets ~/.videre/hashes.jsonl.
    /// Note: place a bare --output AFTER the directory. Cannot be used with --output-sqlite
    #[arg(long, num_args = 0..=1, conflicts_with = "output_sqlite")]
    output: Option<Option<PathBuf>>,

    /// SQLite output file (upserted by path). When neither --output nor
    /// --output-sqlite is given, records go to the resolved default db
    #[arg(long)]
    output_sqlite: Option<PathBuf>,

    /// Also compute and store perceptual hashes for near-duplicate detection
    #[arg(long)]
    similar: bool,

    /// Suppress progress output on stderr
    #[arg(long)]
    silent: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

pub fn run(args: ScanArgs) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                process::exit(1);
            }
        }
    } else {
        run_text(args)
    }
}

/// Scan, hash (in parallel), and optionally phash. Shared by both output modes;
/// contains no exit calls so the JSON path can also use it. Progress and
/// warnings go to stderr, gated by --silent.
fn gather_records(args: &ScanArgs, directory: &std::path::Path) -> Vec<videre::types::FileRecord> {
    if !args.silent {
        eprintln!("Scanning {:?}...", directory);
    }

    let paths = scanner::scan(directory);

    if !args.silent {
        eprintln!("Found {} file(s) to process", paths.len());
    }

    let silent = args.silent;
    let records: Vec<_> = paths
        .par_iter()
        .filter_map(|path| {
            hasher::hash_file(path)
                .map_err(|e| {
                    if !silent {
                        eprintln!("Warning: skipping {:?}: {}", path, e);
                    }
                })
                .ok()
        })
        .collect();

    if args.similar {
        records
            .into_iter()
            .map(|mut r| {
                r.phash = hasher::compute_dhash(std::path::Path::new(&r.path));
                r
            })
            .collect()
    } else {
        records
    }
}

enum OutputTarget {
    Sqlite(PathBuf),
    Jsonl(PathBuf),
}

/// Where records go. Explicit flags behave exactly as before; the bare default
/// is SQLite at the resolved db, and a bare --output is JSONL at the default
/// jsonl path. Defaulted destinations get their parent dir created (that is
/// how ~/.videre comes into existence on first use).
fn output_target(args: &ScanArgs) -> anyhow::Result<OutputTarget> {
    if let Some(ref db) = args.output_sqlite {
        return Ok(OutputTarget::Sqlite(db.clone()));
    }
    match &args.output {
        Some(Some(path)) => Ok(OutputTarget::Jsonl(path.clone())),
        Some(None) => {
            let path = videre_core::home::default_jsonl()?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(OutputTarget::Jsonl(path))
        }
        None => {
            let db = videre_core::home::resolve_db(None)?;
            if let Some(parent) = db.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Ok(OutputTarget::Sqlite(db))
        }
    }
}

/// Text mode: stdout is always empty (progress is on stderr; duplicate
/// reporting is `dedupe`'s job now, not scan's).
fn run_text(args: ScanArgs) -> anyhow::Result<()> {
    let directory = match super::resolve_directory(args.directory.clone()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };
    if !directory.exists() {
        eprintln!("Error: directory {:?} does not exist", directory);
        process::exit(1);
    }
    super::maybe_adopt_default_path(args.directory.as_deref(), args.silent);

    let records = gather_records(&args, &directory);

    match output_target(&args) {
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
        Ok(OutputTarget::Sqlite(db_path)) => {
            if let Err(e) = sqlite_output::write_records(&records, &db_path) {
                eprintln!("Error writing to {:?}: {}", db_path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
            }
        }
        Ok(OutputTarget::Jsonl(path)) => {
            if let Err(e) = output::append_records(&records, &path) {
                eprintln!("Error writing to {:?}: {}", path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), path);
            }
        }
    }

    Ok(())
}

/// JSON mode: identical pipeline, but every failure becomes Err so run() can
/// emit the error JSON document (text mode's process::exit paths would
/// otherwise kill the process with empty stdout).
fn run_json(args: &ScanArgs) -> anyhow::Result<ScanJson> {
    let directory = super::resolve_directory(args.directory.clone())?;
    anyhow::ensure!(
        directory.exists(),
        "directory {:?} does not exist",
        directory
    );
    super::maybe_adopt_default_path(args.directory.as_deref(), args.silent);

    let records = gather_records(args, &directory);

    let output = match output_target(args)? {
        OutputTarget::Sqlite(db_path) => {
            sqlite_output::write_records(&records, &db_path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))?;
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
            }
            ScanOutputJson { kind: "sqlite", path: db_path.display().to_string() }
        }
        OutputTarget::Jsonl(path) => {
            output::append_records(&records, &path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", path, e))?;
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), path);
            }
            ScanOutputJson { kind: "jsonl", path: path.display().to_string() }
        }
    };

    Ok(ScanJson {
        schema_version: SCHEMA_VERSION,
        total_files: records.len(),
        output,
    })
}
```

Wire in: `crates/videre/src/commands/mod.rs` gains `pub mod scan;` (alphabetical: after `report`, before `search`). `crates/videre/src/main.rs`'s `Command` enum gains, after `Report`:

```rust
    /// Scan a directory, hash every image, and populate the database
    Scan(commands::scan::ScanArgs),
```

and the dispatch arm, after `Command::Report`:

```rust
        Command::Scan(args) => commands::scan::run(args),
```

(Leave the existing `Dedupe` variant, `dedupe.rs`, and `tests/integration.rs` completely untouched in this task; Task 3 handles them.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --test scan`
Expected: PASS (15 tests).

Run: `cargo test --workspace`
Expected: PASS, 197 total (182 + 15). No compiler warnings.

Verify by hand: `cargo run -q -p videre --bin videre -- scan --help` shows the flags; `cargo run -q -p videre --bin videre -- dedupe --help` still shows the OLD dedupe flags (directory positional, `--output`, etc.) since `dedupe.rs` is untouched.

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/scan.rs crates/videre/src/commands/mod.rs crates/videre/src/main.rs crates/videre/tests/scan.rs
git commit -m "feat: videre scan subcommand absorbs ingestion; dedupe still works unchanged for now"
```

---

### Task 3: `dedupe` becomes a pure reader

Rewrites `dedupe.rs` from scratch. Adds `resolve_reader_db_must_exist` and `build_find_duplicates` to `commands/mod.rs` (the first consumers). Retargets `tests/integration.rs` from scanning tests (now covered by `tests/scan.rs`) to duplicate-reporting tests via a two-step `scan` then `dedupe` flow. Deletes `DedupeJson` from `types.rs` (its last user, old `dedupe.rs`, is gone) and retargets its two tests to `FindDuplicatesJson`.

**Files:**
- Modify: `crates/videre/src/commands/dedupe.rs`, `crates/videre/src/commands/mod.rs`, `crates/videre/src/types.rs`, `crates/videre/tests/integration.rs`

- [ ] **Step 1: Write the failing tests**

Replace the entire contents of `crates/videre/tests/integration.rs` with:

```rust
use std::process::Command;
use tempfile::tempdir;

fn videre_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("videre");
    path
}

/// Runs `scan <dir> --output-sqlite <db> --silent [extra_scan_args...]`, then
/// returns the db path. Fails the test via panic if the scan itself fails.
fn scan_into_db(dir: &std::path::Path, db: &std::path::Path, extra: &[&str]) {
    let mut cmd = Command::new(videre_bin());
    cmd.arg("scan").arg("--silent").arg("--output-sqlite").arg(db);
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
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "exactly one REMOVE candidate expected: {stdout}");
    assert!(lines[0].ends_with("a.jpg") || lines[0].ends_with("b.jpg"));
}

#[test]
fn dedupe_rejects_a_directory_positional() {
    let out = Command::new(videre_bin())
        .arg("dedupe")
        .arg("/some/directory")
        .output()
        .expect("failed to run videre dedupe");
    assert!(!out.status.success(), "dedupe must not accept a directory argument");
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
    assert!(pair[0].ends_with("a.jpg") && pair[1].ends_with("b.jpg"),
        "keep+remove must be exactly the identical pair, got {pair:?}");
    assert!(keep != removed);

    assert!(doc.get("similar_groups").is_none(),
        "similar_groups key must be absent without --similar");
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
            reader.read_line(&mut line).expect("read from server");
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --test integration`
Expected: FAIL (old `dedupe.rs` still has a `directory` positional, no `--db` flag, and returns `scanned` not `total_files` in `--json`; `dedupe_rejects_a_directory_positional` fails because today's dedupe DOES accept a directory; `dedupe_explicit_db_must_exist` fails because `--db` doesn't exist as a flag yet).

- [ ] **Step 3: Implement**

Add to `crates/videre/src/commands/mod.rs`, after `resolve_reader_db`:

```rust
/// Like `resolve_reader_db`, but checks existence even for an explicit path.
/// Used by commands that bind to one db for their whole session (mcp) or that
/// have no separate ingestion step to fall back on (dedupe, after the
/// scan/dedupe split): a typo'd explicit path should fail loudly, not
/// silently serve or create an empty database.
pub(crate) fn resolve_reader_db_must_exist(
    explicit: Option<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    let db = match explicit {
        Some(p) => p,
        None => videre_core::home::resolve_db(None)?,
    };
    anyhow::ensure!(
        db.exists(),
        "no database found at {}; run 'videre scan <dir>' first",
        db.display()
    );
    Ok(db)
}
```

Change `resolve_reader_db`'s hint string from `run 'videre dedupe <dir>' first` to `run 'videre scan <dir>' first` (the only change to that function; its existing looser explicit-path behavior is untouched):

```rust
pub(crate) fn resolve_reader_db(
    explicit: Option<std::path::PathBuf>,
) -> anyhow::Result<std::path::PathBuf> {
    match explicit {
        Some(p) => Ok(p),
        None => {
            let db = videre_core::home::resolve_db(None)?;
            anyhow::ensure!(
                db.exists(),
                "no database found at {}; run 'videre scan <dir>' first",
                db.display()
            );
            Ok(db)
        }
    }
}
```

Add `build_find_duplicates`, lifted from `mcp.rs` (which still has its own private copy for now; Task 4 deletes it there and switches `mcp.rs` to call this one):

```rust
/// Shared by `dedupe --json` and the MCP `find_duplicates` tool so the two
/// surfaces cannot silently diverge in shape.
pub(crate) fn build_find_duplicates(
    db: &std::path::Path,
    include_similar: bool,
) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let records = videre::sqlite_output::load_records(db)?;
    let total_files = records.len();
    let duplicate_groups = videre::output::find_duplicate_groups(&records)
        .into_iter()
        .map(videre::types::DupGroupJson::from)
        .collect();
    let similar_groups = include_similar.then(|| {
        videre::output::find_similar_groups(&records, 10)
            .into_iter()
            .map(videre::types::SimilarGroupJson::from)
            .collect()
    });
    Ok(videre::types::FindDuplicatesJson {
        schema_version: videre::types::SCHEMA_VERSION,
        total_files,
        duplicate_groups,
        similar_groups,
    })
}
```

Replace the entire contents of `crates/videre/src/commands/dedupe.rs`:

```rust
use videre::types::ErrorJson;
use std::path::PathBuf;
use std::process;

#[derive(clap::Args)]
pub struct DedupeArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Also report perceptual-hash near-duplicate clusters (review-only)
    #[arg(long)]
    similar: bool,

    /// Suppress progress output on stderr (duplicate paths are always written to stdout)
    #[arg(long)]
    silent: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

pub fn run(args: DedupeArgs) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                process::exit(1);
            }
        }
    } else {
        run_text(args)
    }
}

fn run_text(args: DedupeArgs) -> anyhow::Result<()> {
    let db = match super::resolve_reader_db_must_exist(args.db) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };

    let records = match videre::sqlite_output::load_records(&db) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error reading {:?}: {}", db, e);
            process::exit(1);
        }
    };

    let groups = videre::output::find_duplicate_groups(&records);
    if !args.silent {
        if groups.is_empty() {
            eprintln!("No exact duplicates found.");
        } else {
            eprintln!(
                "{} duplicate group(s), {} file(s) to remove.",
                groups.len(),
                groups.iter().map(|g| g.files.len() - 1).sum::<usize>()
            );
        }
    }
    videre::output::print_losers(&groups);

    if args.similar {
        let similar = videre::output::find_similar_groups(&records, 10);
        if !args.silent && !similar.is_empty() {
            eprintln!(
                "{} visually similar group(s) found: review with videre report before deleting.",
                similar.len()
            );
        }
    }

    Ok(())
}

fn run_json(args: &DedupeArgs) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let db = super::resolve_reader_db_must_exist(args.db.clone())?;
    super::build_find_duplicates(&db, args.similar)
}
```

Note: `run_text` calls `find_duplicate_groups`/`find_similar_groups` directly on `load_records`'s output (not through `build_find_duplicates`, since text mode prints plain paths, not JSON); `run_json` calls the shared `build_find_duplicates` directly. This matches the spec's "text and json modes both start from `load_records`, diverging only in what they do with the groups" description.

In `crates/videre/src/types.rs`: delete the `DedupeJson` struct entirely (its doc comment, the struct, nothing else references it after this task). Retarget its two tests:

```rust
    #[test]
    fn dedupe_json_omits_similar_groups_when_none() {
```
becomes
```rust
    #[test]
    fn find_duplicates_json_via_dedupe_omits_similar_groups_when_none() {
```
with body changed from `DedupeJson { schema_version: SCHEMA_VERSION, scanned: 3, duplicate_groups: vec![], similar_groups: None }` to `FindDuplicatesJson { schema_version: SCHEMA_VERSION, total_files: 3, duplicate_groups: vec![], similar_groups: None }`, same assertions.

And:
```rust
    #[test]
    fn dedupe_json_includes_similar_groups_when_some() {
```
becomes
```rust
    #[test]
    fn find_duplicates_json_via_dedupe_includes_similar_groups_when_some() {
```
with `DedupeJson { schema_version: SCHEMA_VERSION, scanned: 2, ... }` changed to `FindDuplicatesJson { schema_version: SCHEMA_VERSION, total_files: 2, ... }`, same assertions.

(These two retargeted tests now assert the same thing as Task 1's `find_duplicates_json_omits_similar_groups_when_none` / `find_duplicates_json_includes_similar_groups_when_some`. Keep both pairs: retarget these two in place under their new names rather than deleting them, so the file ends up with 4 tests covering `FindDuplicatesJson`'s serialization, not 2. This is deliberate, not an oversight - it is cheaper and less error-prone to rename in place than to verify a deletion removed exactly the right two tests and no others.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --lib types::`
Expected: PASS (11 tests: the same 11 as at the end of Task 1 - the two `DedupeJson` tests were renamed and retargeted to `FindDuplicatesJson` in place, not added or removed).

Run: `cargo test -p videre --test integration`
Expected: PASS (7 tests).

Run: `cargo test --workspace`
Expected: PASS, 187 total (197 at the end of Task 2, minus the 17 old `tests/integration.rs` tests that were fully replaced, plus the 7 new ones; the `types.rs` test count is unchanged since Task 1, at 11).

Verify by hand:
```bash
cargo run -q -p videre --bin videre -- dedupe --help
# expect: --db, --similar, --silent, --json; no directory positional
```

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/dedupe.rs crates/videre/src/commands/mod.rs crates/videre/src/types.rs crates/videre/tests/integration.rs
git commit -m "feat: dedupe becomes a pure database reader; scan owns ingestion"
```

---

### Task 4: `mcp.rs` uses the shared `build_find_duplicates`

Pure refactor: `mcp.rs`'s private `FindDuplicatesJson`/`build_find_duplicates` are deleted in favor of the shared ones from Task 3. Zero behavior change, regression-guarded by the existing `find_duplicates_tool_returns_keep_remove_groups` test. Also simplifies `mcp.rs`'s `run()` (drops its now-redundant manual existence check) and updates its two error-hint strings.

**Files:**
- Modify: `crates/videre/src/commands/mcp.rs`

- [ ] **Step 1: Confirm the regression guard exists and currently passes**

Run: `cargo test -p videre --test mcp find_duplicates_tool_returns_keep_remove_groups`
Expected: PASS (this test already exists from the earlier MCP plan; it is the guard for this refactor, not a new test).

- [ ] **Step 2: Implement**

In `crates/videre/src/commands/mcp.rs`:

Delete the `FindDuplicatesJson` struct (lines currently reading `#[derive(Debug, Serialize)] struct FindDuplicatesJson { ... }`) and the `build_find_duplicates` function entirely. Keep `FindDuplicatesParams` (the tool's input schema type - unrelated to the output type, still local to `mcp.rs`).

Update the `find_duplicates` tool method's body to call the shared function:

```rust
    async fn find_duplicates(
        &self,
        Parameters(params): Parameters<FindDuplicatesParams>,
    ) -> Result<CallToolResult, McpError> {
        let db = self.db.clone();
        match blocking(move || super::build_find_duplicates(&db, params.include_similar)).await? {
            Ok(doc) => json_result(&doc),
            Err(e) => Ok(tool_error(&e)),
        }
    }
```

(The `#[tool(...)]` attribute and doc comment above this method are unchanged.)

Simplify `run()`'s db resolution to use the new stricter helper directly, dropping the now-redundant manual `anyhow::ensure!`:

```rust
pub fn run(args: McpArgs) -> Result<()> {
    let db = super::resolve_reader_db_must_exist(args.db)?;
    eprintln!("videre mcp: serving {}", db.display());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let service = VidereServer::new(db).serve(stdio()).await?;
        service.waiting().await?;
        Ok(())
    })
}
```

Update `get_info()`'s instructions string, changing `"...run 'videre dedupe'/'videre watch' to freshen."` to `"...run 'videre scan'/'videre watch' to freshen."`:

```rust
            .with_instructions(
                "Read-only query tools over a videre media library (SQLite). \
                 Results reflect the last scan; verify paths still exist before \
                 acting on them, and run 'videre scan'/'videre watch' to freshen.",
            )
```

Check the file's `use` block: `videre::types::SCHEMA_VERSION` is still needed (used by `StatsJson`'s construction); `videre::types::{DupGroupJson, SimilarGroupJson}` are no longer referenced directly in this file (they were only used inside the now-deleted `build_find_duplicates`) - remove them from any `use` statement if present, to avoid an unused-import warning. `videre::sqlite_output` and `videre::output` may also become unused in this file if `build_find_duplicates` was their only caller here - remove those imports too if the compiler flags them.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p videre --test mcp`
Expected: PASS (all mcp tests, same count as before this task - the refactor changes no observable behavior).

Run: `cargo test --workspace`
Expected: PASS, same total as the end of Task 3 (no tests added or removed here). Zero compiler warnings (`cargo build --workspace 2>&1 | grep -i warning` empty).

- [ ] **Step 4: Commit**

```bash
git add crates/videre/src/commands/mcp.rs
git commit -m "refactor: mcp find_duplicates tool uses the shared build_find_duplicates"
```

---

### Task 5: Remaining mechanical error-hint and doc-string updates

Five spots not already covered by Tasks 2-4 (which already fixed `commands/mod.rs`'s two hint strings, `main.rs`'s `Scan` doc comment, and `mcp.rs`'s two strings).

**Files:**
- Modify: `crates/videre/src/commands/mod.rs`, `crates/videre/src/commands/watch.rs`, `crates/videre/src/main.rs`
- Test: `crates/videre/tests/prune.rs`, `crates/videre/tests/watch.rs`

- [ ] **Step 1: Write the failing test change**

In `crates/videre/tests/prune.rs`, change the `missing_default_db_prints_friendly_error` test's assertion from:

```rust
    assert!(stderr.contains("videre dedupe"), "{stderr}");
```

to:

```rust
    assert!(stderr.contains("videre scan"), "{stderr}");
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre --test prune missing_default_db_prints_friendly_error`
Expected: FAIL (the hint string still says `videre dedupe` at this point, from `resolve_reader_db`... wait, `resolve_reader_db`'s string was already changed to `scan` in Task 3. Re-run and confirm: if Task 3 already fixed this, this test should ALREADY PASS. Run it first before changing anything; if it already passes, skip straight to Step 4 with no code change needed here, and note in your report that this step was a no-op because Task 3's `resolve_reader_db` change already covered it.)

- [ ] **Step 3: Implement remaining spots**

Update the doc comment on `maybe_adopt_default_path` in `crates/videre/src/commands/mod.rs`, changing:

```rust
/// First-use convenience for `dedupe`: if the caller gave an explicit
/// directory and no default path is configured yet, adopt it as the default
/// so future bare `videre dedupe` / `videre watch` calls need no argument.
```

to:

```rust
/// First-use convenience for `scan`: if the caller gave an explicit
/// directory and no default path is configured yet, adopt it as the default
/// so future bare `videre scan` / `videre watch` calls need no argument.
```

Update `crates/videre/src/commands/watch.rs`'s missing-table hint, changing:

```rust
                    "videre watch: file_hashes table not found - run 'videre dedupe --output-sqlite <db> <dir>' or 'videre watch --scan ...' first"
```

to:

```rust
                    "videre watch: file_hashes table not found - run 'videre scan --output-sqlite <db> <dir>' or 'videre watch --scan ...' first"
```

In `crates/videre/src/main.rs`, fix the `Dedupe` variant's now-wrong doc comment (it still describes the old scan-and-report behavior), changing:

```rust
    /// Scan a directory, hash every image, and print duplicate paths to stdout
    Dedupe(commands::dedupe::DedupeArgs),
```

to:

```rust
    /// Report duplicate files from the database and print paths to remove
    Dedupe(commands::dedupe::DedupeArgs),
```

In `crates/videre/tests/watch.rs`, update the comment (no assertion behind it, purely for accuracy):

```rust
    // running `videre watch --faces` before any `videre dedupe`/`videre watch --scan`
```

to:

```rust
    // running `videre watch --faces` before any `videre scan`/`videre watch --scan`
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --test prune --test watch`
Expected: PASS (all tests in both files).

Run: `cargo test --workspace`
Expected: PASS, same total as end of Task 4. Zero compiler warnings.

Verify by hand: `cargo run -q -p videre --bin videre -- --help` shows `Dedupe`'s new one-line description.

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/mod.rs crates/videre/src/commands/watch.rs crates/videre/src/main.rs crates/videre/tests/prune.rs crates/videre/tests/watch.rs
git commit -m "docs: remaining dedupe to scan references in hints and comments"
```

---

### Task 6: Documentation (README.md, CLAUDE.md)

**Files:**
- Modify: `README.md`, `CLAUDE.md`

Read both files fully first (they are long) and match existing style, heading conventions, and factual density. Verify every claim against the real binary (`cargo run -q -p videre --bin videre -- <cmd> --help`).

- [ ] **Step 1: Update README.md**

Subcommands table (near the top): add a `videre scan` row before the `videre dedupe` row, and change the `videre dedupe` row's purpose text:

```
| `videre scan` | Scan a directory, hash every image, and populate the database |
| `videre dedupe` | Report duplicate files from the database, print paths to remove |
```

Quickstart / early examples (`videre dedupe ~/Photos`, `videre dedupe ~/Photos | xargs trash`): change to a two-step form:

```
videre scan ~/Photos
videre dedupe | xargs trash
```

The `videre dedupe --output-sqlite ~/photos.db ~/Photos` example: this flag no longer exists on `dedupe`. Change to:

```
videre scan --output-sqlite ~/photos.db ~/Photos
videre dedupe --db ~/photos.db
```

The `no database found at <path>; run 'videre dedupe <dir>' first` line: change to `run 'videre scan <dir>' first`.

The `## videre dedupe` section (currently documents scanning flags): split into two sections, `## videre scan` (directory, `--output`/`--output-sqlite`, `--similar`, `--silent`, `--json`, using the flag docs from `crates/videre/src/commands/scan.rs`) and `## videre dedupe` (now `--db`, `--similar`, `--silent`, `--json`). Update every example in this area:

```
videre scan ~/Photos                                          # populate the default db
videre dedupe                                                 # preview removals
videre dedupe | xargs trash                                   # delete immediately
videre dedupe --silent > to_delete.txt                        # save list for later
videre scan --output ~/Photos                                 # write JSONL to ~/.videre/hashes.jsonl
videre scan --output-sqlite ~/photos.db ~/Photos               # scan to an explicit db
videre dedupe --db ~/photos.db --similar                       # explicit db, include visual duplicates
```

The "Phase 1" note referencing `videre dedupe`: change "Run immediately after `videre dedupe`" to "Run `videre scan` then `videre dedupe`".

The `--scan` flag doc on `watch` ("same as running `videre dedupe`"): change to "same as running `videre scan`".

Add one line to the existing "Breaking changes" list (find it near the other three deliberate breaking changes already documented): "4. `videre dedupe` no longer scans a directory; it only reads the database. Run `videre scan <dir>` first (or rely on `videre watch`), then `videre dedupe`."

- [ ] **Step 2: Update CLAUDE.md**

Subcommand count and enumeration (near the top, "What it does" paragraph): update from "ten subcommands" to "eleven subcommands", and split the `videre dedupe` description:

```
`videre` is a single binary with eleven subcommands. `videre scan` scans a directory
recursively, hashes every image file (BLAKE3), and writes the results into the database
(or JSONL with `--output`). `videre dedupe` reads that database and writes REMOVE
candidates to stdout one per line: ready to pipe into `trash` or `rm`. Bare `videre scan
<dir>` writes SQLite to the resolved default database (see `~/.videre` below); JSONL
output requires `--output`. `videre report` reads the SQLite database and generates an
HTML review page (or serves a live web UI). The remaining subcommands (`fix-dates`,
`prune`, `embed`, `search`, `faces`, `watch`) operate on the same SQLite database to fix
timestamps, sync metadata, compute semantic embeddings, run text/image/person search,
and detect/label faces. `videre config` shows or edits the resolved paths and
`~/.videre/config.toml` settings. `videre mcp` serves read-only search/find_duplicates/
stats tools over stdio for LLM agents.
```

Usage block: replace `videre dedupe [OPTIONS] [directory]   # directory optional when 'path' is set in videre config` with:

```
videre scan [OPTIONS] [directory]     # directory optional when 'path' is set in videre config
videre dedupe [OPTIONS]               # reads the database; no directory argument
```

"Output behavior" section: the sentence `Bare 'videre dedupe <dir>' writes SQLite to the resolved default database (no JSONL). JSONL output only happens when --output is passed, with or without a value.` changes `dedupe` to `scan` throughout.

"Build & run" examples: change

```
./target/release/videre dedupe ~/Photos                                  # preview removals, writes SQLite to the default db
./target/release/videre dedupe ~/Photos | xargs trash                    # delete duplicates
./target/release/videre dedupe --output-sqlite ~/photos.db ~/Photos      # scan to an explicit SQLite db
```

to

```
./target/release/videre scan ~/Photos                                    # populate the default db
./target/release/videre dedupe | xargs trash                             # delete duplicates
./target/release/videre scan --output-sqlite ~/photos.db ~/Photos        # scan to an explicit SQLite db
./target/release/videre dedupe --db ~/photos.db                          # read from an explicit db
```

The database resolution paragraph: `"no database found at <path>; run 'videre dedupe <dir>' first"` changes to `run 'videre scan <dir>' first`. Also update the reader enumeration in that same paragraph: `--db` on the seven readers - `report`, `fix-dates`, `prune`, `embed`, `search`, `faces`, `mcp` needs `dedupe` added, becoming eight readers: `report`, `fix-dates`, `prune`, `embed`, `search`, `faces`, `mcp`, `dedupe`. The writers list (`--output-sqlite` on the two writers - `dedupe`, `watch`) drops `dedupe`, becoming one writer: `scan` (and `watch` still writes via its own `--output-sqlite`), so: `--output-sqlite` on the two writers - `scan`, `watch`.

The `videre config set path` paragraph mentioning `videre dedupe <dir>` adopting the default path: change every `videre dedupe` reference in this paragraph to `videre scan` (the adoption logic moved there in Task 2).

Other scattered `videre dedupe` mentions to check and update: the `location_name` migration note ("not populated by the initial `videre dedupe` scan" becomes "not populated by the initial `videre scan`"), the `videre watch` section's opening paragraph ("without anyone manually re-running `videre dedupe`, `videre faces`" becomes "`videre scan`, `videre faces`"), the `--scan` flag doc on watch ("same scan/hash/EXIF pipeline as `videre dedupe`" becomes "as `videre scan`"), and the `mcp` section's "even an explicit `--db` to a nonexistent path fails... `run 'videre dedupe <dir>' first`" becomes "`run 'videre scan <dir>' first`".

Project structure section: add `src/commands/scan.rs` to the videre crate's file list (alphabetically, after `report.rs`), and `tests/scan.rs` to the test-file list.

- [ ] **Step 3: Verify and commit**

```bash
cargo run -q -p videre --bin videre -- scan --help
cargo run -q -p videre --bin videre -- dedupe --help
cargo run -q -p videre --bin videre -- --help
```

All three must succeed and be consistent with what you documented. Then grep for anything missed:

```bash
grep -n "videre dedupe" README.md CLAUDE.md
```

Every remaining hit must be either a `videre dedupe <db-flag-or-no-args>` example that is actually correct post-split (i.e. `dedupe` with no directory, or with `--db`), or the intentional historical note in the "Breaking changes" section referencing the OLD behavior for context. Any hit showing `videre dedupe <dir>` or `videre dedupe --output...` is a miss; fix it.

```bash
git add README.md CLAUDE.md
git commit -m "docs: document the scan/dedupe split across README and CLAUDE.md"
```

---

### Task 7: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full suite**

Run: `cargo test --workspace`
Expected: PASS, 0 failed, 0 compiler warnings (`cargo build --workspace 2>&1 | grep -i warning` empty).

- [ ] **Step 2: Release-binary smoke test**

```bash
cargo build --release
H=$(mktemp -d); D=$(mktemp -d)
printf same > "$D/a.jpg"; printf same > "$D/b.jpg"; printf different > "$D/c.jpg"

echo "--- old dedupe invocation must fail ---"
VIDERE_HOME=$H ./target/release/videre dedupe "$D"; echo "exit=$?"
# expected: clap error (unexpected argument), nonzero exit

echo "--- scan then dedupe ---"
VIDERE_HOME=$H ./target/release/videre scan --silent "$D"
echo "scan exit=$?"
ls "$H"
# expected: hashes.db present, no stdout from scan

VIDERE_HOME=$H ./target/release/videre dedupe
echo "dedupe exit=$?"
# expected: exactly one REMOVE path (a.jpg or b.jpg)

echo "--- dedupe --json vs mcp find_duplicates ---"
VIDERE_HOME=$H ./target/release/videre dedupe --json | python3 -m json.tool

echo "--- explicit nonexistent db ---"
VIDERE_HOME=$H ./target/release/videre dedupe --db "$H/nope.db"; echo "exit=$?"
# expected: "no database found at ...; run 'videre scan <dir>' first", exit 1

rm -rf "$H" "$D"
```

Every command's outcome must match its comment.

- [ ] **Step 3: Record results**

PASS/FAIL per step; any FAIL loops back to the owning task.
