# Screenshot/Document Classification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add zero-shot photo/screenshot/document/meme classification over images already embedded by `videre embed`, queryable via a new `videre classify` subcommand and a `--category` mode on `videre search`.

**Architecture:** A new `classifications` table (hash-keyed, like `embeddings`/`faces`) stores one category + confidence per hash. `videre classify` embeds 4 fixed text prompts once via the existing SigLIP `Embedder::embed_text`, then scores every not-yet-classified embedding against them with plain dot products (vectors are already L2-normalized). A pure, TDD'd decision function picks the winner or falls back to `"unknown"` if the top two scores aren't clearly separated. No new model, no image re-loading - this is pure linear algebra over vectors that already exist.

**Tech Stack:** Rust, rusqlite, existing `videre-ml::model::Embedder` (SigLIP), clap. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-07-29-screenshot-document-classification-design.md` - read this first for full rationale; this plan implements it task-by-task.

---

### Task 1: `classifications` table + core module, TDD

**Files:**
- Create: `crates/videre-core/src/classify.rs`
- Modify: `crates/videre-core/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/videre-core/src/classify.rs` with just the test module first:

```rust
//! Classifications table: one row per unique content hash (photo/screenshot/
//! document/meme/unknown), keyed to embeddings.hash. Zero-shot classification
//! reuses embeddings `videre embed` already computed - see
//! docs/superpowers/specs/2026-07-29-screenshot-document-classification-design.md.

use rusqlite::{Connection, Result, params};

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY,
                hash TEXT NOT NULL
            );
            CREATE TABLE embeddings (
                hash        TEXT PRIMARY KEY,
                model_id    TEXT NOT NULL,
                embedding   BLOB NOT NULL,
                embedded_at TEXT NOT NULL
            );",
        )
        .unwrap();
        ensure_classifications_table(&conn).unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str, hash: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash) VALUES (?1, ?2)",
            rusqlite::params![path, hash],
        )
        .unwrap();
    }

    fn insert_embedding(conn: &Connection, hash: &str, model_id: &str) {
        conn.execute(
            "INSERT INTO embeddings (hash, model_id, embedding, embedded_at)
             VALUES (?1, ?2, X'00', datetime('now'))",
            rusqlite::params![hash, model_id],
        )
        .unwrap();
    }

    #[test]
    fn ensure_classifications_table_is_idempotent() {
        let conn = test_db();
        ensure_classifications_table(&conn).unwrap();
        ensure_classifications_table(&conn).unwrap();
    }

    #[test]
    fn pending_hashes_returns_embedded_but_unclassified() {
        let conn = test_db();
        insert_embedding(&conn, "h1", "m");
        insert_embedding(&conn, "h2", "m");
        insert_classifications(&conn, &[("h1".to_string(), "photo", 0.9)]).unwrap();

        let pending = pending_hashes(&conn, "m").unwrap();
        assert_eq!(pending, vec!["h2".to_string()]);
    }

    #[test]
    fn pending_hashes_is_model_aware() {
        let conn = test_db();
        insert_embedding(&conn, "h1", "model-a");

        assert_eq!(pending_hashes(&conn, "model-a").unwrap(), vec!["h1".to_string()]);
        assert!(pending_hashes(&conn, "model-b").unwrap().is_empty());
    }

    #[test]
    fn insert_classifications_upserts_on_conflict() {
        let conn = test_db();
        insert_classifications(&conn, &[("h1".to_string(), "screenshot", 0.4)]).unwrap();
        insert_classifications(&conn, &[("h1".to_string(), "photo", 0.9)]).unwrap();

        let category: String = conn
            .query_row("SELECT category FROM classifications WHERE hash = 'h1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(category, "photo");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1); // upsert, not a second row
    }

    #[test]
    fn paths_for_category_returns_matching_paths_with_hash() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1");
        insert_file(&conn, "/b/1-copy.jpg", "h1");
        insert_file(&conn, "/a/2.png", "h2");
        insert_classifications(
            &conn,
            &[
                ("h1".to_string(), "screenshot", 0.8),
                ("h2".to_string(), "photo", 0.9),
            ],
        )
        .unwrap();

        let hits = paths_for_category(&conn, "screenshot").unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|(_, hash)| hash == "h1"));
    }

    #[test]
    fn paths_for_category_returns_empty_for_unmatched_category() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1");
        insert_classifications(&conn, &[("h1".to_string(), "photo", 0.9)]).unwrap();

        assert!(paths_for_category(&conn, "meme").unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre-core classify:: -- --nocapture`
Expected: FAIL with "cannot find function `ensure_classifications_table`" (and similarly for the other functions) - compile errors, not assertion failures.

- [ ] **Step 3: Implement**

Add above the test module in `crates/videre-core/src/classify.rs`:

```rust
pub fn ensure_classifications_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS classifications (
            hash          TEXT PRIMARY KEY NOT NULL,
            category      TEXT NOT NULL,
            confidence    REAL NOT NULL,
            classified_at TEXT NOT NULL
        );",
    )
}

/// Hashes that have an embedding under `model_id` but no classification yet.
pub fn pending_hashes(conn: &Connection, model_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT hash FROM embeddings
         WHERE model_id = ?1
           AND NOT EXISTS (SELECT 1 FROM classifications c WHERE c.hash = embeddings.hash)
         ORDER BY hash",
    )?;
    let rows = stmt.query_map(params![model_id], |row| row.get(0))?;
    rows.collect()
}

/// Upsert a batch of (hash, category, confidence) rows inside one transaction.
pub fn insert_classifications(conn: &Connection, items: &[(String, &str, f32)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO classifications (hash, category, confidence, classified_at)
             VALUES (?1, ?2, ?3, datetime('now'))",
        )?;
        for (hash, category, confidence) in items {
            stmt.execute(params![hash, category, confidence])?;
        }
    }
    tx.commit()
}

/// (path, hash) pairs for every file classified as `category`, one entry per
/// on-disk path of a matched hash (same duplicate-path convention as
/// `embeddings::paths_for_hash`).
pub fn paths_for_category(conn: &Connection, category: &str) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT file_hashes.path, file_hashes.hash FROM file_hashes
         JOIN classifications ON file_hashes.hash = classifications.hash
         WHERE classifications.category = ?1
         ORDER BY file_hashes.path",
    )?;
    let rows = stmt.query_map(params![category], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-core classify:: -- --nocapture`
Expected: PASS (6 tests).

- [ ] **Step 5: Register the module**

In `crates/videre-core/src/lib.rs`, add as the first line (alphabetically first: "classify" < "db"):

```rust
pub mod classify;
pub mod db;
```

(Keep every other existing `pub mod` line unchanged - only insert the new line before `pub mod db;`.)

- [ ] **Step 6: Build and run the full core test suite**

```bash
cargo build -p videre-core
cargo test -p videre-core
```

Expected: clean build, all tests pass (67 existing + 6 new = 73).

- [ ] **Step 7: Commit**

```bash
git add crates/videre-core/src/classify.rs crates/videre-core/src/lib.rs
git commit -m "feat(classify): add classifications table + core module, TDD'd"
```

---

### Task 2: Pure classification decision logic + prompts, TDD

**Files:**
- Create: `crates/videre-ml/src/classify.rs`
- Modify: `crates/videre-ml/src/lib.rs`

**Context:** This function takes already-computed similarity scores (float, one per category) and decides the winner - it never touches the model or a database, so it's fully unit-testable. The actual scoring (embedding images/prompts, computing dot products) happens in Task 3's CLI command.

- [ ] **Step 1: Write the failing tests**

Create `crates/videre-ml/src/classify.rs`:

```rust
//! Zero-shot classification of already-computed image embeddings against a
//! fixed set of category prompts, reusing the SigLIP text tower `videre
//! embed`/`videre search` already use - no new model, no re-embedding images.
//! See docs/superpowers/specs/2026-07-29-screenshot-document-classification-design.md.

/// Category name used when no prompt's similarity clearly wins.
pub const UNKNOWN_CATEGORY: &str = "unknown";

/// (category name, zero-shot prompt caption). Not exposed as a CLI flag -
/// tune here if real-world results look off. SigLIP embeds full descriptive
/// captions better than bare single-word labels.
pub const CATEGORY_PROMPTS: &[(&str, &str)] = &[
    ("photo", "a photo of a person, place, or thing"),
    ("screenshot", "a screenshot of a phone or computer screen"),
    ("document", "a photo of a document, receipt, or piece of paper"),
    ("meme", "a meme image with text captions overlaid on a picture"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_winner_returns_that_category_with_its_score() {
        let scores = [("photo", 0.9), ("screenshot", 0.3), ("document", 0.2), ("meme", 0.1)];
        let (cat, score) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, "photo");
        assert_eq!(score, 0.9);
    }

    #[test]
    fn top_two_within_margin_falls_back_to_unknown() {
        let scores = [("photo", 0.52), ("screenshot", 0.50), ("document", 0.1), ("meme", 0.05)];
        let (cat, score) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, UNKNOWN_CATEGORY);
        assert_eq!(score, 0.52);
    }

    #[test]
    fn gap_exactly_equal_to_margin_falls_back_to_unknown() {
        let scores = [("photo", 0.55), ("screenshot", 0.50)];
        let (cat, score) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, UNKNOWN_CATEGORY);
        assert_eq!(score, 0.55);
    }

    #[test]
    fn gap_just_over_margin_accepts_top_pick() {
        let scores = [("photo", 0.551), ("screenshot", 0.50)];
        let (cat, _) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, "photo");
    }

    #[test]
    fn single_entry_returns_that_entry_without_panicking() {
        let scores = [("photo", 0.42)];
        let (cat, score) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, "photo");
        assert_eq!(score, 0.42);
    }

    #[test]
    #[should_panic]
    fn empty_scores_panics() {
        let scores: [(&'static str, f32); 0] = [];
        classify_from_scores(&scores, 0.05);
    }

    #[test]
    fn category_prompts_has_four_entries() {
        assert_eq!(CATEGORY_PROMPTS.len(), 4);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre-ml classify:: -- --nocapture`
Expected: FAIL with "cannot find function `classify_from_scores`".

- [ ] **Step 3: Implement**

Add above the test module, after `CATEGORY_PROMPTS`:

```rust
/// Picks the winning category from per-prompt similarity scores, or
/// `UNKNOWN_CATEGORY` if the top two scores are not clearly separated (the
/// gap must be strictly greater than `margin` to accept the top pick - a
/// gap exactly equal to `margin` falls back to unknown).
///
/// Category names are `&'static str` (always `CATEGORY_PROMPTS` entries or
/// `UNKNOWN_CATEGORY`), not tied to `scores`'s borrow, so callers can hold
/// the returned category across loop iterations without lifetime issues.
///
/// Panics if `scores` is empty - callers always pass one score per
/// `CATEGORY_PROMPTS` entry, which is never empty.
pub fn classify_from_scores(scores: &[(&'static str, f32)], margin: f32) -> (&'static str, f32) {
    assert!(!scores.is_empty(), "classify_from_scores requires at least one score");
    let mut sorted: Vec<(&'static str, f32)> = scores.to_vec();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (top_category, top_score) = sorted[0];
    if sorted.len() == 1 {
        return (top_category, top_score);
    }
    let (_, second_score) = sorted[1];
    if top_score - second_score > margin {
        (top_category, top_score)
    } else {
        (UNKNOWN_CATEGORY, top_score)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-ml classify:: -- --nocapture`
Expected: PASS (7 tests).

- [ ] **Step 5: Register the module**

In `crates/videre-ml/src/lib.rs`, add as the first line (alphabetically first: "classify" < "device"):

```rust
pub mod classify;
pub mod device;
```

- [ ] **Step 6: Build and run the full videre-ml test suite**

```bash
cargo build -p videre-ml
cargo test -p videre-ml
```

Expected: clean build, all tests pass (35 existing + 7 new = 42).

- [ ] **Step 7: Commit**

```bash
git add crates/videre-ml/src/classify.rs crates/videre-ml/src/lib.rs
git commit -m "feat(classify): add classify_from_scores + CATEGORY_PROMPTS, TDD'd"
```

---

### Task 3: `videre classify` subcommand

**Files:**
- Create: `crates/videre/src/commands/classify.rs`
- Modify: `crates/videre/src/commands/mod.rs`
- Modify: `crates/videre/src/main.rs`

**Context:** This wires Task 1's DB module and Task 2's pure decision function together with the real SigLIP model. Like `videre faces`'s multi-worker pipeline, the model-loading + DB-iteration logic here is verified by building and running against real data (Task 4), not by unit tests - there's nothing left to unit-test once the pure logic is already covered by Tasks 1-2.

- [ ] **Step 1: Write the subcommand**

Create `crates/videre/src/commands/classify.rs`:

```rust
use anyhow::{Context, Result};
use videre_core::{classify as classify_core, embeddings, vectors};
use videre_ml::{classify as classify_ml, device, model};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct ClassifyArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Re-classify every embedded hash, including ones already classified
    #[arg(long)]
    reprocess: bool,

    /// Min similarity gap between the best and second-best category to
    /// accept a result; below this, stores "unknown" instead. Default 0.05.
    #[arg(long, default_value_t = 0.05)]
    margin: f32,

    /// Suppress per-image progress output on stderr (errors always shown)
    #[arg(long)]
    silent: bool,
}

pub fn run(args: ClassifyArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)
        .with_context(|| format!("open {}", db.display()))?;
    classify_core::ensure_classifications_table(&conn)?;

    let hashes: Vec<String> = if args.reprocess {
        embeddings::load_embeddings(&conn, model::MODEL_ID)?
            .into_iter()
            .map(|(hash, _)| hash)
            .collect()
    } else {
        classify_core::pending_hashes(&conn, model::MODEL_ID)?
    };

    if hashes.is_empty() {
        if !args.silent {
            eprintln!("Nothing to classify: all embedded hashes already classified.");
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let embedder = model::Embedder::load(device::best_device())?;

    // Embed each category prompt once; reused for every image below.
    let prompt_vecs: Vec<(&str, Vec<f32>)> = classify_ml::CATEGORY_PROMPTS
        .iter()
        .map(|(name, prompt)| Ok((*name, embedder.embed_text(prompt)?)))
        .collect::<Result<_>>()?;

    // Look up embeddings by hash rather than holding the whole corpus twice -
    // hashes.len() can be in the tens of thousands.
    let all_embeddings: std::collections::HashMap<String, Vec<u8>> =
        embeddings::load_embeddings(&conn, model::MODEL_ID)?.into_iter().collect();

    let progress = videre_core::progress::Progress::new(hashes.len() as u64, args.silent);
    let mut rows: Vec<(String, &str, f32)> = Vec::with_capacity(hashes.len());
    for hash in &hashes {
        let Some(blob) = all_embeddings.get(hash) else {
            progress.println(&format!("skipping {hash}: embedding vanished mid-run"));
            progress.tick();
            continue;
        };
        let vec = vectors::from_f16_bytes(blob);
        let scores: Vec<(&'static str, f32)> = prompt_vecs
            .iter()
            .map(|(name, prompt_vec)| {
                let dot: f32 = vec.iter().zip(prompt_vec.iter()).map(|(a, b)| a * b).sum();
                (*name, dot)
            })
            .collect();
        let (category, confidence) = classify_ml::classify_from_scores(&scores, args.margin);
        rows.push((hash.clone(), category, confidence));
        progress.tick();
    }
    progress.finish();

    classify_core::insert_classifications(&conn, &rows)?;

    if !args.silent {
        eprintln!("{}", format_summary(rows.len(), started.elapsed()));
    }
    Ok(())
}

/// Assembles the single consolidated summary line printed after
/// classification finishes.
fn format_summary(done: usize, elapsed: std::time::Duration) -> String {
    format!("{done} image(s) classified, done in {}s", elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_summary_reads_naturally() {
        assert_eq!(
            format_summary(42, std::time::Duration::from_secs(3)),
            "42 image(s) classified, done in 3s"
        );
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/videre/src/commands/mod.rs`, add as the first line (alphabetically first: "classify" < "config"):

```rust
pub mod classify;
pub mod config;
```

- [ ] **Step 3: Wire into `main.rs`**

In `crates/videre/src/main.rs`, add a new `Command` variant. Insert it right after `Faces` (classify is closely related to search/embed, but grouping it near faces keeps the "detect/classify" pair together - either position is fine since clap doesn't care about enum order, this is just for human readability of the match arms):

```rust
    /// Detect, embed, and cluster faces; enables person search
    Faces(commands::faces::FacesArgs),
    /// Classify images as photo/screenshot/document/meme (zero-shot, reuses embeddings)
    Classify(commands::classify::ClassifyArgs),
    /// Background loop keeping scan/faces/HEIC-cache/location data fresh
    Watch(commands::watch::WatchArgs),
```

And the matching dispatch arm, in the same relative position:

```rust
        Command::Faces(args) => commands::faces::run(args),
        Command::Classify(args) => commands::classify::run(args),
        Command::Watch(args) => commands::watch::run(args),
```

- [ ] **Step 4: Build and run the full workspace test suite**

```bash
cargo build --workspace
cargo test --workspace
```

Expected: clean build, all tests pass (previous total + 1 new test in `classify.rs`).

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/classify.rs crates/videre/src/commands/mod.rs crates/videre/src/main.rs
git commit -m "feat(classify): add videre classify subcommand"
```

---

### Task 4: `--category` mode in `videre search`

**Files:**
- Modify: `crates/videre/src/commands/search.rs`

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block in `crates/videre/src/commands/search.rs`:

```rust
    #[test]
    fn category_hit_includes_hash_but_omits_score() {
        let doc = SearchJson {
            schema_version: SCHEMA_VERSION,
            query: QueryJson { kind: "category", value: "screenshot".to_string() },
            count: 1,
            results: vec![SearchHitJson {
                path: "/a.png".to_string(),
                hash: Some("abc".to_string()),
                score: None,
            }],
        };
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"kind\":\"category\""));
        assert!(json.contains("\"hash\":\"abc\""));
        assert!(!json.contains("\"score\""));
    }
```

(This test only exercises serialization, same as the existing `text_hit_serializes_with_hash_and_score`/`person_hit_omits_hash_and_score_keys` tests above it - it will actually compile and pass immediately since `SearchJson`/`QueryJson`/`SearchHitJson` already support this shape. The real new-behavior verification is the CLI wiring in Steps 3-5, checked by building and a manual run in Task 5.)

- [ ] **Step 2: Run the test to confirm it passes as-is**

Run: `cargo test -p videre category_hit_includes_hash_but_omits_score -- --nocapture`
Expected: PASS immediately (no new code needed for this specific test - it documents the target JSON shape before the CLI plumbing exists).

- [ ] **Step 3: Add the `--category` flag**

In `crates/videre/src/commands/search.rs`, change the top-of-file import:

```rust
use videre_core::{embeddings, vectors};
```

to:

```rust
use videre_core::{classify as classify_core, embeddings, vectors};
```

Add a new field to `SearchArgs`, right after the existing `person` field:

```rust
    /// Return paths containing a named person (confirmed faces only)
    #[arg(long, conflicts_with = "query", conflicts_with = "image")]
    person: Option<String>,

    /// Return paths classified as this category - photo/screenshot/document/
    /// meme/unknown (requires a prior 'videre classify' run)
    #[arg(long, conflicts_with = "query", conflicts_with = "image", conflicts_with = "person")]
    category: Option<String>,
```

- [ ] **Step 4: Add `category_hits` and wire it into `collect_hits`**

Add a new function right after `person_hits`:

```rust
/// Category query: paths + hash (no score - membership only, not ranked).
/// Unlike `person_hits`, hash comes along for free from the join query
/// itself (person search's own helper doesn't return one), so it's
/// included here.
pub(crate) fn category_hits(conn: &Connection, category: &str) -> Result<Vec<SearchHitJson>> {
    let pairs = classify_core::paths_for_category(conn, category)?;
    Ok(pairs
        .into_iter()
        .map(|(path, hash)| SearchHitJson { path, hash: Some(hash), score: None })
        .collect())
}
```

In `collect_hits`, add a new branch right before the existing `if let Some(name) = &args.person` branch:

```rust
    if let Some(name) = &args.category {
        let hits = category_hits(&conn, name)?;
        if hits.is_empty() && !args.json {
            eprintln!("No files found classified as: {name}");
        }
        return Ok((QueryJson { kind: "category", value: name.clone() }, hits));
    }

    if let Some(name) = &args.person {
```

- [ ] **Step 5: Build and run the full workspace test suite**

```bash
cargo build --workspace
cargo test --workspace
```

Expected: clean build, all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/search.rs
git commit -m "feat(classify): add --category mode to videre search"
```

---

### Task 5: Manual verification against the real library

**Files:** none (verification only).

- [ ] **Step 1: Build release**

```bash
cargo build --release
```

- [ ] **Step 2: Confirm embeddings exist (classify depends on a prior `videre embed` run)**

```bash
./target/release/videre search "test" 2>&1 | head -3
```

Expected: prints results (not a "no embeddings found" error) - confirms the default db already has embeddings from earlier `videre embed` runs in this project. If it errors, run `videre embed` first before continuing.

- [ ] **Step 3: Run classification against the real library**

```bash
time ./target/release/videre classify
```

Expected: prints `Loading model ...` (SigLIP, likely already cached from prior embed/search runs, so this should be fast), then a summary line like `N image(s) classified, done in Ns`. Report the actual N and elapsed time - this should be fast (no image decoding, just dot products over already-loaded vectors), a few seconds to low tens of seconds even for a large library.

- [ ] **Step 4: Confirm resumability**

```bash
./target/release/videre classify
```

Expected: `Nothing to classify: all embedded hashes already classified.` - confirms the second run correctly skips everything the first run already did.

- [ ] **Step 5: Query results**

```bash
./target/release/videre search --category screenshot | head -5
./target/release/videre search --category photo | wc -l
./target/release/videre search --category unknown | wc -l
./target/release/videre search --category screenshot --json | head -c 500
```

Expected: real paths printed for whichever categories actually matched something in the library; the `--json` call prints a valid JSON document with `"kind":"category"`, `"count"`, and a `results` array whose entries have `path`+`hash` but no `score` key.

- [ ] **Step 6: Sanity-check a few real classifications**

Pick 2-3 paths printed under `--category screenshot` (or whichever category returned results) and open them to confirm they're actually screenshots, not real photos. **Report what you find** - if the zero-shot prompts are clearly misclassifying (e.g. real photos landing in "screenshot"), that's a signal the `CATEGORY_PROMPTS` wording in `crates/videre-ml/src/classify.rs` needs adjusting before this is considered done, not something to silently accept. If results look reasonable, note that too.

- [ ] **Step 7: Test `--reprocess`**

```bash
time ./target/release/videre classify --reprocess
```

Expected: re-classifies every hash (not "nothing to classify"), same summary shape as Step 3.

---

### Task 6: README updates

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add to the subcommand table**

Find this block:

```markdown
| `videre search` | Search images by text description, example image, or person name |
| `videre faces` | Detect, embed, and cluster faces; enables person search |
```

Change to:

```markdown
| `videre search` | Search images by text description, example image, person name, or category |
| `videre faces` | Detect, embed, and cluster faces; enables person search |
| `videre classify` | Classify images as photo/screenshot/document/meme (zero-shot, reuses embeddings) |
```

- [ ] **Step 2: Add a quickstart step**

Find:

```markdown
# 8. Search by text or example image
videre search "golden gate bridge at sunset"
videre search --image reference.jpg

# 9. Detect, embed, and cluster faces for person search
videre faces

# 10. Label faces in the browser UI, then save and close
videre report --faces

# 11. Find all photos of a named person
videre search --person "Alice"

# 12. Browse the full collection with in-page similarity search
videre report --all

# 13. Browse a Year/Month/Day drill-down gallery (static HTML, same as --all)
videre report --by-date

# 14. Live report with labeled-face and location metadata in the lightbox
videre report --show-faces

# 15. Keep everything fresh in the background (run alongside step 14, same db)
videre watch ~/Photos
```

Change to (inserts a new step 9, renumbers everything after):

```markdown
# 8. Search by text or example image
videre search "golden gate bridge at sunset"
videre search --image reference.jpg

# 9. Classify images as photo/screenshot/document/meme, then find screenshots
videre classify
videre search --category screenshot

# 10. Detect, embed, and cluster faces for person search
videre faces

# 11. Label faces in the browser UI, then save and close
videre report --faces

# 12. Find all photos of a named person
videre search --person "Alice"

# 13. Browse the full collection with in-page similarity search
videre report --all

# 14. Browse a Year/Month/Day drill-down gallery (static HTML, same as --all)
videre report --by-date

# 15. Live report with labeled-face and location metadata in the lightbox
videre report --show-faces

# 16. Keep everything fresh in the background (run alongside step 15, same db)
videre watch ~/Photos
```

- [ ] **Step 3: Add to the explicit-database example block**

Find:

```markdown
videre scan --output-sqlite ~/photos.db ~/Photos
videre dedupe --db ~/photos.db
videre report --db ~/photos.db
videre search --db ~/photos.db "golden gate bridge at sunset"
videre watch --output-sqlite ~/photos.db ~/Photos
```

Change to:

```markdown
videre scan --output-sqlite ~/photos.db ~/Photos
videre dedupe --db ~/photos.db
videre report --db ~/photos.db
videre search --db ~/photos.db "golden gate bridge at sunset"
videre classify --db ~/photos.db
videre watch --output-sqlite ~/photos.db ~/Photos
```

- [ ] **Step 4: Add a new `## videre classify` section**

Find the `## videre watch` heading (the section right after `## videre faces`'s "Faces workflow" block and its trailing `---`), and insert a new section immediately before it:

```markdown
## videre classify

Classifies every embedded image as `photo`, `screenshot`, `document`, or
`meme` using zero-shot classification against the SigLIP embeddings
`videre embed` already computed - no new model, no re-reading image files.
Requires a prior `videre embed` run.

```bash
videre classify                     # classify all embedded-but-unclassified hashes, default db
videre classify --db <path>         # explicit db
videre classify --reprocess         # re-classify everything, including already-classified hashes
videre classify --silent            # suppress per-image progress
videre classify --margin <f32>      # min similarity gap to accept a category, else "unknown" (default: 0.05)
```

Each image's stored embedding is scored against 4 fixed text prompts (one
per category) via cosine similarity; the best match wins unless the top two
scores are too close together, in which case the image is stored as
`unknown` rather than a low-confidence guess. Resumable like `videre embed`/
`videre faces`: re-running only classifies hashes that don't have a
classification yet, unless `--reprocess`.

```bash
videre search --category screenshot          # print paths of all screenshots
videre search --category document --json     # same, JSON output
```

---

## videre watch
```

- [ ] **Step 5: Add `classifications` to the SQLite schema reference**

Find:

```sql
CREATE TABLE IF NOT EXISTS faces_scanned (
    hash        TEXT PRIMARY KEY,
    scanned_at  TEXT DEFAULT (datetime('now'))
);
```
```

Change to:

```sql
CREATE TABLE IF NOT EXISTS faces_scanned (
    hash        TEXT PRIMARY KEY,
    scanned_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS classifications (
    hash          TEXT PRIMARY KEY,
    category      TEXT NOT NULL,
    confidence    REAL NOT NULL,
    classified_at TEXT NOT NULL
);
```
```

And add a sentence to the paragraph right after that code block (find the paragraph starting "Re-scanning upserts existing rows by `path`...` and ending "...this is what makes detection resumable." - append after it):

```markdown
`classifications` is populated by `videre classify` (zero-shot, reuses `embeddings` - no new model or image decoding) and queried via `videre search --category <name>`.
```

- [ ] **Step 6: Add `videre classify` to the embed/search paragraph**

Find:

```markdown
`videre embed` computes SigLIP embeddings (google/siglip-so400m-patch14-384, 1152-dim f16) for every image in the database and stores them keyed by content hash. Re-running only processes images not yet embedded. `.mov`, `.mp4`, and `.dng` files are skipped.
```

Change to:

```markdown
`videre embed` computes SigLIP embeddings (google/siglip-so400m-patch14-384, 1152-dim f16) for every image in the database and stores them keyed by content hash. Re-running only processes images not yet embedded. `.mov`, `.mp4`, and `.dng` files are skipped. `videre classify` (see below) reuses these embeddings for zero-shot photo/screenshot/document/meme classification, so it's worth running `videre embed` even if you don't need text/image search.
```

- [ ] **Step 7: Review the final diff for correctness**

```bash
git diff README.md
```

Read through it once to confirm every inserted block reads naturally in context (no dangling headers, no duplicated `---` separators).

- [ ] **Step 8: Commit**

```bash
git add README.md
git commit -m "docs: document videre classify in README (subcommand table, quickstart, section, schema)"
```

---

### Task 7: CLAUDE.md updates

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Update the subcommand count and description**

Find:

```markdown
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

Change to:

```markdown
`videre` is a single binary with twelve subcommands. `videre scan` scans a directory
recursively, hashes every image file (BLAKE3), and writes the results into the database
(or JSONL with `--output`). `videre dedupe` reads that database and writes REMOVE
candidates to stdout one per line: ready to pipe into `trash` or `rm`. Bare `videre scan
<dir>` writes SQLite to the resolved default database (see `~/.videre` below); JSONL
output requires `--output`. `videre report` reads the SQLite database and generates an
HTML review page (or serves a live web UI). The remaining subcommands (`fix-dates`,
`prune`, `embed`, `search`, `faces`, `classify`, `watch`) operate on the same SQLite
database to fix timestamps, sync metadata, compute semantic embeddings, run
text/image/person/category search, detect/label faces, and classify images as
photo/screenshot/document/meme. `videre config` shows or edits the resolved paths and
`~/.videre/config.toml` settings. `videre mcp` serves read-only search/find_duplicates/
stats tools over stdio for LLM agents.
```

- [ ] **Step 2: Update the project structure file list**

Find:

```
    src/commands/{mod.rs,dedupe.rs,report.rs,scan.rs,fix_dates.rs,prune.rs,embed.rs,search.rs,faces.rs,watch.rs,config.rs,mcp.rs}
```

Change to:

```
    src/commands/{mod.rs,dedupe.rs,report.rs,scan.rs,fix_dates.rs,prune.rs,embed.rs,search.rs,faces.rs,classify.rs,watch.rs,config.rs,mcp.rs}
```

Find:

```
    src/vectors.rs
    src/embeddings.rs
    src/face_db.rs
```

Change to:

```
    src/vectors.rs
    src/embeddings.rs
    src/classify.rs
    src/face_db.rs
```

Find:

```
    src/{device.rs,model.rs,preprocess.rs,search.rs,pipeline.rs}
    src/{face_models.rs,face_detect.rs,face_align.rs,face_embed.rs}
```

Change to:

```
    src/{device.rs,model.rs,preprocess.rs,search.rs,pipeline.rs,classify.rs}
    src/{face_models.rs,face_detect.rs,face_align.rs,face_embed.rs}
```

- [ ] **Step 3: Add a `classifications` table to the SQLite schema section**

Find:

```sql
CREATE TABLE IF NOT EXISTS faces (
    id            INTEGER PRIMARY KEY,
    hash          TEXT NOT NULL,
    bbox          TEXT NOT NULL,
    landmark      TEXT,
    embedding     BLOB NOT NULL,
    cluster_id    INTEGER,
    person_label  TEXT,
    confirmed     INTEGER DEFAULT 0,
    is_primary    INTEGER DEFAULT 0
);
```
```

Change to:

```sql
CREATE TABLE IF NOT EXISTS faces (
    id            INTEGER PRIMARY KEY,
    hash          TEXT NOT NULL,
    bbox          TEXT NOT NULL,
    landmark      TEXT,
    embedding     BLOB NOT NULL,
    cluster_id    INTEGER,
    person_label  TEXT,
    confirmed     INTEGER DEFAULT 0,
    is_primary    INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS classifications (
    hash          TEXT PRIMARY KEY,
    category      TEXT NOT NULL,
    confidence    REAL NOT NULL,
    classified_at TEXT NOT NULL
);
```
```

(This is documented in the main "SQLite schema" section here, unlike `embeddings` which CLAUDE.md documents separately in its own `## videre embed / videre search` section - `classifications` fits better here since it's a small standalone table without much subcommand-specific behavior to explain alongside it, mirroring how `faces`/`faces_scanned` are documented in this same section.)

Add a sentence after the existing `faces_scanned` paragraph (find "...so no-face images are detected once rather than every run. Created by `create_faces_table` alongside `faces`; written per hash as detection proceeds." and insert after it):

```markdown
`classifications` is populated by `videre classify` (zero-shot photo/screenshot/document/meme classification, scoring `embeddings` rows already computed by `videre embed` against 4 fixed text prompts via cosine similarity - no new model, no image re-decoding) and queried via `videre search --category <name>`. Rows below the configurable `--margin` similarity gap between the best and second-best category are stored as `category = "unknown"` rather than a low-confidence guess.
```

- [ ] **Step 4: Add a `## videre classify` section**

Find the `## videre faces` heading and insert a new section immediately before it (right after the `## videre embed / videre search` section's closing ```` ``` ```` from the `embeddings` schema block):

```markdown
## videre classify

`videre classify` (optionally `--db <db>`) classifies every embedded hash as `photo`,
`screenshot`, `document`, or `meme` via zero-shot classification: each stored embedding
is scored by cosine similarity against 4 fixed text prompts (`crates/videre-ml/src/classify.rs`'s
`CATEGORY_PROMPTS`), embedded once via the same SigLIP text tower `videre search` uses.
No new model, no image re-decoding - this runs entirely over vectors `videre embed`
already computed. Resumable: re-running only classifies hashes not yet in
`classifications`, unless `--reprocess`. `--margin` (default 0.05) is the min similarity
gap between the best and second-best category to accept a result; below that, the row
is stored as `unknown` rather than a low-confidence guess.

```
videre classify                     # classify all embedded-but-unclassified hashes, default db
videre classify --db <path>         # explicit db
videre classify --reprocess         # re-classify everything, including already-classified hashes
videre classify --silent            # suppress per-image progress
videre classify --margin <f32>      # min similarity gap to accept a category (default: 0.05)
```

`videre search --category <name>` queries the `classifications` table for rows matching
`category` and prints matching image paths (or, under `--json`, a `results` array with
`path`+`hash` per entry - no `score`, since this is set membership, not a ranked query).

```

- [ ] **Step 5: Add the design spec to the "Design specs" list**

Find the last line:

```markdown
- `docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md` - `videre faces` profiling instrumentation + multi-worker pipeline parallelization (`--workers`/`--profile` flags)
```

Add a new line right after it:

```markdown
- `docs/superpowers/specs/2026-07-29-screenshot-document-classification-design.md` - zero-shot photo/screenshot/document/meme classification reusing `videre embed` embeddings; `videre classify` subcommand + `videre search --category`
```

- [ ] **Step 6: Review the final diff for correctness**

```bash
git diff CLAUDE.md
```

Read through it once to confirm every inserted block reads naturally (no dangling headers, no duplicated content, the new `## videre classify` section sits between `## videre embed / videre search` and `## videre faces` correctly).

- [ ] **Step 7: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document videre classify in CLAUDE.md (schema, section, project structure, design specs list)"
```

---

### Task 8: Version bump + final verification

**Files:**
- Modify: `crates/videre/Cargo.toml`, `crates/videre-api/Cargo.toml`, `crates/videre-core/Cargo.toml`, `crates/videre-ml/Cargo.toml`
- Modify: `Cargo.lock`

- [ ] **Step 1: Bump all four crate versions (minor - new user-facing subcommand + flag)**

Check the current version first:

```bash
grep -n "^version" crates/videre/Cargo.toml crates/videre-api/Cargo.toml crates/videre-core/Cargo.toml crates/videre-ml/Cargo.toml
```

Bump each from whatever `X.Y.Z` is currently shown to `X.(Y+1).0` (e.g. if current is `0.8.1`, bump to `0.9.0`) - a new subcommand and a new search mode are user-facing feature additions, not a patch.

```bash
sed -i '' 's/version = "OLD_VERSION"/version = "NEW_VERSION"/' crates/videre/Cargo.toml crates/videre-api/Cargo.toml crates/videre-core/Cargo.toml crates/videre-ml/Cargo.toml
```

(Replace `OLD_VERSION`/`NEW_VERSION` with the actual values found above - do not guess without checking.)

- [ ] **Step 2: Update Cargo.lock**

```bash
cargo update --workspace --offline
```

- [ ] **Step 3: Full workspace build and test**

```bash
cargo build --workspace
cargo test --workspace
```

Expected: clean build, every test passes (no regressions in any crate).

- [ ] **Step 4: Commit**

```bash
git add Cargo.lock crates/videre/Cargo.toml crates/videre-api/Cargo.toml crates/videre-core/Cargo.toml crates/videre-ml/Cargo.toml
git commit -m "chore: bump version for videre classify"
```

---
