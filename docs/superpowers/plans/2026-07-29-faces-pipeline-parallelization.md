# Faces Pipeline Profiling + Parallelization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-stage timing instrumentation to `videre faces`'s detection
pipeline, then restructure it from strictly serial to a multi-worker pipeline
(approach A - symmetric worker pool), per
`docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md`.

**Architecture:** `run_face_pipeline` (`crates/videre-ml/src/pipeline.rs`)
currently processes one image at a time on a single thread. This plan first
adds optional profiling (Task 1-2), then extracts two pure/testable helpers
(Task 3-4), gives each ONNX session an explicit low intra-op thread cap (Task
5), restructures the pipeline into N worker threads round-robin-partitioning
the work with a single coordinator thread as the sole SQLite writer (Task 6),
exposes a `--workers` flag (Task 7), and finishes with a real regression +
speedup measurement against the pre-change baseline (Task 8) plus doc updates
(Task 9). Memory `architecture-faces-pipeline-parallelization.md` explicitly
requires Task 8's empirical validation before this is considered settled -
do not skip it.

**Tech Stack:** Rust, `ort` 2.0.0-rc.12 (ONNX Runtime bindings), `std::thread::scope`
+ `std::sync::mpsc` (no new dependency), `rusqlite`.

**Note on testing:** real ONNX-model-dependent detection has no unit test
coverage in this codebase (would require downloaded model weights) - existing
coverage of `run_face_pipeline` is limited to the empty-input no-op case and
synthetic-embedding clustering tests. This plan follows the same constraint:
new pure logic (partitioning, result aggregation, profile formatting) gets
real TDD; the actual concurrent ONNX pipeline is verified by building it and
running it against the real library (Task 8), not a new model-dependent
integration test.

---

### Task 1: `ProfileStats` accumulator + wire into the (still serial) pipeline

**Files:**
- Modify: `crates/videre-ml/src/pipeline.rs`

- [ ] **Step 1: Write the failing test for `ProfileStats::merge`**

Add to the `#[cfg(test)] mod tests` block in `crates/videre-ml/src/pipeline.rs`:

```rust
    #[test]
    fn profile_stats_merge_sums_all_fields() {
        let mut a = ProfileStats {
            load_heic: std::time::Duration::from_millis(100),
            load_other: std::time::Duration::from_millis(50),
            detect: std::time::Duration::from_millis(200),
            align: std::time::Duration::from_millis(10),
            embed: std::time::Duration::from_millis(80),
            db_write: std::time::Duration::from_millis(5),
            count_heic: 2,
            count_other: 3,
        };
        let b = ProfileStats {
            load_heic: std::time::Duration::from_millis(20),
            load_other: std::time::Duration::from_millis(10),
            detect: std::time::Duration::from_millis(40),
            align: std::time::Duration::from_millis(2),
            embed: std::time::Duration::from_millis(16),
            db_write: std::time::Duration::from_millis(1),
            count_heic: 1,
            count_other: 1,
        };
        a.merge(b);
        assert_eq!(a.load_heic, std::time::Duration::from_millis(120));
        assert_eq!(a.load_other, std::time::Duration::from_millis(60));
        assert_eq!(a.detect, std::time::Duration::from_millis(240));
        assert_eq!(a.align, std::time::Duration::from_millis(12));
        assert_eq!(a.embed, std::time::Duration::from_millis(96));
        assert_eq!(a.db_write, std::time::Duration::from_millis(6));
        assert_eq!(a.count_heic, 3);
        assert_eq!(a.count_other, 4);
    }

    #[test]
    fn format_profile_report_computes_per_image_averages() {
        let stats = ProfileStats {
            load_heic: std::time::Duration::from_millis(1000),
            load_other: std::time::Duration::from_millis(400),
            detect: std::time::Duration::from_millis(500),
            align: std::time::Duration::from_millis(50),
            embed: std::time::Duration::from_millis(200),
            db_write: std::time::Duration::from_millis(10),
            count_heic: 2,
            count_other: 4,
        };
        let report = format_profile_report(&stats);
        assert_eq!(
            report,
            "--profile: 6 image(s) (2 heic, 4 other)\n\
             load: heic avg 500ms (n=2), other avg 100ms (n=4)\n\
             detect: avg 83ms\n\
             align: avg 8ms\n\
             embed: avg 33ms\n\
             db_write: avg 1ms"
        );
    }

    #[test]
    fn format_profile_report_handles_zero_counts_without_dividing_by_zero() {
        let stats = ProfileStats::default();
        let report = format_profile_report(&stats);
        assert_eq!(report, "--profile: 0 image(s) (0 heic, 0 other)");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre-ml profile_stats_merge_sums_all_fields format_profile_report -- --nocapture`
Expected: FAIL with "cannot find type `ProfileStats`" / "cannot find function `format_profile_report`" (compile error, not a runtime assertion failure - that's fine, it means the types don't exist yet).

- [ ] **Step 3: Implement `ProfileStats` and `format_profile_report`**

Add near the top of `crates/videre-ml/src/pipeline.rs` (after the existing `use` statements, before `FacesRunResult`):

```rust
/// Per-stage timing accumulator for `videre faces --profile`. All durations
/// are cumulative across every image processed; `format_profile_report`
/// divides by the relevant count to report per-image averages. Load time is
/// tracked separately for HEIC (goes through a `qlmanage` subprocess) vs.
/// everything else, since that's the one stage known to differ sharply by
/// file type - see `docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md`.
#[derive(Debug, Default, Clone)]
pub struct ProfileStats {
    pub load_heic: std::time::Duration,
    pub load_other: std::time::Duration,
    pub detect: std::time::Duration,
    pub align: std::time::Duration,
    pub embed: std::time::Duration,
    pub db_write: std::time::Duration,
    pub count_heic: usize,
    pub count_other: usize,
}

impl ProfileStats {
    /// Merges another worker's (or the coordinator's) stats into this one -
    /// used once the pipeline is multi-threaded (Task 6) to combine each
    /// worker's local accumulator plus the coordinator's own db_write timing
    /// into a single report at the end of the run.
    pub fn merge(&mut self, other: ProfileStats) {
        self.load_heic += other.load_heic;
        self.load_other += other.load_other;
        self.detect += other.detect;
        self.align += other.align;
        self.embed += other.embed;
        self.db_write += other.db_write;
        self.count_heic += other.count_heic;
        self.count_other += other.count_other;
    }
}

/// Formats a `--profile` report: per-image averages for each pipeline stage,
/// with load time split HEIC vs. other. Divisions guard against zero counts
/// (an empty or all-one-type run) rather than panicking.
pub fn format_profile_report(stats: &ProfileStats) -> String {
    let total = stats.count_heic + stats.count_other;
    let avg_ms = |total: std::time::Duration, count: usize| -> u128 {
        if count == 0 { 0 } else { total.as_millis() / count as u128 }
    };
    let mut s = format!(
        "--profile: {total} image(s) ({} heic, {} other)",
        stats.count_heic, stats.count_other
    );
    if total == 0 {
        return s;
    }
    s.push_str(&format!(
        "\nload: heic avg {}ms (n={}), other avg {}ms (n={})",
        avg_ms(stats.load_heic, stats.count_heic), stats.count_heic,
        avg_ms(stats.load_other, stats.count_other), stats.count_other,
    ));
    s.push_str(&format!("\ndetect: avg {}ms", avg_ms(stats.detect, total)));
    s.push_str(&format!("\nalign: avg {}ms", avg_ms(stats.align, total)));
    s.push_str(&format!("\nembed: avg {}ms", avg_ms(stats.embed, total)));
    s.push_str(&format!("\ndb_write: avg {}ms", avg_ms(stats.db_write, total)));
    s
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-ml profile_stats_merge_sums_all_fields format_profile_report -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Wire timing into the existing serial loop**

Change `run_face_pipeline`'s signature and body in `crates/videre-ml/src/pipeline.rs` to accept an optional `&mut ProfileStats` and time each stage. Replace the function signature:

```rust
pub fn run_face_pipeline(
    conn: &Connection,
    to_process: &[(String, String)],
    batch: usize,
    dry_run: bool,
    silent: bool,
    mut profile: Option<&mut ProfileStats>,
) -> Result<FacesRunResult> {
```

Inside the per-image loop, wrap the load call:

```rust
        for (path, hash) in chunk {
            images_processed += 1;
            let is_heic = path.to_lowercase().ends_with(".heic");
            let load_start = std::time::Instant::now();
            let img = match load_image(path) {
                Ok(i) => i,
                Err(msg) => {
                    progress.println(&format!("skipping {path}: {msg}"));
                    detect_errors += 1;
                    progress.tick();
                    continue;
                }
            };
            if let Some(p) = profile.as_deref_mut() {
                let d = load_start.elapsed();
                if is_heic { p.load_heic += d; p.count_heic += 1; } else { p.load_other += d; p.count_other += 1; }
            }
            let detect_start = std::time::Instant::now();
            let detections = match detector.detect(&img) {
                Ok(d) => d,
                Err(e) => {
                    progress.println(&format!("detect failed {path}: {e}"));
                    detect_errors += 1;
                    progress.tick();
                    continue;
                }
            };
            if let Some(p) = profile.as_deref_mut() { p.detect += detect_start.elapsed(); }
            if detections.is_empty() {
                // Record the no-face image as scanned so it is never
                // re-detected on a later (resumed) run.
                if !dry_run {
                    let _ = videre_core::face_db::mark_scanned(conn, hash);
                }
                progress.tick();
                continue;
            }

            let align_start = std::time::Instant::now();
            let crops: Vec<image::RgbImage> = detections.iter()
                .map(|d| face_align::align_face(&img, &d.landmarks))
                .collect();
            if let Some(p) = profile.as_deref_mut() { p.align += align_start.elapsed(); }

            let n_crops = crops.len();
            chunk_crops.extend(crops);
            chunk_entries.push(ChunkEntry { path: path.clone(), hash: hash.clone(), detections, n_crops });
            progress.tick();
        }

        if chunk_crops.is_empty() { continue; }

        let embed_start = std::time::Instant::now();
        let all_embeddings = match embedder.embed_batch(&chunk_crops) {
            Ok(e) => e,
            Err(e) => {
                progress.println(&format!("embed_batch failed: {e}"));
                detect_errors += chunk_entries.len();
                continue;
            }
        };
        if let Some(p) = profile.as_deref_mut() { p.embed += embed_start.elapsed(); }
```

And around the DB write inside the `for entry in &chunk_entries` loop:

```rust
            total_faces += rows.len();
            if !dry_run {
                let write_start = std::time::Instant::now();
                let write_result = videre_core::face_db::replace_faces_for_hash(conn, &entry.hash, &rows);
                if let Some(p) = profile.as_deref_mut() { p.db_write += write_start.elapsed(); }
                if let Err(e) = write_result {
                    progress.println(&format!("write failed {}: {e}", entry.path));
                    write_errors += 1;
                } else {
                    // Mark scanned only after the faces are durably written, so
                    // an interrupt before this point re-processes the hash.
                    let _ = videre_core::face_db::mark_scanned(conn, &entry.hash);
                }
            }
```

- [ ] **Step 6: Update the existing empty-input test call site**

`run_face_pipeline_on_empty_input_is_a_noop` (already in the test module) calls `run_face_pipeline(&conn, &[], 8, false, true)` - update it to pass `None` for the new parameter:

```rust
        let result = run_face_pipeline(&conn, &[], 8, false, true, None).unwrap();
```

- [ ] **Step 7: Update `crates/videre/src/commands/faces.rs`'s call site**

Find `let result = run_face_pipeline(&conn, &to_process, args.batch, args.dry_run, args.silent)?;` and change to:

```rust
    let result = run_face_pipeline(&conn, &to_process, args.batch, args.dry_run, args.silent, None)?;
```

(The `--profile` CLI flag itself is added in Task 2 - this step just keeps the build green after the signature change, passing `None` for now.)

- [ ] **Step 8: Update `crates/videre/src/commands/watch.rs`'s call site**

Find `let result = run_face_pipeline(conn, &to_process, 8, false, args.silent)?;` and change to:

```rust
        let result = run_face_pipeline(conn, &to_process, 8, false, args.silent, None)?;
```

- [ ] **Step 9: Run the full test suite and build**

```bash
cargo test --workspace
cargo build --workspace
```

Expected: all tests pass (including the 3 new ones), clean build.

- [ ] **Step 10: Commit**

```bash
git add crates/videre-ml/src/pipeline.rs crates/videre/src/commands/faces.rs crates/videre/src/commands/watch.rs
git commit -m "feat(faces): add ProfileStats accumulator, wire into serial pipeline"
```

---

### Task 2: `--profile` CLI flag + run it against the real library

**Files:**
- Modify: `crates/videre/src/commands/faces.rs`

- [ ] **Step 1: Add the flag**

In `FacesArgs` (`crates/videre/src/commands/faces.rs`), add:

```rust
    /// Print per-stage timing (load/detect/align/embed/db_write, load split
    /// HEIC vs. other) averaged per image, after the run finishes. A tuning
    /// tool, not part of the normal summary - see
    /// docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md.
    #[arg(long)] profile: bool,
```

- [ ] **Step 2: Wire it through `run`**

Change the call site from Task 1 Step 7:

```rust
    let mut profile_stats = videre_ml::pipeline::ProfileStats::default();
    let result = run_face_pipeline(
        &conn, &to_process, args.batch, args.dry_run, args.silent,
        if args.profile { Some(&mut profile_stats) } else { None },
    )?;
```

Add the import at the top of the file: change
`use videre_ml::pipeline::{run_clustering, run_face_pipeline, ClusteringResult, FacesRunResult};`
to
`use videre_ml::pipeline::{format_profile_report, run_clustering, run_face_pipeline, ClusteringResult, FacesRunResult, ProfileStats};`

(`ProfileStats` needs to be constructible from outside `videre-ml` - confirm it's `pub` with `pub` fields, as written in Task 1; if the compiler complains about private fields, make them `pub` rather than adding a constructor, to keep this simple.)

After the existing summary-printing block (`if !args.silent { eprintln!("{}", format_summary(...)); ... }`), add:

```rust
    if args.profile {
        eprintln!("{}", format_profile_report(&profile_stats));
    }
```

- [ ] **Step 3: Build**

```bash
cargo build --workspace
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/videre/src/commands/faces.rs
git commit -m "feat(faces): add --profile flag"
```

- [ ] **Step 5: Run it against the real library and report the numbers**

This is the design's explicit checkpoint - Phase 2's worker-count default (Task 7) should be informed by real data, not a guess. Run a bounded sample (fast enough to not take hours, large enough to be representative - `--limit 300` covers both HEIC and non-HEIC files given the library's ~18% HEIC mix):

```bash
cargo build --release
./target/release/videre faces --profile --limit 300 --silent
```

Expected: prints something like `--profile: N image(s) (H heic, O other)` followed by the five per-stage average lines. **Report these exact numbers** (don't estimate or round from memory) - they inform:
- Whether load (and specifically the HEIC/other split) is still the dominant cost as expected, which validates round-robin partitioning's assumption that mixing file types across workers matters
- A reasonable `--workers` default for Task 7 (see that task for how the numbers are used)

If this step surfaces something unexpected (e.g., embed dominates instead of load+detect), stop and note it rather than proceeding with Task 6's design unchanged - that would be new information the spec's authors didn't have.

---

### Task 3: Extract and TDD `round_robin_partition`

**Files:**
- Modify: `crates/videre-ml/src/pipeline.rs`

- [ ] **Step 1: Write the failing tests**

Add to the test module:

```rust
    #[test]
    fn round_robin_partition_covers_every_item_exactly_once() {
        let items: Vec<i32> = (0..10).collect();
        let parts = round_robin_partition(&items, 3);
        assert_eq!(parts.len(), 3);
        let mut seen: Vec<i32> = parts.iter().flatten().copied().collect();
        seen.sort();
        assert_eq!(seen, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn round_robin_partition_assigns_by_index_modulo_worker_count() {
        let items: Vec<i32> = (0..6).collect();
        let parts = round_robin_partition(&items, 3);
        assert_eq!(parts[0], vec![0, 3]);
        assert_eq!(parts[1], vec![1, 4]);
        assert_eq!(parts[2], vec![2, 5]);
    }

    #[test]
    fn round_robin_partition_more_workers_than_items_leaves_some_empty() {
        let items: Vec<i32> = vec![10, 20];
        let parts = round_robin_partition(&items, 5);
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], vec![10]);
        assert_eq!(parts[1], vec![20]);
        assert!(parts[2].is_empty());
        assert!(parts[3].is_empty());
        assert!(parts[4].is_empty());
    }

    #[test]
    fn round_robin_partition_empty_items_returns_empty_partitions() {
        let items: Vec<i32> = vec![];
        let parts = round_robin_partition(&items, 4);
        assert_eq!(parts.len(), 4);
        assert!(parts.iter().all(|p: &Vec<i32>| p.is_empty()));
    }

    #[test]
    #[should_panic]
    fn round_robin_partition_zero_workers_panics() {
        let items: Vec<i32> = vec![1, 2, 3];
        round_robin_partition(&items, 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre-ml round_robin_partition -- --nocapture`
Expected: FAIL with "cannot find function `round_robin_partition`".

- [ ] **Step 3: Implement**

Add near `ProfileStats` in `crates/videre-ml/src/pipeline.rs`:

```rust
/// Splits `items` into `workers` partitions round-robin (item at index `i`
/// goes to partition `i % workers`), not contiguous chunks - this spreads
/// any clustering in the input order (e.g. a run of HEIC files from one
/// photo-import session sitting contiguously) evenly across workers instead
/// of letting one worker inherit a disproportionately slow subset. Panics if
/// `workers` is 0 (a caller bug, not a runtime condition to handle
/// gracefully - `--workers` is validated to be at least 1 before this is
/// called).
pub fn round_robin_partition<T: Clone>(items: &[T], workers: usize) -> Vec<Vec<T>> {
    assert!(workers > 0, "round_robin_partition requires at least 1 worker");
    let mut parts: Vec<Vec<T>> = vec![Vec::new(); workers];
    for (i, item) in items.iter().enumerate() {
        parts[i % workers].push(item.clone());
    }
    parts
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-ml round_robin_partition -- --nocapture`
Expected: PASS (5 tests, including the `should_panic` one).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-ml/src/pipeline.rs
git commit -m "feat(faces): add round_robin_partition, TDD'd"
```

---

### Task 4: Extract and TDD worker-message aggregation

**Files:**
- Modify: `crates/videre-ml/src/pipeline.rs`

**Context:** Task 6 restructures `run_face_pipeline` so each worker sends a
`WorkerMsg` per image (or per failed chunk) back to a coordinator thread,
which aggregates them into the same `FacesRunResult` the function has always
returned, and performs the DB write for `Faces` messages (workers never touch
the connection). This task defines that message type and the pure
apply-one-message-to-a-result-in-progress step, so it's unit-testable without
any real threads, ONNX models, or a real database connection.

- [ ] **Step 1: Write the failing tests**

Add to the test module:

```rust
    fn sample_face_row(hash: &str) -> videre_core::face_db::FaceRow {
        videre_core::face_db::FaceRow {
            hash: hash.to_string(),
            bbox: "0,0,10,10".to_string(),
            landmark: None,
            embedding: vec![0u8; 1024],
            cluster_id: None,
            person_label: None,
            confirmed: 0,
            is_primary: 0,
        }
    }

    #[test]
    fn apply_worker_msg_no_face_increments_images_processed_only() {
        let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };
        apply_worker_msg_counts(&mut result, &WorkerMsg::NoFace { hash: "h1".into() });
        assert_eq!(result.images_processed, 1);
        assert_eq!(result.total_faces, 0);
        assert_eq!(result.detect_errors, 0);
    }

    #[test]
    fn apply_worker_msg_faces_increments_processed_and_total_faces() {
        let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };
        let rows = vec![sample_face_row("h1"), sample_face_row("h1")];
        apply_worker_msg_counts(&mut result, &WorkerMsg::Faces { hash: "h1".into(), rows });
        assert_eq!(result.images_processed, 1);
        assert_eq!(result.total_faces, 2);
    }

    #[test]
    fn apply_worker_msg_image_error_increments_processed_and_detect_errors() {
        let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };
        apply_worker_msg_counts(&mut result, &WorkerMsg::ImageError);
        assert_eq!(result.images_processed, 1);
        assert_eq!(result.detect_errors, 1);
    }

    #[test]
    fn apply_worker_msg_embed_batch_error_increments_processed_and_detect_errors_by_n() {
        let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };
        apply_worker_msg_counts(&mut result, &WorkerMsg::EmbedBatchError { n: 5 });
        assert_eq!(result.images_processed, 5);
        assert_eq!(result.detect_errors, 5);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre-ml apply_worker_msg -- --nocapture`
Expected: FAIL with "cannot find type `WorkerMsg`" / "cannot find function `apply_worker_msg_counts`".

- [ ] **Step 3: Implement**

Add near `FacesRunResult` in `crates/videre-ml/src/pipeline.rs`:

```rust
/// One worker thread's report of a single image's (or, for
/// `EmbedBatchError`, a whole failed chunk's) outcome, sent to the
/// coordinator thread over an `mpsc` channel. The coordinator is the only
/// thing that touches the database (see Task 6) - `Faces` carries everything
/// needed to call `replace_faces_for_hash` there. Per-image error messages
/// (`skipping ...`, `detect failed ...`) are printed by the worker itself via
/// the shared, thread-safe `Progress::println` (see
/// `videre_core::progress::Progress`'s doc comment) - `ImageError`/
/// `EmbedBatchError` carry counts only, not message text, since the text was
/// already printed at the point of failure.
pub enum WorkerMsg {
    NoFace { hash: String },
    Faces { hash: String, rows: Vec<videre_core::face_db::FaceRow> },
    ImageError,
    EmbedBatchError { n: usize },
}

/// Updates `result`'s counters for one `WorkerMsg` - the part of handling a
/// message that's pure bookkeeping, independent of the (impure) DB write a
/// `Faces` message also triggers on the coordinator. Extracted so this
/// bookkeeping is unit-testable without a real `Connection`.
pub fn apply_worker_msg_counts(result: &mut FacesRunResult, msg: &WorkerMsg) {
    match msg {
        WorkerMsg::NoFace { .. } => {
            result.images_processed += 1;
        }
        WorkerMsg::Faces { rows, .. } => {
            result.images_processed += 1;
            result.total_faces += rows.len();
        }
        WorkerMsg::ImageError => {
            result.images_processed += 1;
            result.detect_errors += 1;
        }
        WorkerMsg::EmbedBatchError { n } => {
            result.images_processed += n;
            result.detect_errors += n;
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-ml apply_worker_msg -- --nocapture`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-ml/src/pipeline.rs
git commit -m "feat(faces): add WorkerMsg + apply_worker_msg_counts, TDD'd"
```

---

### Task 5: Cap each ONNX session's intra-op thread count

**Files:**
- Modify: `crates/videre-ml/src/face_models.rs`
- Modify: `crates/videre-ml/src/face_detect.rs`
- Modify: `crates/videre-ml/src/face_embed.rs`

**Context:** confirmed API (`ort` 2.0.0-rc.12 source,
`session/builder/impl_options.rs:52`): `SessionBuilder::with_intra_threads(self,
num_threads: usize) -> ort::Result<SessionBuilder>`, chained with `?` before
`.commit_from_file(...)`. Without this, each worker's session defaults to
using every core for its own intra-op pool - with N workers that's N-way
oversubscription of the machine's cores, which would make Task 6's
parallelization actively worse, not better.

- [ ] **Step 1: Update `build_session`**

In `crates/videre-ml/src/face_models.rs`, change:

```rust
pub fn build_session(model_path: &Path) -> Result<Session> {
    Session::builder()
        .context("create ort SessionBuilder")?
        .commit_from_file(model_path)
        .context("load ONNX model")
}
```

to:

```rust
/// Builds an ORT `Session` for a face model, capped to `intra_threads`
/// intra-op threads. When `run_face_pipeline` runs N workers concurrently
/// (see pipeline.rs), each worker's session must NOT default to using every
/// core - N sessions x "every core" oversubscribes the machine far worse
/// than the single-session baseline this comment used to describe. Pass a
/// small number (e.g. 2) per worker so N workers x intra_threads stays near
/// the machine's actual core count.
pub fn build_session(model_path: &Path, intra_threads: usize) -> Result<Session> {
    Session::builder()
        .context("create ort SessionBuilder")?
        .with_intra_threads(intra_threads)
        .context("set intra-op thread count")?
        .commit_from_file(model_path)
        .context("load ONNX model")
}
```

Also update the doc comment above the function (currently describes the
old CoreML investigation and says ORT uses "all cores by default" - keep the
CoreML history, since that's still true and valuable context, but the "all
cores by default" framing needs to reflect that callers now explicitly choose
the thread count):

```rust
/// Builds an ORT `Session` for a face model. Previously ran with ORT's
/// default all-core intra-op thread pool; now takes an explicit
/// `intra_threads` cap (see below) since `run_face_pipeline` runs multiple
/// worker threads concurrently, each with its own session - the macOS
/// CoreML execution provider was measured (2026-07-23) to give no speedup
/// for these InsightFace models (the SCRFD/ArcFace graphs don't accelerate
/// on CoreML, and it adds a multi-second per-process model-compile cost), so
/// it is intentionally not used. The dominant cost of `videre faces` is
/// SCRFD detection plus per-image loading (HEIC via qlmanage) and, per the
/// pipeline being fully serial until 2026-07-29, a lack of concurrency - see
/// docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md.
```

- [ ] **Step 2: Update `FaceDetector::new` and `FaceEmbedder::new`**

In `crates/videre-ml/src/face_detect.rs`, change:

```rust
    pub fn new(model_path: &Path) -> Result<Self> {
        let session = crate::face_models::build_session(model_path)
            .context("load SCRFD ONNX model")?;
        Ok(Self { session })
    }
```

to:

```rust
    pub fn new(model_path: &Path, intra_threads: usize) -> Result<Self> {
        let session = crate::face_models::build_session(model_path, intra_threads)
            .context("load SCRFD ONNX model")?;
        Ok(Self { session })
    }
```

In `crates/videre-ml/src/face_embed.rs`, make the same change to `FaceEmbedder::new`:

```rust
    pub fn new(model_path: &Path, intra_threads: usize) -> Result<Self> {
        let session = crate::face_models::build_session(model_path, intra_threads)
            .context("load ArcFace ONNX model")?;
        Ok(Self { session })
    }
```

- [ ] **Step 3: Update the one remaining call site (still serial at this point)**

In `crates/videre-ml/src/pipeline.rs`, `run_face_pipeline` currently has:

```rust
    let mut detector = face_detect::FaceDetector::new(&det_path)?;
    let mut embedder = face_embed::FaceEmbedder::new(&rec_path)?;
```

Change to (this function is still single-threaded until Task 6 - pass a
generous thread count for now, matching today's "use all cores" behavior for
a single session; Task 6 changes this call site again once there are multiple
workers):

```rust
    let intra_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let mut detector = face_detect::FaceDetector::new(&det_path, intra_threads)?;
    let mut embedder = face_embed::FaceEmbedder::new(&rec_path, intra_threads)?;
```

- [ ] **Step 4: Build and run the full test suite**

```bash
cargo build --workspace
cargo test --workspace
```

Expected: clean build, all tests pass (this is a signature change with no
behavior change for the single-worker case - `intra_threads` equals what ORT
was already defaulting to).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-ml/src/face_models.rs crates/videre-ml/src/face_detect.rs crates/videre-ml/src/face_embed.rs crates/videre-ml/src/pipeline.rs
git commit -m "feat(faces): cap ONNX session intra-op threads explicitly"
```

---

### Task 6: Restructure `run_face_pipeline` into a multi-worker pipeline

**Files:**
- Modify: `crates/videre-ml/src/pipeline.rs`
- Modify: `crates/videre/src/commands/faces.rs`
- Modify: `crates/videre/src/commands/watch.rs`

This is the main integration task. It reuses `ProfileStats` (Task 1),
`round_robin_partition` (Task 3), `WorkerMsg`/`apply_worker_msg_counts` (Task
4), and the intra-thread-capped session constructors (Task 5) - no new pure
logic to TDD here, since the concurrency plumbing itself can only be
meaningfully verified by building and running it (see Task 8), not a unit
test against a fake connection.

- [ ] **Step 1: Replace `run_face_pipeline`'s body**

Replace the entire function body of `run_face_pipeline` in
`crates/videre-ml/src/pipeline.rs` (keep the same public signature from Task
1/2 - `conn`, `to_process`, `batch`, `dry_run`, `silent`, `profile: Option<&mut
ProfileStats>` - and add one new parameter, `workers: usize`, at the end):

```rust
pub fn run_face_pipeline(
    conn: &Connection,
    to_process: &[(String, String)],
    batch: usize,
    dry_run: bool,
    silent: bool,
    mut profile: Option<&mut ProfileStats>,
    workers: usize,
) -> Result<FacesRunResult> {
    use crate::face_models;

    if to_process.is_empty() {
        return Ok(FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 });
    }

    let (det_path, rec_path) = face_models::buffalo_l_paths()?;
    let progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);

    let worker_count = workers.max(1);
    let intra_threads = std::thread::available_parallelism()
        .map(|n| (n.get() / worker_count).max(1))
        .unwrap_or(1);
    let partitions = round_robin_partition(to_process, worker_count);
    let want_profile = profile.is_some();

    let mut result = FacesRunResult { total_faces: 0, write_errors: 0, images_processed: 0, detect_errors: 0 };

    std::thread::scope(|scope| -> Result<()> {
        let (tx, rx) = std::sync::mpsc::channel::<WorkerMsg>();

        // Spawn one worker per partition, keeping each ScopedJoinHandle (not
        // discarding it) so its returned ProfileStats - and any Err from
        // model loading inside the worker - actually reaches the caller.
        // thread::scope only guarantees threads are joined before the scope
        // returns; it does not automatically surface their return values.
        let handles: Vec<std::thread::ScopedJoinHandle<Result<ProfileStats>>> = partitions
            .iter()
            .map(|partition| {
                let tx = tx.clone();
                let det_path = det_path.clone();
                let rec_path = rec_path.clone();
                let progress = &progress;
                scope.spawn(move || -> Result<ProfileStats> {
                    let mut local_profile = ProfileStats::default();
                    let mut detector = face_detect::FaceDetector::new(&det_path, intra_threads)?;
                    let mut embedder = face_embed::FaceEmbedder::new(&rec_path, intra_threads)?;

                    for chunk in partition.chunks(batch) {
                        struct ChunkEntry {
                            hash: String,
                            detections: Vec<face_detect::Detection>,
                            n_crops: usize,
                        }
                        let mut chunk_entries: Vec<ChunkEntry> = Vec::new();
                        let mut chunk_crops: Vec<image::RgbImage> = Vec::new();

                        for (path, hash) in chunk {
                            let is_heic = path.to_lowercase().ends_with(".heic");
                            let load_start = std::time::Instant::now();
                            let img = match load_image(path) {
                                Ok(i) => i,
                                Err(msg) => {
                                    progress.println(&format!("skipping {path}: {msg}"));
                                    let _ = tx.send(WorkerMsg::ImageError);
                                    progress.tick();
                                    continue;
                                }
                            };
                            if want_profile {
                                let d = load_start.elapsed();
                                if is_heic { local_profile.load_heic += d; local_profile.count_heic += 1; }
                                else { local_profile.load_other += d; local_profile.count_other += 1; }
                            }

                            let detect_start = std::time::Instant::now();
                            let detections = match detector.detect(&img) {
                                Ok(d) => d,
                                Err(e) => {
                                    progress.println(&format!("detect failed {path}: {e}"));
                                    let _ = tx.send(WorkerMsg::ImageError);
                                    progress.tick();
                                    continue;
                                }
                            };
                            if want_profile { local_profile.detect += detect_start.elapsed(); }

                            if detections.is_empty() {
                                let _ = tx.send(WorkerMsg::NoFace { hash: hash.clone() });
                                progress.tick();
                                continue;
                            }

                            let align_start = std::time::Instant::now();
                            let crops: Vec<image::RgbImage> = detections.iter()
                                .map(|d| face_align::align_face(&img, &d.landmarks))
                                .collect();
                            if want_profile { local_profile.align += align_start.elapsed(); }

                            let n_crops = crops.len();
                            chunk_crops.extend(crops);
                            chunk_entries.push(ChunkEntry { hash: hash.clone(), detections, n_crops });
                            progress.tick();
                        }

                        if chunk_crops.is_empty() { continue; }

                        let embed_start = std::time::Instant::now();
                        let all_embeddings = match embedder.embed_batch(&chunk_crops) {
                            Ok(e) => e,
                            Err(e) => {
                                progress.println(&format!("embed_batch failed: {e}"));
                                let _ = tx.send(WorkerMsg::EmbedBatchError { n: chunk_entries.len() });
                                continue;
                            }
                        };
                        if want_profile { local_profile.embed += embed_start.elapsed(); }

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
                                    .flat_map(|&v| half::f16::from_f32(v).to_le_bytes())
                                    .collect();
                                videre_core::face_db::FaceRow {
                                    hash: entry.hash.clone(), bbox, landmark: Some(lm_str),
                                    embedding, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0,
                                }
                            }).collect();
                            let _ = tx.send(WorkerMsg::Faces { hash: entry.hash.clone(), rows });
                        }
                    }
                    Ok(local_profile)
                })
            })
            .collect();
        drop(tx); // coordinator's own handle - workers hold the rest, channel closes once all clones drop

        for msg in rx {
            apply_worker_msg_counts(&mut result, &msg);
            match msg {
                WorkerMsg::Faces { hash, rows } => {
                    if !dry_run {
                        let write_start = std::time::Instant::now();
                        let write_result = videre_core::face_db::replace_faces_for_hash(conn, &hash, &rows);
                        if let Some(p) = profile.as_deref_mut() { p.db_write += write_start.elapsed(); }
                        match write_result {
                            Ok(()) => { let _ = videre_core::face_db::mark_scanned(conn, &hash); }
                            Err(e) => {
                                progress.println(&format!("write failed {hash}: {e}"));
                                result.write_errors += 1;
                            }
                        }
                    }
                }
                WorkerMsg::NoFace { hash } => {
                    if !dry_run { let _ = videre_core::face_db::mark_scanned(conn, &hash); }
                }
                WorkerMsg::ImageError | WorkerMsg::EmbedBatchError { .. } => {}
            }
        }

        // Join every worker, propagating both a thread panic and the
        // worker's own Result<ProfileStats> error, and merge each worker's
        // timing into the caller's accumulator (if profiling was requested).
        for handle in handles {
            let worker_profile = handle
                .join()
                .map_err(|_| anyhow::anyhow!("face detection worker thread panicked"))??;
            if let Some(p) = profile.as_deref_mut() {
                p.merge(worker_profile);
            }
        }
        Ok(())
    })?;

    progress.finish();
    Ok(result)
}
```

- [ ] **Step 2: Update the two remaining call sites for the new `workers` parameter**

In `crates/videre/src/commands/faces.rs`, the Task 2 call site becomes (still
hardcoding a worker count here - Task 7 adds the `--workers` flag):

```rust
    let mut profile_stats = videre_ml::pipeline::ProfileStats::default();
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let result = run_face_pipeline(
        &conn, &to_process, args.batch, args.dry_run, args.silent,
        if args.profile { Some(&mut profile_stats) } else { None },
        workers,
    )?;
```

In `crates/videre/src/commands/watch.rs`, the Task 1 Step 8 call site becomes:

```rust
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let result = run_face_pipeline(conn, &to_process, 8, false, args.silent, None, workers)?;
```

- [ ] **Step 3: Update `run_face_pipeline_on_empty_input_is_a_noop`**

Change the call in the test module to pass a `workers` argument:

```rust
        let result = run_face_pipeline(&conn, &[], 8, false, true, None, 4).unwrap();
```

- [ ] **Step 4: Build and run the full test suite**

```bash
cargo build --workspace
cargo test --workspace
```

Expected: clean build, all existing tests pass. This does not yet exercise
real multi-worker detection (no test fixture has real model weights/real
faces) - Task 8 does that via a real run.

- [ ] **Step 5: Commit**

```bash
git add crates/videre-ml/src/pipeline.rs crates/videre/src/commands/faces.rs crates/videre/src/commands/watch.rs
git commit -m "feat(faces): restructure run_face_pipeline into a multi-worker pipeline"
```

---

### Task 7: `--workers` CLI flag

**Files:**
- Modify: `crates/videre/src/commands/faces.rs`

- [ ] **Step 1: Add the flag**

In `FacesArgs`:

```rust
    /// Number of worker threads for face detection/embedding (each with its
    /// own ONNX sessions, intra-op-thread-capped so they don't collectively
    /// oversubscribe the machine). Defaults to available core count. See
    /// docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md.
    #[arg(long)] workers: Option<usize>,
```

- [ ] **Step 2: Use it instead of the Task 6 hardcoded default**

Replace:

```rust
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
```

with:

```rust
    let workers = args.workers.unwrap_or_else(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
    });
```

**Use Task 2's `--profile` numbers to decide whether `available_parallelism()`
alone is the right default**, per the spec's note that a plain "cores" default
may undercount how many workers are useful if load (subprocess-bound, not
CPU-bound) dominates - i.e. if Task 2's data shows load time is large relative
to detect/embed, consider defaulting to something like `available_parallelism()
* 1.5` (rounded) instead of a flat 1:1 mapping, since many workers can be
blocked on `qlmanage` subprocesses simultaneously without consuming CPU. Pick
a concrete default based on what Task 2 actually measured - don't leave this
as a TODO; if Task 2's numbers are inconclusive, `available_parallelism()`
unscaled is a reasonable, defensible starting point and can be revisited in
Task 8's measurement.

- [ ] **Step 3: Build**

```bash
cargo build --workspace
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/videre/src/commands/faces.rs
git commit -m "feat(faces): add --workers flag"
```

---

### Task 8: Regression check + empirical validation (required - see memory)

**Files:** none (verification only, plus memory updates)

Memory `architecture-faces-pipeline-parallelization.md` explicitly requires
this step before treating approach A as validated - do not skip it even
under time pressure.

- [ ] **Step 1: Full test suite one more time**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 2: Regression parity check on a small real sample**

Pick a small, previously-unprocessed sample from the real database (e.g. via
`--limit 50 --dry-run` so nothing is written) and run it twice: once with
`--workers 1` (should behave identically to the old serial pipeline, just
routed through the single-worker case of the new code path), once with the
default worker count. Compare `total_faces`/`images_processed`/`detect_errors`
between the two runs - **they must match** (parallelizing must not change
which faces are found, only how fast). If they don't match, that's a Task 6
bug to fix before proceeding, not an acceptable discrepancy.

```bash
./target/release/videre faces --workers 1 --limit 50 --dry-run
./target/release/videre faces --limit 50 --dry-run   # default workers, different 50 images unless the first run's --limit 50 already marked them scanned under --dry-run (it doesn't write faces_scanned under --dry-run, so the same 50 should be selected again - confirm this is actually true by checking the reported image count matches between runs)
```

- [ ] **Step 3: Measure real speedup**

Run a larger, identical-sized sample two ways and compare wall-clock time -
this is the actual empirical validation the design's estimate (4-5x) needs to
be checked against, and what the linked memory says must happen before
trusting that number:

```bash
time ./target/release/videre faces --workers 1 --limit 300 --profile --silent
# then, against a DIFFERENT unprocessed 300 (the first run already marked these scanned unless --dry-run was used - use --dry-run for both so the exact same 300 images are reprocessed both times):
time ./target/release/videre faces --workers 1 --limit 300 --profile --dry-run --silent
time ./target/release/videre faces --limit 300 --profile --dry-run --silent   # default workers
```

**Report the actual measured speedup** (real elapsed times, not an estimate).
If it's well below the ~4-5x estimate, or profiling data suggests approach B
would meaningfully outperform A, that's the trigger to revisit approach B per
the linked memory - not a reason to have built B speculatively before this
measurement existed.

- [ ] **Step 4: Update memory with the real findings**

Edit `architecture_faces_pipeline_parallelization.md` (in the memory
directory) to add a dated update recording: the actual measured speedup, the
regression-parity check's result, and whether approach A is now considered
validated or whether approach B should be revisited. This step is not
optional - the memory currently says this validation is outstanding, and it
needs to be updated with real findings, not left stale once the work is done.

- [ ] **Step 5: No commit for this task**

This task is measurement and memory-update only, not a code change (aside from
whatever the memory file's own version control, if any, requires - the
project's memory files are outside the `videre` git repo, so there is no
`git commit` step here for the plan's own repo).

---

### Task 9: Update CLAUDE.md docs

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add the two new flags to the `videre faces` section**

Find the `videre faces` flag list in `CLAUDE.md` (the block listing `--reprocess`,
`--recluster`, `--dry-run`, `--batch`, etc.) and add:

```
videre faces --workers <n>              # worker threads for detection/embedding (default: available core count)
videre faces --profile                  # print per-stage timing (load/detect/align/embed/db_write) after the run
```

Add a short paragraph after the existing "Detection is resumable" paragraph
explaining the concurrency model at a level a future reader (or a future
session with compacted context) needs, without repeating the full spec:

```
`videre faces` runs `--workers` worker threads concurrently (default: the
machine's available core count), each with its own ONNX sessions
(intra-op-thread-capped so they don't collectively oversubscribe the
machine) processing a round-robin-assigned slice of the work - not
contiguous chunks, so one worker doesn't inherit a disproportionately
HEIC-heavy (slower) subset. All database writes happen on a single
coordinator thread that receives results from workers over a channel;
workers never touch the connection directly. See
docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md
for the full design and why this approach (a symmetric worker pool) was
chosen over a producer/consumer split with separate loader/inference pools.

One behavior change from the single-threaded version: interrupting (Ctrl-C)
used to lose at most one in-flight image's progress (mark_scanned for
everything before it had already happened). With multiple workers, up to
`--workers` images can be in flight simultaneously, so an interrupt can now
lose up to that many images' progress instead of one - still small and
bounded, and it's the direct tradeoff of processing that many images
concurrently instead of one at a time.
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document --workers/--profile flags and the concurrency model"
```
