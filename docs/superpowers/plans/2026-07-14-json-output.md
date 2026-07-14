# `--json` Output for `videre search` and `videre dedupe` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in `--json` flag to `videre search` and `videre dedupe` so agents can parse one guaranteed-valid JSON object from stdout (success or error) instead of scraping text.

**Architecture:** Per the spec at `docs/superpowers/specs/2026-07-14-json-output-design.md`. New `Serialize` result structs (dedupe's + shared error type in `crates/videre/src/types.rs`, search's local to `search.rs`). Each command's `run` branches once on `args.json`: the JSON path builds the struct and prints compact JSON (converting all failures to `Err` so even errors emit JSON), the text path stays byte-identical to today. Search's query logic is unified into one `collect_hits` used by both printers; dedupe shares record-gathering but keeps its text path's verbatim `eprintln!`/`process::exit` calls.

**Tech Stack:** Rust, existing `serde`/`serde_json` (already deps; `FileRecord` already derives `Serialize`). No new dependencies. Baseline: `cargo test --workspace` = 130 passing on `main` (HEAD `ebaecd2`).

**Behavior contract:** Without `--json`, every byte of stdout/stderr and every exit code is unchanged. With `--json`, stdout is exactly one compact JSON object always (success or `{"schema_version":1,"error":{"message":...}}` + exit 1); stderr keeps progress/summary lines as today.

---

### Task 1: JSON result types in `types.rs`

**Files:**
- Modify: `crates/videre/src/types.rs`

- [ ] **Step 1: Write the failing unit tests**

Append inside the existing `#[cfg(test)] mod tests` in `crates/videre/src/types.rs` (reuse its style; it builds `FileRecord`s inline). Add a small helper and four tests:

```rust
    fn rec(path: &str, hash: &str) -> FileRecord {
        FileRecord {
            path: path.to_string(),
            hash: hash.to_string(),
            size_bytes: 1,
            created_at: None,
            modified_at: None,
            ext: "jpg".to_string(),
            phash: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn dup_group_json_splits_keep_and_remove() {
        let group = DuplicateGroup {
            hash: "h".to_string(),
            files: vec![rec("/keep.jpg", "h"), rec("/rm1.jpg", "h"), rec("/rm2.jpg", "h")],
        };
        let json_group = DupGroupJson::from(group);
        assert_eq!(json_group.keep.path, "/keep.jpg");
        assert_eq!(json_group.remove.len(), 2);
        assert_eq!(json_group.remove[0].path, "/rm1.jpg");
    }

    #[test]
    fn dedupe_json_omits_similar_groups_when_none() {
        let doc = DedupeJson {
            schema_version: SCHEMA_VERSION,
            scanned: 3,
            duplicate_groups: vec![],
            similar_groups: None,
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(!json.contains("similar_groups"));
    }

    #[test]
    fn dedupe_json_includes_similar_groups_when_some() {
        let doc = DedupeJson {
            schema_version: SCHEMA_VERSION,
            scanned: 2,
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
    fn error_json_contains_schema_version_and_message() {
        let err = anyhow::anyhow!("root cause").context("outer");
        let doc = ErrorJson::from_err(&err);
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(json.contains("\"error\""));
        assert!(json.contains("outer"), "message must render the anyhow chain: {json}");
        assert!(json.contains("root cause"), "chain rendered with {{e:#}}: {json}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --lib types::`
Expected: COMPILE ERROR (`DupGroupJson`, `DedupeJson`, `SimilarGroupJson`, `ErrorJson`, `SCHEMA_VERSION` not found).

- [ ] **Step 3: Implement the types**

Add to `crates/videre/src/types.rs`, after the `DuplicateGroup` definition (note: `Serialize` is already imported at the top of the file):

```rust
/// Version of the --json output schema. Additive changes (new fields) do not
/// bump this; removals or renames would.
pub const SCHEMA_VERSION: u32 = 1;

/// One exact-duplicate group in `dedupe --json`: byte-identical files split
/// into the one to keep (oldest by the KEEP rule) and the rest to remove.
#[derive(Debug, Serialize)]
pub struct DupGroupJson {
    pub hash: String,
    pub keep: FileRecord,
    pub remove: Vec<FileRecord>,
}

impl From<DuplicateGroup> for DupGroupJson {
    fn from(group: DuplicateGroup) -> Self {
        let mut files = group.files.into_iter();
        let keep = files.next().expect("duplicate groups always have >= 2 files");
        DupGroupJson { hash: group.hash, keep, remove: files.collect() }
    }
}

/// One perceptual-hash near-duplicate group in `dedupe --json --similar`.
/// Deliberately a flat review cluster with no keep/remove split: these files
/// are NOT byte-identical, so no deletion is safe without human/agent judgment.
#[derive(Debug, Serialize)]
pub struct SimilarGroupJson {
    pub hash: String,
    pub files: Vec<FileRecord>,
}

impl From<DuplicateGroup> for SimilarGroupJson {
    fn from(group: DuplicateGroup) -> Self {
        SimilarGroupJson { hash: group.hash, files: group.files }
    }
}

/// Top-level document for `dedupe --json`.
#[derive(Debug, Serialize)]
pub struct DedupeJson {
    pub schema_version: u32,
    pub scanned: usize,
    pub duplicate_groups: Vec<DupGroupJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similar_groups: Option<Vec<SimilarGroupJson>>,
}

/// Error document: in --json mode stdout always carries exactly one valid JSON
/// object, so runtime failures are emitted as this instead of leaving stdout empty.
#[derive(Debug, Serialize)]
pub struct ErrorJson {
    pub schema_version: u32,
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub message: String,
}

impl ErrorJson {
    pub fn from_err(e: &anyhow::Error) -> Self {
        ErrorJson {
            schema_version: SCHEMA_VERSION,
            error: ErrorBody { message: format!("{e:#}") },
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --lib types::`
Expected: PASS (8 tests: 4 existing + 4 new).

Run: `cargo test --workspace`
Expected: PASS, 134 total (130 baseline + 4).

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/types.rs
git commit -m "feat: JSON output types for dedupe and shared error document (schema_version 1)"
```

---

### Task 2: `dedupe --json`

**Files:**
- Modify: `crates/videre/src/commands/dedupe.rs`
- Test: `crates/videre/tests/integration.rs`

- [ ] **Step 1: Write the failing integration tests**

Append to `crates/videre/tests/integration.rs` (the file already has `videre_bin()`, `tempfile::tempdir`, and uses `serde_json`):

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --test integration`
Expected: the 3 new tests FAIL (clap rejects the unknown `--json` flag, so exit is nonzero and stdout is empty / not JSON). The 6 existing tests still PASS.

- [ ] **Step 3: Implement**

Rewrite `crates/videre/src/commands/dedupe.rs` as follows. The text path is the current `run` body **verbatim** (same `eprintln!` strings, same `process::exit(1)` sites) with only the record-gathering block moved into a shared helper; the JSON path converts every failure to `Err`.

Add the flag to `DedupeArgs` after `silent`:

```rust
    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
```

Replace `pub fn run` and add the helpers. Imports: keep the existing
`use videre::{hasher, output, scanner, sqlite_output, types};` line unchanged and add one new line
below it:

```rust
use videre::types::{DedupeJson, DupGroupJson, ErrorJson, SimilarGroupJson, SCHEMA_VERSION};
```

```rust
pub fn run(args: DedupeArgs) -> anyhow::Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                // stdout must always carry exactly one valid JSON object; the
                // error goes here (not stderr) and we exit before main's eprintln.
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
/// warnings go to stderr, gated by --silent, exactly as before.
fn gather_records(args: &DedupeArgs) -> Vec<types::FileRecord> {
    if !args.silent {
        eprintln!("Scanning {:?}...", args.directory);
    }

    let paths = scanner::scan(&args.directory);

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

/// The pre-existing text mode, byte-identical: same stderr text, same
/// process::exit(1) sites, same stdout lines.
fn run_text(args: DedupeArgs) -> anyhow::Result<()> {
    if !args.directory.exists() {
        eprintln!("Error: directory {:?} does not exist", args.directory);
        process::exit(1);
    }

    let records = gather_records(&args);

    if let Some(ref db_path) = args.output_sqlite {
        if let Err(e) = sqlite_output::write_records(&records, db_path) {
            eprintln!("Error writing to {:?}: {}", db_path, e);
            process::exit(1);
        }
        if !args.silent {
            eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
        }
    } else {
        if let Err(e) = output::append_records(&records, &args.output) {
            eprintln!("Error writing to {:?}: {}", args.output, e);
            process::exit(1);
        }
        if !args.silent {
            eprintln!("Wrote {} record(s) to {:?}", records.len(), args.output);
        }
    }

    // Exact duplicates: print REMOVE candidates to stdout (one path per line)
    let groups = output::find_duplicate_groups(&records);
    if !args.silent {
        if groups.is_empty() {
            eprintln!("No exact duplicates found.");
        } else {
            eprintln!("{} duplicate group(s), {} file(s) to remove.",
                groups.len(),
                groups.iter().map(|g| g.files.len() - 1).sum::<usize>()
            );
        }
    }
    output::print_losers(&groups);

    // Similar groups: informational only: review via videre report before acting
    if args.similar {
        let similar = output::find_similar_groups(&records, 10);
        if !args.silent {
            if similar.is_empty() {
                eprintln!("No visually similar images found.");
            } else {
                eprintln!("{} visually similar group(s) found: review with videre report before deleting.", similar.len());
            }
        }
    }

    Ok(())
}

/// JSON mode: identical pipeline, but every failure becomes Err so run() can
/// emit the error JSON document (text mode's process::exit paths would
/// otherwise kill the process with empty stdout).
fn run_json(args: &DedupeArgs) -> anyhow::Result<DedupeJson> {
    anyhow::ensure!(
        args.directory.exists(),
        "directory {:?} does not exist",
        args.directory
    );

    let records = gather_records(args);

    if let Some(ref db_path) = args.output_sqlite {
        sqlite_output::write_records(&records, db_path)
            .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))?;
        if !args.silent {
            eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
        }
    } else {
        output::append_records(&records, &args.output)
            .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", args.output, e))?;
        if !args.silent {
            eprintln!("Wrote {} record(s) to {:?}", records.len(), args.output);
        }
    }

    let scanned = records.len();
    let duplicate_groups = output::find_duplicate_groups(&records)
        .into_iter()
        .map(DupGroupJson::from)
        .collect();
    let similar_groups = args.similar.then(|| {
        output::find_similar_groups(&records, 10)
            .into_iter()
            .map(SimilarGroupJson::from)
            .collect()
    });

    Ok(DedupeJson {
        schema_version: SCHEMA_VERSION,
        scanned,
        duplicate_groups,
        similar_groups,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --test integration`
Expected: PASS (9 tests: 6 existing + 3 new).

Also verify by hand that text mode is unchanged:

```bash
cargo run -q -p videre --bin videre -- dedupe --help   # shows the new --json flag
mkdir -p /tmp/jsontest && printf same > /tmp/jsontest/a.jpg && printf same > /tmp/jsontest/b.jpg
cargo run -q -p videre --bin videre -- dedupe --silent --output /tmp/jsontest.out /tmp/jsontest
# expected: exactly one path on stdout (the REMOVE candidate), as before
cargo run -q -p videre --bin videre -- dedupe --silent --output /tmp/jsontest.out --json /tmp/jsontest
# expected: one compact JSON line starting {"schema_version":1,...}
rm -rf /tmp/jsontest /tmp/jsontest.out
```

Run: `cargo test --workspace`
Expected: PASS, 137 total (134 + 3).

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/dedupe.rs crates/videre/tests/integration.rs
git commit -m "feat: dedupe --json emits duplicate groups as a single JSON document"
```

---

### Task 3: `search --json`

**Files:**
- Modify: `crates/videre/src/commands/search.rs`
- Test: `crates/videre/tests/person_search.rs`

Note on test strategy: the text/image query success path needs the SigLIP model (network download), so integration tests cover the `--person` success path and the text-query **error** path (which fails at `load_embeddings`, before any model load). The text-hit JSON shape is covered by unit tests on the serialization.

- [ ] **Step 1: Write the failing integration tests**

Append to `crates/videre/tests/person_search.rs`:

```rust
#[test]
fn person_search_json_outputs_document() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let out = Command::new(bin())
        .arg("search")
        .arg(&db)
        .arg("--person")
        .arg("Alice")
        .arg("--json")
        .output()
        .expect("failed to run videre search");
    assert!(out.status.success());
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
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let plain = Command::new(bin())
        .arg("search").arg(&db).arg("--person").arg("Alice").arg("--json")
        .output().expect("failed to run videre search");
    let with_scores = Command::new(bin())
        .arg("search").arg(&db).arg("--person").arg("Alice").arg("--json").arg("--scores")
        .output().expect("failed to run videre search");
    assert!(with_scores.status.success(), "--scores with --json must not be rejected");
    assert_eq!(plain.stdout, with_scores.stdout, "--scores must be a no-op under --json");
}

#[test]
fn search_json_error_is_json_object_on_stdout() {
    let dir = tempdir().unwrap();
    // Fresh DB with no tables: open_wal succeeds (SQLite creates the file),
    // then load_embeddings fails (no embeddings table): the reliable error trigger.
    let db = dir.path().join("empty.db");
    Connection::open(&db).unwrap();
    let out = Command::new(bin())
        .arg("search")
        .arg(&db)
        .arg("beach")
        .arg("--json")
        .output()
        .expect("failed to run videre search");
    assert!(!out.status.success(), "must exit nonzero");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("even on error, stdout must be one valid JSON object");
    assert_eq!(doc["schema_version"], 1);
    assert!(doc["error"]["message"].as_str().is_some());
    assert!(doc.get("results").is_none());
}
```

Check whether `person_search.rs` needs `serde_json` in scope: it is a dev-dependency of the `videre` crate already (used by `tests/integration.rs`), so `serde_json::Value` resolves with no Cargo.toml change.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre --test person_search`
Expected: 3 existing PASS, 3 new FAIL (clap rejects unknown `--json` flag).

- [ ] **Step 3: Implement**

Rewrite `crates/videre/src/commands/search.rs`. The query pipeline is unified into `collect_hits` (used by both modes; person hits carry no hash/score, text/image hits carry both), then each mode prints. Text output stays byte-identical: paths one per line; `--scores` prepends `{score:.4}\t` only for hits that have a score (person hits never do, matching today's behavior where the person branch ignores `--scores`).

```rust
use anyhow::{Context, Result};
use serde::Serialize;
use videre::types::{ErrorJson, SCHEMA_VERSION};
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, search};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct SearchArgs {
    /// SQLite database with embeddings (run videre embed first)
    db: PathBuf,

    /// Text query, e.g. "sunset on beach" (omit when using --image)
    query: Option<String>,

    /// Search by example image instead of text
    #[arg(long, conflicts_with = "query")]
    image: Option<PathBuf>,

    /// Return paths containing a named person (confirmed faces only)
    #[arg(long, conflicts_with = "query", conflicts_with = "image")]
    person: Option<String>,

    /// Number of results
    #[arg(short = 'k', long, default_value_t = 20)]
    top_k: usize,

    /// Prepend the cosine score to each output line (no-op with --json: score is always included)
    #[arg(long)]
    scores: bool,

    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct SearchJson {
    schema_version: u32,
    query: QueryJson,
    count: usize,
    results: Vec<SearchHitJson>,
}

#[derive(Debug, Serialize)]
struct QueryJson {
    kind: &'static str,
    value: String,
}

#[derive(Debug, Serialize)]
struct SearchHitJson {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f32>,
}

pub fn run(args: SearchArgs) -> Result<()> {
    if args.json {
        match run_json(&args) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                // stdout must always carry exactly one valid JSON object; the
                // error goes here (not stderr) and we exit before main's eprintln.
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                std::process::exit(1);
            }
        }
    } else {
        run_text(&args)
    }
}

fn run_text(args: &SearchArgs) -> Result<()> {
    let (_query, hits) = collect_hits(args)?;
    for hit in hits {
        match hit.score {
            Some(score) if args.scores => println!("{score:.4}\t{}", hit.path),
            _ => println!("{}", hit.path),
        }
    }
    Ok(())
}

fn run_json(args: &SearchArgs) -> Result<SearchJson> {
    let (query, results) = collect_hits(args)?;
    Ok(SearchJson {
        schema_version: SCHEMA_VERSION,
        query,
        count: results.len(),
        results,
    })
}

/// The single query pipeline behind both output modes. Person hits carry only
/// a path (person search returns bare paths, no hash/score); text and image
/// hits carry hash + cosine score, one entry per on-disk path of a matched hash.
fn collect_hits(args: &SearchArgs) -> Result<(QueryJson, Vec<SearchHitJson>)> {
    let conn = videre_core::db::open_wal(&args.db)
        .with_context(|| format!("open {}", args.db.display()))?;

    if let Some(name) = &args.person {
        let paths = videre_core::person_search::search_by_person(&conn, name, None)?;
        if paths.is_empty() {
            eprintln!("No confirmed photos found for person: {name}");
        }
        let hits = paths
            .into_iter()
            .map(|path| SearchHitJson { path, hash: None, score: None })
            .collect();
        return Ok((QueryJson { kind: "person", value: name.clone() }, hits));
    }

    let corpus_raw = embeddings::load_embeddings(&conn, model::MODEL_ID)?;
    anyhow::ensure!(
        !corpus_raw.is_empty(),
        "no embeddings found in {} for model {}; run videre embed first",
        args.db.display(),
        model::MODEL_ID
    );
    let corpus: Vec<(String, Vec<f32>)> = corpus_raw
        .into_iter()
        .map(|(hash, blob)| (hash, vectors::from_f16_bytes(&blob)))
        .collect();

    let embedder = model::Embedder::load(device::best_device())?;
    let (query_vec, query) = match (&args.query, &args.image) {
        (Some(text), None) => (
            embedder.embed_text(text)?,
            QueryJson { kind: "text", value: text.clone() },
        ),
        (None, Some(img)) => (
            model::embed_image_file(&embedder, img)?,
            QueryJson { kind: "image", value: img.display().to_string() },
        ),
        _ => anyhow::bail!("provide either a text query or --image <path>"),
    };

    let mut hits = Vec::new();
    for (hash, score) in search::top_k(&query_vec, &corpus, args.top_k) {
        for path in embeddings::paths_for_hash(&conn, &hash)? {
            hits.push(SearchHitJson {
                path,
                hash: Some(hash.clone()),
                score: Some(score),
            });
        }
    }
    Ok((query, hits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_hit_serializes_with_hash_and_score() {
        let doc = SearchJson {
            schema_version: SCHEMA_VERSION,
            query: QueryJson { kind: "text", value: "sunset".to_string() },
            count: 1,
            results: vec![SearchHitJson {
                path: "/a.jpg".to_string(),
                hash: Some("abc".to_string()),
                score: Some(0.5),
            }],
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.starts_with("{\"schema_version\":1"));
        assert!(json.contains("\"kind\":\"text\""));
        assert!(json.contains("\"hash\":\"abc\""));
        assert!(json.contains("\"score\":0.5"));
        assert!(json.contains("\"count\":1"));
    }

    #[test]
    fn person_hit_omits_hash_and_score_keys() {
        let doc = SearchJson {
            schema_version: SCHEMA_VERSION,
            query: QueryJson { kind: "person", value: "Alice".to_string() },
            count: 1,
            results: vec![SearchHitJson {
                path: "/a.jpg".to_string(),
                hash: None,
                score: None,
            }],
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(!json.contains("hash"));
        assert!(!json.contains("score"));
        assert!(json.contains("\"path\":\"/a.jpg\""));
    }
}
```

Type note: `top_k` returns `(String, f32)` pairs and today's text mode prints the `f32` with `{score:.4}`; `SearchHitJson.score` is therefore `Option<f32>` (not f64) so text formatting stays byte-identical. JSON numbers carry no f32/f64 distinction.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --test person_search`
Expected: PASS (6 tests: 3 existing + 3 new).

Run: `cargo test -p videre --bin videre`
Expected: PASS (12 unit tests: 10 existing + 2 new in `commands::search::tests`).

Also verify the text path by hand against the same DB shape the tests use, confirming unchanged line output:

```bash
cargo run -q -p videre --bin videre -- search --help   # shows --json; --scores help mentions the no-op
```

Run: `cargo test --workspace`
Expected: PASS, 142 total (137 + 3 integration + 2 unit).

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/search.rs crates/videre/tests/person_search.rs
git commit -m "feat: search --json emits results as a single JSON document; --scores becomes a no-op under it"
```

---

### Task 4: Documentation

**Files:**
- Modify: `README.md`, `CLAUDE.md`

- [ ] **Step 1: Document the flag in README.md**

In the `videre dedupe` options/usage section, add the `--json` flag line alongside the existing flags:

```
  --json                   Emit a single JSON object on stdout instead of text
```

In the `videre search` section, add the same flag line and one sentence noting `--scores` is a no-op under `--json` (score is always included).

Add a short subsection (place it near the search/dedupe reference material, matching the README's existing heading style):

```markdown
## JSON output (agentic use)

`videre search` and `videre dedupe` accept `--json`. With it, stdout is always exactly one
compact JSON object; progress stays on stderr (`--silent` suppresses it). Every document
starts with `"schema_version": 1`. On failure the object is
`{"schema_version":1,"error":{"message":"..."}}` and the exit code is nonzero, so callers can
always parse stdout first and then branch. `dedupe --json` reports exact duplicates as
`duplicate_groups` with a safe `keep`/`remove` split; with `--similar` it adds review-only
`similar_groups` (flat file clusters, no keep/remove: near-duplicates are not safe to
auto-delete). `search --json` returns per-path `results` with `hash` and `score` (omitted for
`--person` hits).
```

- [ ] **Step 2: Document in CLAUDE.md**

In the `videre embed / videre search` section, add a sentence: `videre search ... --json` emits a single JSON document (`schema_version`, `query`, `count`, `results` with per-path `hash`/`score`; `--person` hits carry `path` only). In the `videre dedupe` usage block, add the `--json` option line with the same one-line description as README. Add one sentence to the "Output behavior" section: with `--json`, stdout is one compact JSON object always (error object + nonzero exit on failure) instead of REMOVE-path lines.

- [ ] **Step 3: Verify claims against the binary and commit**

```bash
cargo run -q -p videre --bin videre -- dedupe --help | grep -- --json
cargo run -q -p videre --bin videre -- search --help | grep -- --json
```

Both must show the flag with the documented wording. Then:

```bash
git add README.md CLAUDE.md
git commit -m "docs: document --json output for search and dedupe"
```

---

### Task 5: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full suite**

Run: `cargo test --workspace`
Expected: PASS, 142 tests, 0 failed.

- [ ] **Step 2: Release-binary smoke test**

```bash
cargo build --release
D=$(mktemp -d); printf same > "$D/a.jpg"; printf same > "$D/b.jpg"
./target/release/videre dedupe --silent --output "$D/hashes" --json "$D" | python3 -m json.tool
# expected: parses; schema_version 1; one duplicate group
./target/release/videre dedupe --silent --json /nonexistent/dir; echo "exit=$?"
# expected: one JSON error line on stdout, exit=1
./target/release/videre dedupe --silent --output "$D/hashes" "$D"
# expected: exactly one plain path line (text contract intact)
rm -rf "$D"
```

- [ ] **Step 3: Record results**

PASS/FAIL per step; any FAIL loops back to the owning task.
