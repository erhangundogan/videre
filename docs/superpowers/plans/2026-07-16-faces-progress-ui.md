# `videre faces` progress UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `videre faces`'s per-image stdout/stderr spam with a brew/docker/npm-style in-place progress bar plus one consolidated summary line, via a new reusable `Progress` module in `videre-core`.

**Architecture:** A new `crates/videre-core/src/progress.rs` module wraps `indicatif::ProgressBar` behind a small `Progress` type with three modes (real TTY bar, non-TTY periodic plain-text fallback, fully silent) selected once at construction via `std::io::IsTerminal`. `crates/videre-ml/src/pipeline.rs`'s `run_face_pipeline` uses it to replace per-image `eprintln!` calls with bar ticks, and its per-image error messages route through `Progress::println` so they survive an active bar. `run_clustering` stops printing internally and instead returns a `ClusteringResult` so callers can fold clustering stats into one summary line. `crates/videre/src/commands/faces.rs` assembles that summary via two `pub(crate)` formatting functions, which `crates/videre/src/commands/watch.rs`'s faces stage also reuses (both are the only two callers of `run_face_pipeline`/`run_clustering` in this codebase - `watch.rs` must be updated in the same task as `run_clustering`'s signature change or the `videre` binary crate will not compile).

**Tech Stack:** Rust, `indicatif` (already a transitive dependency in `Cargo.lock` at 0.17.11, promoted to a direct `videre-core` dependency here), `std::io::IsTerminal` (stable since Rust 1.70; this toolchain is 1.96.0). Baseline: `cargo test --workspace` = 187 passing on `main` at `a09c397`.

**House rules (mandatory):** never use the em dash character anywhere (code, comments, commit messages); no Co-Authored-By trailer or "Generated with" line; use the exact commit messages given.

**Branch:** work on a new branch `faces-progress-ui` off `main`:

```bash
cd /Users/erhangundogan/projects/rust/videre
git checkout -b faces-progress-ui
```

---

### Task 1: `Progress` module in `videre-core`

**Files:**
- Modify: `crates/videre-core/Cargo.toml`, `crates/videre-core/src/lib.rs`
- Create: `crates/videre-core/src/progress.rs`

- [ ] **Step 1: Add the dependency**

In `crates/videre-core/Cargo.toml`, add `indicatif` to `[dependencies]` (alphabetical position, after `image`):

```toml
[dependencies]
anyhow = "1"
half = "2"
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "gif", "webp", "bmp", "tiff"] }
indicatif = "0.17"
reverse_geocoder = "4"
rusqlite = { version = "0.32", features = ["bundled"] }
toml = { version = "0.8", features = ["preserve_order"] }
```

- [ ] **Step 2: Write the failing tests**

Create `crates/videre-core/src/progress.rs`:

```rust
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;

/// Reports progress for a batch of N items as an in-place bar (brew/docker/
/// npm style) when stderr is a terminal, or periodic plain-text lines when
/// it isn't (piped to a file, CI log) - so a long run never looks hung in a
/// log file, without per-item spam either way. `silent` suppresses the bar
/// and periodic lines entirely, but NOT error output (see `println`) or the
/// caller's own decision about whether to print a final summary.
///
/// Does not track elapsed time itself: callers that need it (e.g.
/// `faces.rs`, whose summary spans both detection and clustering, not just
/// the `Progress`-tracked detection phase) should use their own `Instant`
/// spanning whatever the summary needs to cover.
pub struct Progress {
    total: u64,
    done: u64,
    mode: Mode,
}

enum Mode {
    Bar(ProgressBar),
    /// Non-TTY fallback: print one line every LOG_INTERVAL ticks.
    Plain,
    /// --silent: no bar, no periodic lines. Errors still print (see println).
    Silent,
}

const LOG_INTERVAL: u64 = 25;

impl Progress {
    /// Creates a progress reporter for `total` items. When stderr is a TTY,
    /// renders an in-place bar. When it isn't, falls back to one plain-text
    /// line every `LOG_INTERVAL` items. `silent` suppresses both.
    pub fn new(total: u64, silent: bool) -> Self {
        let mode = if silent {
            Mode::Silent
        } else if std::io::stderr().is_terminal() {
            let bar = ProgressBar::new(total);
            bar.set_style(
                ProgressStyle::with_template("{bar:40} {percent}%")
                    .unwrap()
                    .progress_chars("=> "),
            );
            Mode::Bar(bar)
        } else {
            Mode::Plain
        };
        Progress { total, done: 0, mode }
    }

    /// Advance by one item.
    pub fn tick(&mut self) {
        self.done += 1;
        match &self.mode {
            Mode::Bar(bar) => bar.set_position(self.done),
            Mode::Plain => {
                if self.done % LOG_INTERVAL == 0 || self.done == self.total {
                    eprintln!("{}/{} images processed", self.done, self.total);
                }
            }
            Mode::Silent => {}
        }
    }

    /// Print a line that survives an active progress bar without corrupting
    /// its rendering. Always prints, regardless of `silent` - matches the
    /// existing unconditional behavior of per-image error messages
    /// (`detect failed ...`, `embed_batch failed ...`, `write failed ...`),
    /// which must stay visible even under --silent since they indicate data
    /// loss, not routine progress.
    pub fn println(&self, msg: &str) {
        match &self.mode {
            Mode::Bar(bar) => bar.println(msg),
            Mode::Plain | Mode::Silent => eprintln!("{msg}"),
        }
    }

    /// Clears the bar (if any) so the final summary prints cleanly below it
    /// rather than being overwritten. Does not print anything itself - the
    /// caller assembles and prints its own summary line(s).
    pub fn finish(self) {
        if let Mode::Bar(bar) = self.mode {
            bar.finish_and_clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_mode_tick_does_not_panic() {
        let mut p = Progress::new(10, true);
        for _ in 0..10 {
            p.tick();
        }
        p.finish();
    }

    #[test]
    fn silent_mode_println_still_prints() {
        // println() must not panic in silent mode; it always writes to
        // stderr regardless of `silent` (verified by not panicking here -
        // capturing stderr output itself is not practical in a unit test).
        let p = Progress::new(5, true);
        p.println("an error message");
    }

    #[test]
    fn zero_total_does_not_panic() {
        let mut p = Progress::new(0, true);
        p.tick();
        p.finish();
    }
}
```

- [ ] **Step 3: Register the module**

In `crates/videre-core/src/lib.rs`, add `pub mod progress;` in alphabetical position (after `person_search`, before `thumb_cache`):

```rust
pub mod db;
pub mod embeddings;
pub mod face_cluster;
pub mod face_db;
pub mod heic;
pub mod home;
pub mod location;
pub mod person_search;
pub mod progress;
pub mod thumb_cache;
pub mod vectors;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-core --lib progress::`
Expected: PASS (3 tests).

Run: `cargo test --workspace`
Expected: PASS, 190 total (187 baseline + 3). No compiler warnings (`cargo build --workspace 2>&1 | grep -i warning` empty).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/Cargo.toml crates/videre-core/src/lib.rs crates/videre-core/src/progress.rs
git commit -m "feat: Progress module wraps indicatif with TTY-aware bar/plain/silent modes"
```

---

### Task 2: `run_face_pipeline` uses `Progress`

Replaces the per-image `[faces] {path}: {N} face(s)` spam with bar ticks, and routes the three existing error messages through `Progress::println` so they survive an active bar.

**Files:**
- Modify: `crates/videre-ml/src/pipeline.rs`

- [ ] **Step 1: Write the failing test change**

In `crates/videre-ml/src/pipeline.rs`, update the existing `run_face_pipeline_on_empty_input_is_a_noop` test (in the `#[cfg(test)] mod tests` block at the bottom of the file) to assert the two new `FacesRunResult` fields this task adds:

```rust
    #[test]
    fn run_face_pipeline_on_empty_input_is_a_noop() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        let result = run_face_pipeline(&conn, &[], 8, false, true).unwrap();
        assert_eq!(result.total_faces, 0);
        assert_eq!(result.write_errors, 0);
        assert_eq!(result.images_processed, 0);
        assert_eq!(result.detect_errors, 0);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre-ml --lib pipeline::tests::run_face_pipeline_on_empty_input_is_a_noop`
Expected: COMPILE ERROR (`FacesRunResult` has no field `images_processed`/`detect_errors` yet).

- [ ] **Step 3: Implement**

Replace `crates/videre-ml/src/pipeline.rs`'s `FacesRunResult` struct and `run_face_pipeline` function (everything from `pub struct FacesRunResult {` through the closing `}` of `run_face_pipeline`, i.e. current lines 4-107) with:

```rust
pub struct FacesRunResult {
    pub total_faces: usize,
    pub write_errors: usize,
    pub images_processed: usize,
    pub detect_errors: usize,
}

/// Detects, embeds, and writes faces for the given (path, hash) pairs -
/// callers are responsible for deciding which hashes need processing (e.g.
/// "not already in the faces table" for incremental use, or "everything"
/// for --reprocess). Chunks work by `batch` images per embedding call, same
/// as dupe-faces has always done.
pub fn run_face_pipeline(
    conn: &Connection,
    to_process: &[(String, String)],
    batch: usize,
    dry_run: bool,
    silent: bool,
) -> Result<FacesRunResult> {
    use crate::{face_align, face_detect, face_embed, face_models};
    use half::f16;

    if to_process.is_empty() {
        return Ok(FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 });
    }

    let (det_path, rec_path) = face_models::buffalo_l_paths()?;
    let mut detector = face_detect::FaceDetector::new(&det_path)?;
    let mut embedder = face_embed::FaceEmbedder::new(&rec_path)?;

    let mut progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);

    let mut total_faces = 0usize;
    let mut write_errors = 0usize;
    let mut images_processed = 0usize;
    let mut detect_errors = 0usize;

    for chunk in to_process.chunks(batch) {
        struct ChunkEntry {
            path: String,
            hash: String,
            detections: Vec<face_detect::Detection>,
            n_crops: usize,
        }
        let mut chunk_entries: Vec<ChunkEntry> = Vec::new();
        let mut chunk_crops: Vec<image::RgbImage> = Vec::new();

        for (path, hash) in chunk {
            images_processed += 1;
            let img = match load_image(path) {
                Some(i) => i,
                None => { progress.tick(); continue; }
            };
            let detections = match detector.detect(&img) {
                Ok(d) => d,
                Err(e) => {
                    progress.println(&format!("detect failed {path}: {e}"));
                    detect_errors += 1;
                    progress.tick();
                    continue;
                }
            };
            if detections.is_empty() { progress.tick(); continue; }

            let crops: Vec<image::RgbImage> = detections.iter()
                .map(|d| face_align::align_face(&img, &d.landmarks))
                .collect();

            let n_crops = crops.len();
            chunk_crops.extend(crops);
            chunk_entries.push(ChunkEntry { path: path.clone(), hash: hash.clone(), detections, n_crops });
            progress.tick();
        }

        if chunk_crops.is_empty() { continue; }

        let all_embeddings = match embedder.embed_batch(&chunk_crops) {
            Ok(e) => e,
            Err(e) => {
                progress.println(&format!("embed_batch failed: {e}"));
                detect_errors += chunk_entries.len();
                continue;
            }
        };

        let mut emb_offset = 0;
        for entry in &chunk_entries {
            let n = entry.n_crops;
            let embs = &all_embeddings[emb_offset..emb_offset + n];
            emb_offset += n;

            let rows: Vec<videre_core::face_db::FaceRow> = entry.detections.iter().zip(embs.iter()).map(|(det, emb)| {
                let [x1, y1, x2, y2] = det.bbox;
                let bbox = format!("{},{},{},{}", x1 as i32, y1 as i32, (x2 - x1) as i32, (y2 - y1) as i32);
                let lm_str: String = det.landmarks.iter()
                    .flat_map(|[x, y]| [x.to_string(), y.to_string()])
                    .collect::<Vec<_>>().join(",");
                let embedding: Vec<u8> = emb.iter()
                    .flat_map(|&v| f16::from_f32(v).to_le_bytes())
                    .collect();
                videre_core::face_db::FaceRow {
                    hash: entry.hash.clone(), bbox, landmark: Some(lm_str),
                    embedding, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0,
                }
            }).collect();

            total_faces += rows.len();
            if !dry_run {
                if let Err(e) = videre_core::face_db::replace_faces_for_hash(conn, &entry.hash, &rows) {
                    progress.println(&format!("write failed {}: {e}", entry.path));
                    write_errors += 1;
                }
            }
        }
    }

    progress.finish();

    Ok(FacesRunResult { total_faces, write_errors, images_processed, detect_errors })
}
```

Note the three tick-placement changes from the original code, all deliberate:
- The `load_image` failure branch (`None => continue`) and the "detections is empty" branch (`if detections.is_empty() { continue; }`) now call `progress.tick()` before `continue`, since both are still "one image attempted" - the original code's bare `continue` there would otherwise skip incrementing progress for those images.
- The `detector.detect` failure branch calls `progress.tick()` after `progress.println(...)` and `detect_errors += 1`, for the same reason.
- The success path's `progress.tick()` moved to the end of the `for (path, hash) in chunk` loop body (after `chunk_entries.push(...)`), so it fires exactly once per image regardless of which branch was taken above - every `continue` and the natural fall-through both still result in exactly one `tick()` per iteration of the outer `for (path, hash) in chunk` loop.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-ml --lib pipeline::`
Expected: PASS (2 tests: `run_face_pipeline_on_empty_input_is_a_noop`, `run_clustering_on_empty_db_does_not_error` - the latter is untouched by this task and still uses the old 4-arg `run_clustering` signature, which still exists at this point since Task 3 changes it).

Run: `cargo test --workspace`
Expected: PASS, 190 total (unchanged from Task 1 - no tests added or removed, one existing test's assertions extended). No compiler warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/videre-ml/src/pipeline.rs
git commit -m "feat: run_face_pipeline reports progress via Progress instead of per-image prints"
```

---

### Task 3: `run_clustering` returns data; `faces.rs` and `watch.rs` consume it

`run_clustering`'s signature change ripples into both of its callers (`faces.rs` and `watch.rs`), which live in the same `videre` binary crate as each other - if only one of the two callers were fixed, the whole crate would fail to compile and no test in it could run. This task therefore updates all three files together.

**Files:**
- Modify: `crates/videre-ml/src/pipeline.rs`, `crates/videre/src/commands/faces.rs`, `crates/videre/src/commands/watch.rs`

- [ ] **Step 1: Write the failing test change**

In `crates/videre-ml/src/pipeline.rs`'s `#[cfg(test)] mod tests` block, update `run_clustering_on_empty_db_does_not_error`:

```rust
    #[test]
    fn run_clustering_on_empty_db_does_not_error() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        let result = run_clustering(&conn, 0.6, 3).unwrap();
        assert!(result.is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre-ml --lib pipeline::tests::run_clustering_on_empty_db_does_not_error`
Expected: COMPILE ERROR (`run_clustering` still takes 4 arguments and returns `Result<()>`, not `Result<Option<ClusteringResult>>`).

- [ ] **Step 3: Implement `pipeline.rs`'s `run_clustering`**

Replace `crates/videre-ml/src/pipeline.rs`'s `run_clustering` function (the block starting `/// Re-runs DBSCAN clustering...` through its closing `}`) with:

```rust
pub struct ClusteringResult {
    pub total_faces: usize,
    pub clustered_faces: usize,
    pub cluster_count: usize,
}

/// Re-runs DBSCAN clustering over every face embedding currently in the
/// database - safe to call whether or not run_face_pipeline found anything
/// new, since re-clustering is idempotent. Returns `None` when there are no
/// faces in the database to cluster; callers decide whether/how to report
/// that.
pub fn run_clustering(
    conn: &Connection,
    eps: f32,
    min_cluster_size: usize,
) -> Result<Option<ClusteringResult>> {
    let all_embs = videre_core::face_db::load_face_embeddings(conn)?;
    if all_embs.is_empty() {
        return Ok(None);
    }
    let assignments = videre_core::face_cluster::dbscan_cosine(&all_embs, eps, min_cluster_size);
    videre_core::face_db::update_cluster_assignments(conn, &assignments)?;
    let clustered_faces = assignments.iter().filter(|(_, c)| c.is_some()).count();
    let cluster_count = assignments
        .iter()
        .filter_map(|(_, c)| *c)
        .collect::<std::collections::HashSet<_>>()
        .len();
    Ok(Some(ClusteringResult { total_faces: all_embs.len(), clustered_faces, cluster_count }))
}
```

- [ ] **Step 4: Run the pipeline.rs tests**

Run: `cargo test -p videre-ml --lib pipeline::`
Expected: PASS (2 tests). This crate (`videre-ml`) compiles independently of `videre` (the binary crate containing `faces.rs`/`watch.rs`), so this step can pass even though `videre` itself won't compile yet until Steps 5-6 below.

- [ ] **Step 5: Write `faces.rs`'s new unit tests**

Add a `#[cfg(test)] mod tests` block at the end of `crates/videre/src/commands/faces.rs` (the file currently has no test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_summary_no_errors() {
        let result = FacesRunResult { total_faces: 187, write_errors: 0, images_processed: 234, detect_errors: 0 };
        let clustering = Some(ClusteringResult { total_faces: 187, clustered_faces: 152, cluster_count: 14 });
        let summary = format_summary(&result, clustering, 0.6, std::time::Duration::from_secs(41));
        assert_eq!(
            summary,
            "234 image(s) processed, 187 face(s) found, 152/187 clustered into 14 people (eps=0.60), done in 41s"
        );
    }

    #[test]
    fn format_summary_with_errors() {
        let result = FacesRunResult { total_faces: 187, write_errors: 2, images_processed: 234, detect_errors: 1 };
        let clustering = Some(ClusteringResult { total_faces: 187, clustered_faces: 152, cluster_count: 14 });
        let summary = format_summary(&result, clustering, 0.6, std::time::Duration::from_secs(41));
        assert_eq!(
            summary,
            "234 image(s) processed, 187 face(s) found, 152/187 clustered into 14 people (eps=0.60), done in 41s, 3 error(s) (see above)"
        );
    }

    #[test]
    fn format_summary_no_faces_found() {
        let result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 234, detect_errors: 0 };
        let summary = format_summary(&result, None, 0.6, std::time::Duration::from_secs(41));
        assert_eq!(summary, "234 image(s) processed, 0 face(s) found, done in 41s");
    }

    #[test]
    fn format_clustering_only_summary_some() {
        let clustering = Some(ClusteringResult { total_faces: 187, clustered_faces: 152, cluster_count: 14 });
        let summary = format_clustering_only_summary(clustering, 0.6);
        assert_eq!(summary, "152/187 faces clustered into 14 people (eps=0.60)");
    }

    #[test]
    fn format_clustering_only_summary_none() {
        let summary = format_clustering_only_summary(None, 0.6);
        assert_eq!(summary, "no faces in database to cluster (eps=0.60)");
    }
}
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test -p videre --lib faces::tests`
Expected: COMPILE ERROR (`format_summary`/`format_clustering_only_summary` don't exist yet, and `run()`'s existing summary logic uses the old `run_clustering` signature).

- [ ] **Step 7: Implement `faces.rs`**

Replace the entire contents of `crates/videre/src/commands/faces.rs` from `use anyhow::Result;` through the end of `pub fn run(args: FacesArgs) -> Result<()> { ... }` (i.e. everything before the test module added in Step 5) with:

```rust
use anyhow::Result;
use videre_core::face_db;
use videre_ml::pipeline::{run_clustering, run_face_pipeline, ClusteringResult, FacesRunResult};
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct FacesArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,
    #[arg(long)] reprocess: bool,
    /// Skip detection; just re-run clustering on existing embeddings
    #[arg(long)] recluster: bool,
    #[arg(long, default_value = "8")] batch: usize,
    #[arg(long)] dry_run: bool,
    #[arg(long)] silent: bool,
    /// DBSCAN cosine-distance radius (0 = identical, 2 = opposite). Default 0.6.
    #[arg(long, default_value = "0.6")] eps: f32,
    /// Minimum faces per cluster (below this, faces are left as singletons). Default 3.
    #[arg(long, default_value = "3")] min_cluster_size: usize,
}

pub fn run(args: FacesArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    if !db.exists() {
        anyhow::bail!("{:?} does not exist", db);
    }
    let conn = videre_core::db::open_wal(&db)?;
    face_db::create_faces_table(&conn)?;

    // 1. Determine which hashes to process
    let all_paths: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT path, hash FROM file_hashes WHERE ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')"
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let skip_hashes: std::collections::HashSet<String> = if args.reprocess {
        std::collections::HashSet::new()
    } else {
        face_db::hashes_with_faces(&conn)?.into_iter().collect()
    };

    // Gap 1: Deduplicate by hash - one representative path per hash
    let mut seen_hashes = std::collections::HashSet::new();
    let to_process: Vec<(String, String)> = all_paths.into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash) && seen_hashes.insert(hash.clone()))
        .collect();

    if args.recluster || to_process.is_empty() {
        if !args.silent && to_process.is_empty() && !args.recluster {
            eprintln!("All hashes already processed.");
        }
        // Skip detection; jump straight to clustering
        if !args.dry_run {
            let clustering = run_clustering(&conn, args.eps, args.min_cluster_size)?;
            if !args.silent {
                eprintln!("{}", format_clustering_only_summary(clustering, args.eps));
            }
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let result = run_face_pipeline(&conn, &to_process, args.batch, args.dry_run, args.silent)?;

    // Cluster whenever there are faces in the DB, not only when new faces were found
    let clustering = if !args.dry_run {
        run_clustering(&conn, args.eps, args.min_cluster_size)?
    } else {
        None
    };

    if !args.silent {
        eprintln!("{}", format_summary(&result, clustering, args.eps, started.elapsed()));
    }

    if result.write_errors > 0 || result.detect_errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Assembles the single consolidated summary line printed after both
/// detection and clustering finish. `pub(crate)` since `watch.rs`'s faces
/// stage does not call this one (it has no per-run elapsed-time figure to
/// report - see `format_clustering_only_summary` for its equivalent), but
/// keeping visibility consistent with its sibling function below.
pub(crate) fn format_summary(
    result: &FacesRunResult,
    clustering: Option<ClusteringResult>,
    eps: f32,
    elapsed: std::time::Duration,
) -> String {
    let mut s = format!(
        "{} image(s) processed, {} face(s) found",
        result.images_processed, result.total_faces
    );
    if let Some(c) = &clustering {
        s.push_str(&format!(
            ", {}/{} clustered into {} people (eps={:.2})",
            c.clustered_faces, c.total_faces, c.cluster_count, eps
        ));
    }
    s.push_str(&format!(", done in {}s", elapsed.as_secs()));
    let error_count = result.write_errors + result.detect_errors;
    if error_count > 0 {
        s.push_str(&format!(", {error_count} error(s) (see above)"));
    }
    s
}

/// Assembles the summary line for the `--recluster` (and "nothing new to
/// process, but recluster anyway") path, where no detection ran this
/// invocation - so there is no image count or elapsed-time figure to report.
/// `pub(crate)` since `watch.rs`'s faces stage also calls this.
pub(crate) fn format_clustering_only_summary(clustering: Option<ClusteringResult>, eps: f32) -> String {
    match clustering {
        Some(c) => format!(
            "{}/{} faces clustered into {} people (eps={:.2})",
            c.clustered_faces, c.total_faces, c.cluster_count, eps
        ),
        None => format!("no faces in database to cluster (eps={eps:.2})"),
    }
}
```

- [ ] **Step 8: Fix `watch.rs`'s compile break**

`watch.rs:152,161` call `run_face_pipeline`/`run_clustering` with the old signature. In `crates/videre/src/commands/watch.rs`, add the new import (in alphabetical position among the existing `use` lines, after `use anyhow::Result;` and before `use rayon::prelude::*;` - matching the file's existing top-of-file import block):

```rust
use anyhow::Result;
use super::faces::format_clustering_only_summary;
use videre::{hasher, scanner, sqlite_output, types};
use videre_core::{db, face_db};
use videre_ml::pipeline::{run_clustering, run_face_pipeline};
use rayon::prelude::*;
use std::path::PathBuf;
use std::time::Duration;
```

Replace the `run_faces_stage` function (currently `watch.rs:139-163`) with:

```rust
fn run_faces_stage(args: &WatchArgs, conn: &rusqlite::Connection) -> Result<()> {
    let all_paths = dedup_paths_by_hash(
        conn,
        "ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')",
    )?;
    let skip_hashes: std::collections::HashSet<String> =
        face_db::hashes_with_faces(conn)?.into_iter().collect();
    let to_process: Vec<(String, String)> = all_paths
        .into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash))
        .collect();

    if !to_process.is_empty() {
        let result = run_face_pipeline(conn, &to_process, 8, false, args.silent)?;
        if !args.silent {
            eprintln!(
                "videre watch: faces stage processed {} new hash(es), {} face(s)",
                to_process.len(),
                result.total_faces
            );
        }
    }
    let clustering = run_clustering(conn, 0.6, 3)?;
    if !args.silent {
        eprintln!("videre watch: {}", format_clustering_only_summary(clustering, 0.6));
    }
    Ok(())
}
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test -p videre-ml --lib pipeline::`
Expected: PASS (2 tests).

Run: `cargo test -p videre --lib faces::tests`
Expected: PASS (5 tests: `format_summary_no_errors`, `format_summary_with_errors`, `format_summary_no_faces_found`, `format_clustering_only_summary_some`, `format_clustering_only_summary_none`).

Run: `cargo test --workspace`
Expected: PASS, 195 total (190 at the end of Task 1, plus 5 new `faces.rs` tests from this task; Task 2 added no new tests, only extended one existing assertion set). No compiler warnings (`cargo build --workspace 2>&1 | grep -i warning` empty) - this specifically confirms `watch.rs` compiles cleanly with the new import and call site.

No existing test in `crates/videre/tests/watch.rs` asserts on `run_faces_stage`'s output text or calls it directly (it is a private, non-`pub` function only reachable from within `watch.rs` itself, and no integration test constructs a scenario that reaches the faces stage's specific eprintln text), so no test file outside `faces.rs` needs updating for this task.

- [ ] **Step 10: Commit**

```bash
git add crates/videre-ml/src/pipeline.rs crates/videre/src/commands/faces.rs crates/videre/src/commands/watch.rs
git commit -m "feat: consolidated faces summary line; run_clustering returns data instead of printing"
```

---

### Task 4: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full suite**

Run: `cargo test --workspace`
Expected: PASS, 195 total, 0 failed, 0 compiler warnings (`cargo build --workspace 2>&1 | grep -i warning` empty).

- [ ] **Step 2: Release build**

Run: `cargo build --release`
Expected: succeeds with no errors.

- [ ] **Step 3: Manual TTY verification**

This step cannot be automated (see the design spec's Testing section on why `Progress`'s `IsTerminal` branching is not unit tested) - it must be run by a human at a real terminal, not by an agent, since an agent's shell is typically not a TTY.

```bash
# Requires a database with at least a few dozen images already scanned
# (e.g. via 'videre scan <dir>'). Run in a real terminal, not piped.
./target/release/videre faces --db <path-to-a-populated-db>
```

Expected: an in-place percentage bar appears and updates during detection, clears cleanly, and is followed by exactly one summary line matching the format from Task 3 (e.g. `"42 image(s) processed, 8 face(s) found, 6/8 clustered into 2 people (eps=0.60), done in 3s"`). No per-image `[faces] ...` lines should appear anywhere in the output.

- [ ] **Step 4: Manual non-TTY verification**

```bash
./target/release/videre faces --db <path-to-a-populated-db> > /tmp/faces-out.log 2>&1
cat /tmp/faces-out.log
```

Expected: `/tmp/faces-out.log` contains periodic `N/total images processed` lines (no bar control characters, no per-image spam) followed by the same summary line format as Step 3.

- [ ] **Step 5: Record results**

PASS/FAIL per step; any FAIL loops back to the owning task.
