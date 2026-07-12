# `dupe-watch` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new `dupe-watch` binary that periodically re-scans a directory and incrementally runs the scan/faces/HEIC-cache/location pipeline stages in the background, so `dupe-report --show-faces` never pays a first-access conversion/lookup cost.

**Architecture:** Two refactors first (expose `dupe`'s scan pipeline as a library, extract `dupe-faces`'s incremental detect/embed/cluster loop into a callable `dupe-ml` function), then two shared `dupe-core` additions (a WAL-mode connection helper, a shared HEIC-via-QuickLook + thumbnail-cache-path module), then the new `dupe-watch` binary itself wires these together behind `--scan`/`--faces`/`--heic`/`--location` flags on an interval loop. Finally `dupe-report`'s `/api/raw` checks the same thumbnail cache before falling back to its existing live-conversion path.

**Tech Stack:** Rust workspace (`dupe`, `dupe-core`, `dupe-ml`), `rusqlite`, `clap`, existing `qlmanage`/`reverse_geocoder`/ONNX (`ort`) infrastructure - no new external dependencies.

**Reference spec:** `docs/superpowers/specs/2026-07-13-dupe-watch-design.md`

---

### Task 1: Expose `dupe`'s scan pipeline as a library

**Files:**
- Create: `crates/dupe/src/lib.rs`
- Modify: `crates/dupe/src/main.rs`
- Modify: `crates/dupe/Cargo.toml`

`crates/dupe/src/{hasher,scanner,output,sqlite_output,types}.rs` are currently declared via `mod` only inside `main.rs`, so they're private to the `dupe` binary target - `dupe-watch` (a different bin target) can't call `scanner::scan()`/`hasher::hash_file()`/`sqlite_output::write_records()` without this.

- [ ] **Step 1: Add a `[lib]` target to `Cargo.toml`**

Edit `crates/dupe/Cargo.toml`, adding before the `[[bin]]` sections:

```toml
[lib]
name = "dupe"
path = "src/lib.rs"
```

- [ ] **Step 2: Create `src/lib.rs`**

```rust
pub mod hasher;
pub mod output;
pub mod scanner;
pub mod sqlite_output;
pub mod types;
```

- [ ] **Step 3: Update `main.rs` to use the lib instead of `mod` declarations**

Edit `crates/dupe/src/main.rs`, replacing the top of the file:

```rust
use dupe::{hasher, output, scanner, sqlite_output, types};
use clap::Parser;
use rayon::prelude::*;
use std::path::PathBuf;
use std::process;
```

(Remove the five `mod hasher;` / `mod output;` / `mod scanner;` / `mod sqlite_output;` / `mod types;` lines - Cargo automatically links a package's own `[lib]` target into its `[[bin]]` targets, no new `Cargo.toml` dependency line is needed for this.)

- [ ] **Step 4: Run the existing test suite to confirm no regressions**

Run: `cargo test -p dupe`
Expected: PASS - unchanged behavior, this task only changes module visibility

- [ ] **Step 5: Commit**

```bash
git add crates/dupe/Cargo.toml crates/dupe/src/lib.rs crates/dupe/src/main.rs
git commit -m "refactor: expose dupe's scan pipeline as a library for dupe-watch to reuse"
```

---

### Task 2: `dupe-core` - WAL-mode connection helper

**Files:**
- Create: `crates/dupe-core/src/db.rs`
- Modify: `crates/dupe-core/src/lib.rs`
- Modify: every `Connection::open(...)` call site (listed in Step 3)

- [ ] **Step 1: Write the failing test**

Create `crates/dupe-core/src/db.rs`:

```rust
use rusqlite::Connection;
use std::path::Path;

/// Opens a SQLite connection and switches it to WAL journal mode - allows
/// one writer plus many concurrent readers without "database is locked"
/// errors, which matters once dupe-watch (writing in the background) and a
/// running dupe-report --show-faces server (reading/writing) hold separate
/// connections to the same file at the same time. WAL mode persists in the
/// database file itself once set, so this is idempotent - safe to call on
/// every connection open, not just the first.
pub fn open_wal(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_wal_sets_journal_mode() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = open_wal(&db_path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn open_wal_is_idempotent_across_repeated_opens() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        open_wal(&db_path).unwrap();
        // Second open on the same file must not error - WAL mode already
        // persisted from the first open.
        let conn = open_wal(&db_path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }
}
```

Add `tempfile = "3"` to `crates/dupe-core/Cargo.toml`'s `[dev-dependencies]` (create that section if it doesn't exist).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe-core db::`
Expected: FAIL - module not wired into `lib.rs` yet

- [ ] **Step 3: Wire the module in and update every call site**

Edit `crates/dupe-core/src/lib.rs`, adding `pub mod db;`.

Then replace `Connection::open(...)` with `dupe_core::db::open_wal(...)` at each of these locations (all currently plain `rusqlite::Connection::open`):

- `crates/dupe/src/sqlite_output.rs:6` - `let conn = Connection::open(db_path)?;` → `let conn = dupe_core::db::open_wal(db_path)?;` (add `dupe-core` as a dependency of `crates/dupe` if not already present - check `Cargo.toml` first, it likely already is via `dupe_report.rs`'s existing use)
- `crates/dupe/src/bin/dupe_report.rs` (three non-test sites: the static-mode `main()` open, the `serve_faces_async` open, and any other production open - search for `Connection::open(` and replace each, leaving the `Connection::open_in_memory()` test helper alone since in-memory databases don't need WAL)
- `crates/dupe/src/bin/dupe_fix_dates.rs`
- `crates/dupe/src/bin/dupe_prune.rs`
- `crates/dupe-ml/src/bin/dupe-faces.rs`
- `crates/dupe-ml/src/bin/dupe-embed.rs`
- `crates/dupe-ml/src/bin/dupe-search.rs`

Each site needs `use dupe_core::db;` (or `dupe_core::db::open_wal`) added to its imports; `dupe-core` is already a workspace dependency of both `crates/dupe` and `crates/dupe-ml`, so no `Cargo.toml` changes are needed beyond Step 1's `tempfile` addition.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p dupe-core db::`
Expected: PASS (2 tests)

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS, no regressions - WAL mode doesn't change query semantics, only locking behavior

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: switch all SQLite connections to WAL mode for multi-process safety"
```

---

### Task 3: `dupe-core` - shared HEIC-via-QuickLook module

**Files:**
- Create: `crates/dupe-core/src/heic.rs`
- Modify: `crates/dupe-core/src/lib.rs`
- Modify: `crates/dupe-core/Cargo.toml`
- Modify: `crates/dupe/src/bin/dupe_report.rs`
- Modify: `crates/dupe-ml/src/bin/dupe-faces.rs`

Today there are **two separate, near-identical copies** of HEIC-via-QuickLook conversion: `dupe_report.rs`'s `heic_via_quicklook(path, tag)` and `dupe-faces.rs`'s `heic_via_quicklook(path)` (macOS-only, no `tag` parameter). `dupe-watch`'s `--heic` stage needs this same conversion - rather than adding a *third* copy, this consolidates both into one `dupe-core` function.

- [ ] **Step 1: Add `image` as a `dupe-core` dependency**

Edit `crates/dupe-core/Cargo.toml`, adding to `[dependencies]`:

```toml
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "gif", "webp", "bmp", "tiff"] }
```

- [ ] **Step 2: Create `crates/dupe-core/src/heic.rs`**

```rust
use image::DynamicImage;

/// Convert a HEIC file to a `DynamicImage` via QuickLook (`qlmanage -t`).
///
/// `sips -s format jpeg` copies the raw sensor-buffer pixels unrotated for
/// HEIC files where the camera encoded rotation via the HEIF `irot`
/// transform box rather than a classic EXIF Orientation tag - the same
/// rotation Finder/Preview/Photos apply via QuickLook. Using `sips` would
/// produce sideways images (or, for dupe-faces, detect faces and compute
/// bounding boxes against the wrongly oriented image).
///
/// `tag` disambiguates concurrent/repeated conversions of the same path for
/// different purposes (e.g. a 240px thumbnail vs a 1200px lightbox version)
/// so their temp-directory names don't collide.
pub fn heic_via_quicklook(path: &str, tag: &str) -> Option<DynamicImage> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    tag.hash(&mut hasher);
    let out_dir = std::env::temp_dir().join(format!("dupe_ql_{:016x}", hasher.finish()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).ok()?;
    let ok = std::process::Command::new("qlmanage")
        .args(["-t", "-s", "10000", "-o"])
        .arg(&out_dir)
        .arg(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let file_name = std::path::Path::new(path).file_name()?.to_str()?;
    let out_file = out_dir.join(format!("{file_name}.png"));
    let result = if ok { image::open(&out_file).ok() } else { None };
    let _ = std::fs::remove_dir_all(&out_dir);
    result
}
```

(This is `dupe_report.rs`'s existing implementation verbatim, moved - it already has the more general `tag` parameter `dupe-faces.rs`'s copy lacks.)

- [ ] **Step 3: Wire the module in**

Edit `crates/dupe-core/src/lib.rs`, adding `pub mod heic;`.

- [ ] **Step 4: Update `dupe_report.rs` to use the shared function**

Delete the private `heic_via_quicklook` function from `crates/dupe/src/bin/dupe_report.rs` (currently ~line 2216) and replace its call sites with `dupe_core::heic::heic_via_quicklook(...)`.

- [ ] **Step 5: Update `dupe-faces.rs` to use the shared function**

Delete the private `#[cfg(target_os = "macos")] fn heic_via_quicklook(path: &str) -> Option<DynamicImage>` from `crates/dupe-ml/src/bin/dupe-faces.rs` (lines 209-230). Update `load_image` (lines 189-199) to call `dupe_core::heic::heic_via_quicklook(path, "faces")` instead - note the shared version isn't `#[cfg(target_os = "macos")]`-gated internally (it just returns `None` if `qlmanage` isn't found/fails, which happens naturally on non-macOS), so `load_image`'s own `#[cfg(target_os = "macos")]` split can be simplified to always call the shared function and let it fail gracefully - but **keep the existing behavior exactly** (return `None` immediately on non-macOS without attempting to spawn a nonexistent `qlmanage`) by leaving the `#[cfg(target_os = "macos")]`/`#[cfg(not(target_os = "macos"))]` split in `load_image` as-is, just changing what the macOS branch calls.

- [ ] **Step 6: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS, no regressions

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: consolidate the two duplicate heic_via_quicklook implementations into dupe-core"
```

---

### Task 4: `dupe-core` - thumbnail cache path helpers

**Files:**
- Create: `crates/dupe-core/src/thumb_cache.rs`
- Modify: `crates/dupe-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

```rust
use std::path::PathBuf;

/// Directory holding pre-converted HEIC thumbnails, keyed by content hash
/// rather than file path - the same photo scanned into different databases
/// only needs converting once. Mirrors this project's existing
/// `~/.cache/ort/` convention for cached model weights.
pub fn cache_dir() -> PathBuf {
    dirs_cache_dir().join("dupe").join("thumbnails")
}

/// Path to a cached thumbnail for `hash` at `size` pixels (e.g. 240 or
/// 1200), whether or not it currently exists on disk.
pub fn thumb_path(hash: &str, size: u32) -> PathBuf {
    cache_dir().join(format!("{hash}_{size}.jpg"))
}

/// True if a cached thumbnail already exists for this hash/size.
pub fn thumb_exists(hash: &str, size: u32) -> bool {
    thumb_path(hash, size).exists()
}

fn dirs_cache_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".cache"))
        .unwrap_or_else(|| PathBuf::from(".cache"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_path_is_keyed_by_hash_and_size() {
        let p1 = thumb_path("abc123", 240);
        let p2 = thumb_path("abc123", 1200);
        let p3 = thumb_path("def456", 240);
        assert_ne!(p1, p2, "different sizes must produce different paths");
        assert_ne!(p1, p3, "different hashes must produce different paths");
        assert!(p1.to_string_lossy().contains("abc123_240.jpg"));
    }

    #[test]
    fn thumb_exists_false_for_missing_file() {
        assert!(!thumb_exists("nonexistent-hash-xyz", 240));
    }
}
```

Save as `crates/dupe-core/src/thumb_cache.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe-core thumb_cache::`
Expected: FAIL - module not wired into `lib.rs` yet

- [ ] **Step 3: Wire the module in**

Edit `crates/dupe-core/src/lib.rs`, adding `pub mod thumb_cache;`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p dupe-core thumb_cache::`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-core/src/thumb_cache.rs crates/dupe-core/src/lib.rs
git commit -m "feat: add thumbnail cache path helpers to dupe-core"
```

---

### Task 5: `dupe-ml` - extract the incremental face-detection pipeline into a callable function

**Files:**
- Create: `crates/dupe-ml/src/pipeline.rs`
- Modify: `crates/dupe-ml/src/lib.rs`
- Modify: `crates/dupe-ml/src/bin/dupe-faces.rs`

`dupe-faces.rs`'s `main()` (lines 26-187 today) inlines the entire "which hashes need processing → detect → embed → write → cluster" pipeline. `dupe-watch`'s `--faces` stage needs the same detect/embed/write/cluster logic, minus the CLI-specific `--reprocess`/`--recluster` branching (which only makes sense as an explicit one-shot CLI choice, not something a background loop does automatically).

- [ ] **Step 1: Write the failing test**

Create `crates/dupe-ml/src/pipeline.rs`:

```rust
use anyhow::Result;
use rusqlite::Connection;

pub struct FacesRunResult {
    pub total_faces: usize,
    pub write_errors: usize,
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
    todo!("moved from dupe-faces.rs main() in Step 3")
}

/// Re-runs DBSCAN clustering over every face embedding currently in the
/// database - safe to call whether or not run_face_pipeline found anything
/// new, since re-clustering is idempotent.
pub fn run_clustering(
    conn: &Connection,
    eps: f32,
    min_cluster_size: usize,
    silent: bool,
) -> Result<()> {
    todo!("moved from dupe-faces.rs main() in Step 3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dupe_core::face_db;

    #[test]
    fn run_face_pipeline_on_empty_input_is_a_noop() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        let result = run_face_pipeline(&conn, &[], 8, false, true).unwrap();
        assert_eq!(result.total_faces, 0);
        assert_eq!(result.write_errors, 0);
    }

    #[test]
    fn run_clustering_on_empty_db_does_not_error() {
        let conn = Connection::open_in_memory().unwrap();
        face_db::create_faces_table(&conn).unwrap();
        run_clustering(&conn, 0.6, 3, true).unwrap();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe-ml pipeline::`
Expected: FAIL - `todo!()` panics

- [ ] **Step 3: Move the real implementation in from `dupe-faces.rs`**

Replace the two `todo!()` bodies with the actual logic currently in `dupe-faces.rs`'s `main()`:

`run_face_pipeline` gets the chunk-processing loop currently at lines 78-166 (from `if !args.silent { eprintln!("Processing...") }` through the end of the `for chunk in to_process.chunks(...)` loop), adapted to use the function's parameters instead of `args.*`:

```rust
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
        return Ok(FacesRunResult { total_faces: 0, write_errors: 0 });
    }

    if !silent { eprintln!("Processing {} images...", to_process.len()); }

    let (det_path, rec_path) = face_models::buffalo_l_paths()?;
    let mut detector = face_detect::FaceDetector::new(&det_path)?;
    let mut embedder = face_embed::FaceEmbedder::new(&rec_path)?;

    let mut total_faces = 0usize;
    let mut write_errors = 0usize;

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
            let img = match load_image(path) {
                Some(i) => i,
                None => continue,
            };
            let detections = match detector.detect(&img) {
                Ok(d) => d,
                Err(e) => { eprintln!("detect failed {path}: {e}"); continue; }
            };
            if detections.is_empty() { continue; }

            let crops: Vec<image::RgbImage> = detections.iter()
                .map(|d| face_align::align_face(&img, &d.landmarks))
                .collect();

            if !silent { eprintln!("[faces] {path}: {} face(s)", detections.len()); }
            let n_crops = crops.len();
            chunk_crops.extend(crops);
            chunk_entries.push(ChunkEntry { path: path.clone(), hash: hash.clone(), detections, n_crops });
        }

        if chunk_crops.is_empty() { continue; }

        let all_embeddings = match embedder.embed_batch(&chunk_crops) {
            Ok(e) => e,
            Err(e) => { eprintln!("embed_batch failed: {e}"); continue; }
        };

        let mut emb_offset = 0;
        for entry in &chunk_entries {
            let n = entry.n_crops;
            let embs = &all_embeddings[emb_offset..emb_offset + n];
            emb_offset += n;

            let rows: Vec<dupe_core::face_db::FaceRow> = entry.detections.iter().zip(embs.iter()).map(|(det, emb)| {
                let [x1, y1, x2, y2] = det.bbox;
                let bbox = format!("{},{},{},{}", x1 as i32, y1 as i32, (x2 - x1) as i32, (y2 - y1) as i32);
                let lm_str: String = det.landmarks.iter()
                    .flat_map(|[x, y]| [x.to_string(), y.to_string()])
                    .collect::<Vec<_>>().join(",");
                let embedding: Vec<u8> = emb.iter()
                    .flat_map(|&v| f16::from_f32(v).to_le_bytes())
                    .collect();
                dupe_core::face_db::FaceRow {
                    hash: entry.hash.clone(), bbox, landmark: Some(lm_str),
                    embedding, cluster_id: None, person_label: None, confirmed: 0, is_primary: 0,
                }
            }).collect();

            total_faces += rows.len();
            if !dry_run {
                if let Err(e) = dupe_core::face_db::replace_faces_for_hash(conn, &entry.hash, &rows) {
                    eprintln!("write failed {}: {e}", entry.path);
                    write_errors += 1;
                }
            }
        }
    }

    Ok(FacesRunResult { total_faces, write_errors })
}
```

`run_clustering` gets the clustering block currently at lines 169-180:

```rust
pub fn run_clustering(conn: &Connection, eps: f32, min_cluster_size: usize, silent: bool) -> Result<()> {
    let all_embs = dupe_core::face_db::load_face_embeddings(conn)?;
    if all_embs.is_empty() {
        return Ok(());
    }
    let assignments = dupe_core::face_cluster::dbscan_cosine(&all_embs, eps, min_cluster_size);
    dupe_core::face_db::update_cluster_assignments(conn, &assignments)?;
    if !silent {
        let clustered = assignments.iter().filter(|(_, c)| c.is_some()).count();
        eprintln!("Clustering complete: {}/{} faces assigned to clusters (eps={:.2}).", clustered, all_embs.len(), eps);
    }
    Ok(())
}
```

Also move `load_image` (currently private to `dupe-faces.rs`, lines 189-199) into `pipeline.rs` as a `pub(crate)` or `pub` function, updated to call `dupe_core::heic::heic_via_quicklook(path, "faces")` per Task 3:

```rust
fn load_image(path: &str) -> Option<image::DynamicImage> {
    if path.to_lowercase().ends_with(".heic") {
        return dupe_core::heic::heic_via_quicklook(path, "faces");
    }
    image::open(path).ok()
}
```

- [ ] **Step 4: Wire the module into `lib.rs`**

Edit `crates/dupe-ml/src/lib.rs`, adding `pub mod pipeline;`.

- [ ] **Step 5: Run the pipeline tests**

Run: `cargo test -p dupe-ml pipeline::`
Expected: PASS (2 tests)

- [ ] **Step 6: Update `dupe-faces.rs`'s `main()` to call the extracted functions**

Replace the inlined loop (lines 78-180) with:

```rust
use dupe_ml::pipeline::{run_clustering, run_face_pipeline};

// ... (keep the existing to_process/skip_hashes/reprocess/recluster
// computation exactly as-is, lines 34-76) ...

if args.recluster || to_process.is_empty() {
    if !args.silent && to_process.is_empty() && !args.recluster {
        eprintln!("All hashes already processed.");
    }
    if !args.dry_run {
        run_clustering(&conn, args.eps, args.min_cluster_size, args.silent)?;
    }
    return Ok(());
}

let result = run_face_pipeline(&conn, &to_process, args.batch, args.dry_run, args.silent)?;

if !args.dry_run {
    run_clustering(&conn, args.eps, args.min_cluster_size, args.silent)?;
}

if !args.silent { eprintln!("Done: {} new face(s) detected.", result.total_faces); }
if result.write_errors > 0 {
    std::process::exit(1);
}
Ok(())
```

Delete the now-unused private `load_image`/`heic_via_quicklook` functions from `dupe-faces.rs` (moved to `pipeline.rs`/`dupe-core` respectively).

- [ ] **Step 7: Run the existing dupe-faces integration tests**

Run: `cargo test -p dupe-ml --test faces_pipeline`
Expected: PASS, no behavior change from the user's perspective

- [ ] **Step 8: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor: extract dupe-faces incremental detect/embed/cluster pipeline into dupe-ml for dupe-watch to reuse"
```

---

### Task 6: New `dupe-watch` binary - skeleton, CLI, and interval loop

**Files:**
- Modify: `crates/dupe-ml/Cargo.toml`
- Create: `crates/dupe-ml/src/bin/dupe-watch.rs`

`dupe-watch` needs both `dupe`'s scan pipeline (Task 1) and `dupe-ml`'s face pipeline (Task 5), so it lives in `crates/dupe-ml` (which already carries the heavier ML dependencies) with a new dependency on `dupe`.

- [ ] **Step 1: Add the new bin target and dependency**

Edit `crates/dupe-ml/Cargo.toml`, adding a `[[bin]]` section:

```toml
[[bin]]
name = "dupe-watch"
path = "src/bin/dupe-watch.rs"
```

And adding to `[dependencies]`:

```toml
dupe = { path = "../dupe" }
```

- [ ] **Step 2: Write the CLI skeleton**

Create `crates/dupe-ml/src/bin/dupe-watch.rs`:

```rust
use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "dupe-watch", about = "Periodically populate the scan/faces/HEIC-cache/location pipeline in the background")]
struct Args {
    /// Directory to scan recursively
    directory: PathBuf,

    /// SQLite database to populate (same file dupe-report reads)
    #[arg(long)]
    output_sqlite: PathBuf,

    /// Re-run the scan/hash/EXIF pipeline each cycle
    #[arg(long)]
    scan: bool,
    /// Run incremental face detection each cycle
    #[arg(long)]
    faces: bool,
    /// Pre-convert and cache HEIC thumbnails each cycle
    #[arg(long)]
    heic: bool,
    /// Pre-resolve reverse-geocoded location names each cycle
    #[arg(long)]
    location: bool,

    /// Seconds between cycles
    #[arg(long, default_value = "300")]
    interval: u64,

    #[arg(long)]
    silent: bool,
}

fn main() -> Result<()> {
    let mut args = Args::parse();
    if !args.directory.exists() {
        anyhow::bail!("{:?} does not exist", args.directory);
    }
    // If no stage flags were passed, run all four - the common case is
    // "just keep everything up to date", not memorizing four flags.
    if !(args.scan || args.faces || args.heic || args.location) {
        args.scan = true;
        args.faces = true;
        args.heic = true;
        args.location = true;
    }

    loop {
        if !args.silent {
            eprintln!("dupe-watch: cycle starting ({})", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"));
        }
        if let Err(e) = run_cycle(&args) {
            eprintln!("dupe-watch: cycle error: {e}");
        }
        if !args.silent {
            eprintln!("dupe-watch: sleeping {}s", args.interval);
        }
        std::thread::sleep(Duration::from_secs(args.interval));
    }
}

fn run_cycle(args: &Args) -> Result<()> {
    // Stages implemented in Tasks 7-10.
    Ok(())
}
```

Add `chrono = { version = "0.4", features = ["serde"] }` to `crates/dupe-ml/Cargo.toml`'s `[dependencies]` if not already present (check first - `dupe-core`/`dupe` already use it, but `dupe-ml` may not).

- [ ] **Step 3: Confirm it builds and runs one empty cycle**

Run: `cargo build -p dupe-ml --bin dupe-watch`
Expected: clean build

Run (manually, Ctrl-C after a few seconds): `./target/debug/dupe-watch --help`
Expected: shows the CLI help with all flags listed

- [ ] **Step 4: Commit**

```bash
git add crates/dupe-ml/Cargo.toml crates/dupe-ml/src/bin/dupe-watch.rs
git commit -m "feat: add dupe-watch binary skeleton with CLI args and interval loop"
```

---

### Task 7: `dupe-watch` - `--scan` stage

**Files:**
- Modify: `crates/dupe-ml/src/bin/dupe-watch.rs`

Reuses the exact existing scan+hash+EXIF pipeline from Task 1 - re-scans and re-hashes every file in the directory each cycle. This isn't maximally efficient (it doesn't skip unchanged files by mtime), but `sqlite_output::write_records`'s `INSERT OR REPLACE` already makes repeated full scans safe/idempotent, and BLAKE3 hashing a personal photo collection takes well under a minute (see `CLAUDE.md`'s real-world timing: 8,022 files in 59 seconds) - premature to add mtime-diffing before it's shown to matter.

- [ ] **Step 1: Write the failing test**

Add to a new `crates/dupe-ml/tests/watch.rs`:

```rust
use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn watch_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop();
    path.pop();
    path.push("dupe-watch");
    path
}

#[test]
fn scan_stage_populates_file_hashes() {
    let dir = tempdir().unwrap();
    let pics = dir.path().join("pics");
    std::fs::create_dir(&pics).unwrap();
    std::fs::write(pics.join("a.jpg"), b"dummy-bytes").unwrap();
    let db = dir.path().join("test.db");

    // Run one cycle directly via a very short interval, then kill after
    // giving it time for exactly one cycle.
    let mut child = Command::new(watch_bin())
        .arg(&pics)
        .arg("--output-sqlite").arg(&db)
        .arg("--scan")
        .arg("--interval").arg("3600") // long enough we only observe one cycle
        .arg("--silent")
        .spawn()
        .expect("failed to spawn dupe-watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    child.kill().ok();
    child.wait().ok();

    let conn = Connection::open(&db).unwrap();
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1, "expected the scan stage to have inserted the one file");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe-ml --test watch scan_stage_populates_file_hashes`
Expected: FAIL - `run_cycle` is currently a no-op

- [ ] **Step 3: Implement the scan stage**

Edit `crates/dupe-ml/src/bin/dupe-watch.rs`, updating `run_cycle`:

```rust
use dupe::{hasher, output, scanner, sqlite_output, types};
use rayon::prelude::*;

fn run_cycle(args: &Args) -> Result<()> {
    if args.scan {
        run_scan_stage(args)?;
    }
    Ok(())
}

fn run_scan_stage(args: &Args) -> Result<()> {
    let paths = scanner::scan(&args.directory);
    let records: Vec<types::FileRecord> = paths
        .par_iter()
        .filter_map(|path| hasher::hash_file(path).ok())
        .collect();
    sqlite_output::write_records(&records, &args.output_sqlite)?;
    if !args.silent {
        eprintln!("dupe-watch: scan stage wrote {} record(s)", records.len());
    }
    Ok(())
}
```

Add `rayon = "1"` to `crates/dupe-ml/Cargo.toml`'s `[dependencies]` if not already present.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p dupe-ml --test watch scan_stage_populates_file_hashes`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-ml/Cargo.toml crates/dupe-ml/src/bin/dupe-watch.rs crates/dupe-ml/tests/watch.rs
git commit -m "feat: implement dupe-watch --scan stage"
```

---

### Task 8: `dupe-watch` - `--faces` stage

**Files:**
- Modify: `crates/dupe-ml/src/bin/dupe-watch.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/dupe-ml/tests/watch.rs` (this test seeds a database directly rather than relying on real face detection, since that requires downloaded ONNX models - it only verifies the incremental-selection logic runs without error on an already-fully-processed DB):

```rust
#[test]
fn faces_stage_skips_hashes_already_processed() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL, bbox TEXT NOT NULL,
             landmark TEXT, embedding BLOB NOT NULL, cluster_id INTEGER, person_label TEXT,
             confirmed INTEGER DEFAULT 0, is_primary INTEGER DEFAULT 0);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/a.jpg', 'h1', 'jpg');
         INSERT INTO faces (hash, bbox, embedding) VALUES ('h1', '0,0,10,10', X'0000');",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(watch_bin())
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--faces")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn dupe-watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().ok();
    child.wait().ok();
    assert!(still_running, "dupe-watch --faces should not have crashed on an already-processed hash");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe-ml --test watch faces_stage_skips_hashes_already_processed`
Expected: FAIL - faces stage not implemented yet (test would currently pass trivially since `run_cycle` no-ops on `--faces`; verify by temporarily checking the binary doesn't yet call any faces logic - this test's real value is exercised once Step 3 lands and it still passes)

- [ ] **Step 3: Implement the faces stage**

Edit `crates/dupe-ml/src/bin/dupe-watch.rs`:

```rust
use dupe_core::{db, face_db};
use dupe_ml::pipeline::{run_clustering, run_face_pipeline};

fn run_cycle(args: &Args) -> Result<()> {
    if args.scan {
        run_scan_stage(args)?;
    }
    if args.faces || args.heic || args.location {
        // These three stages all read file_hashes; open once and reuse.
        let conn = db::open_wal(&args.output_sqlite)?;
        face_db::create_faces_table(&conn)?;
        if args.faces {
            run_faces_stage(args, &conn)?;
        }
        // heic/location stages added in Tasks 9-10
    }
    Ok(())
}

fn run_faces_stage(args: &Args, conn: &rusqlite::Connection) -> Result<()> {
    let all_paths: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT path, hash FROM file_hashes WHERE ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')"
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let skip_hashes: std::collections::HashSet<String> =
        face_db::hashes_with_faces(conn)?.into_iter().collect();
    let mut seen_hashes = std::collections::HashSet::new();
    let to_process: Vec<(String, String)> = all_paths.into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash) && seen_hashes.insert(hash.clone()))
        .collect();

    if !to_process.is_empty() {
        let result = run_face_pipeline(conn, &to_process, 8, false, args.silent)?;
        if !args.silent {
            eprintln!("dupe-watch: faces stage processed {} new hash(es), {} face(s)", to_process.len(), result.total_faces);
        }
    }
    run_clustering(conn, 0.6, 3, args.silent)?;
    Ok(())
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p dupe-ml --test watch faces_stage_skips_hashes_already_processed`
Expected: PASS

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/dupe-ml/src/bin/dupe-watch.rs crates/dupe-ml/tests/watch.rs
git commit -m "feat: implement dupe-watch --faces stage"
```

---

### Task 9: `dupe-watch` - `--heic` stage

**Files:**
- Modify: `crates/dupe-ml/src/bin/dupe-watch.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/dupe-ml/tests/watch.rs`:

```rust
#[test]
fn heic_stage_writes_no_cache_file_for_non_heic_hashes() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT);
         INSERT INTO file_hashes (path, hash, ext) VALUES ('/tmp/a.jpg', 'hjpg', 'jpg');",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(watch_bin())
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--heic")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn dupe-watch");
    std::thread::sleep(std::time::Duration::from_millis(800));
    child.kill().ok();
    child.wait().ok();

    assert!(!dupe_core::thumb_cache::thumb_exists("hjpg", 240), "non-HEIC hash must not get a cached thumbnail");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe-ml --test watch heic_stage_writes_no_cache_file_for_non_heic_hashes`
Expected: This specific assertion actually passes trivially before implementation (no stage runs, so no cache file is ever written) - the real regression-guard is Step 4's full-suite run after implementation. Proceed to Step 3.

- [ ] **Step 3: Implement the heic stage**

Edit `crates/dupe-ml/src/bin/dupe-watch.rs`:

```rust
fn run_cycle(args: &Args) -> Result<()> {
    if args.scan {
        run_scan_stage(args)?;
    }
    if args.faces || args.heic || args.location {
        let conn = db::open_wal(&args.output_sqlite)?;
        face_db::create_faces_table(&conn)?;
        if args.faces {
            run_faces_stage(args, &conn)?;
        }
        if args.heic {
            run_heic_stage(args, &conn)?;
        }
        // location stage added in Task 10
    }
    Ok(())
}

fn run_heic_stage(args: &Args, conn: &rusqlite::Connection) -> Result<()> {
    let heic_paths: Vec<(String, String)> = {
        let mut stmt = conn.prepare("SELECT path, hash FROM file_hashes WHERE ext = 'heic'")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut converted = 0usize;
    let mut seen = std::collections::HashSet::new();
    for (path, hash) in heic_paths {
        if !seen.insert(hash.clone()) { continue; } // one representative path per hash
        if dupe_core::thumb_cache::thumb_exists(&hash, 240) && dupe_core::thumb_cache::thumb_exists(&hash, 1200) {
            continue;
        }
        std::fs::create_dir_all(dupe_core::thumb_cache::cache_dir()).ok();
        for size in [240u32, 1200] {
            if dupe_core::thumb_cache::thumb_exists(&hash, size) { continue; }
            if let Some(img) = dupe_core::heic::heic_via_quicklook(&path, &format!("watch{size}")) {
                let img = if img.width() > size || img.height() > size {
                    img.resize(size, size, image::imageops::FilterType::Triangle)
                } else {
                    img
                };
                if img.save(dupe_core::thumb_cache::thumb_path(&hash, size)).is_ok() {
                    converted += 1;
                }
            }
        }
    }
    if !args.silent && converted > 0 {
        eprintln!("dupe-watch: heic stage cached {converted} thumbnail(s)");
    }
    Ok(())
}
```

Add `image` as a direct dependency of `crates/dupe-ml` if the `image::imageops::FilterType` import needs it explicitly (it's already a transitive/direct dependency per the existing `Cargo.toml` - confirm before adding a duplicate line).

- [ ] **Step 4: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS (this task's heic stage only exercises real `qlmanage` conversion on macOS with real HEIC files - the test above only confirms non-HEIC hashes are correctly skipped, not that real conversion works; real conversion is verified manually in Task 12)

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-ml/src/bin/dupe-watch.rs crates/dupe-ml/tests/watch.rs
git commit -m "feat: implement dupe-watch --heic stage"
```

---

### Task 10: `dupe-watch` - `--location` stage

**Files:**
- Modify: `crates/dupe-ml/src/bin/dupe-watch.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/dupe-ml/tests/watch.rs`:

```rust
#[test]
fn location_stage_populates_location_name_for_gps_rows() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL, ext TEXT,
             gps_lat REAL, gps_lon REAL, location_name TEXT);
         INSERT INTO file_hashes (path, hash, ext, gps_lat, gps_lon)
             VALUES ('/tmp/paris.jpg', 'hparis', 'jpg', 48.8566, 2.3522);",
    ).unwrap();
    drop(conn);

    let mut child = Command::new(watch_bin())
        .arg(dir.path())
        .arg("--output-sqlite").arg(&db)
        .arg("--location")
        .arg("--interval").arg("3600")
        .arg("--silent")
        .spawn()
        .expect("failed to spawn dupe-watch");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().ok();
    child.wait().ok();

    let conn = Connection::open(&db).unwrap();
    let name: Option<String> = conn
        .query_row("SELECT location_name FROM file_hashes WHERE hash = 'hparis'", [], |r| r.get(0))
        .unwrap();
    assert!(name.is_some(), "expected the location stage to have resolved and cached a name");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe-ml --test watch location_stage_populates_location_name_for_gps_rows`
Expected: FAIL - location stage not implemented yet

- [ ] **Step 3: Implement the location stage**

Edit `crates/dupe-ml/src/bin/dupe-watch.rs`:

```rust
fn run_cycle(args: &Args) -> Result<()> {
    if args.scan {
        run_scan_stage(args)?;
    }
    if args.faces || args.heic || args.location {
        let conn = db::open_wal(&args.output_sqlite)?;
        face_db::create_faces_table(&conn)?;
        dupe_core::location::ensure_location_column(&conn);
        if args.faces {
            run_faces_stage(args, &conn)?;
        }
        if args.heic {
            run_heic_stage(args, &conn)?;
        }
        if args.location {
            run_location_stage(args, &conn)?;
        }
    }
    Ok(())
}

fn run_location_stage(args: &Args, conn: &rusqlite::Connection) -> Result<()> {
    let unresolved: Vec<(f64, f64)> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT gps_lat, gps_lon FROM file_hashes \
             WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL AND location_name IS NULL"
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut resolved = 0usize;
    for (lat, lon) in unresolved {
        if let Some(name) = dupe_core::location::location_name(lat, lon) {
            conn.execute(
                "UPDATE file_hashes SET location_name = ?1 WHERE gps_lat = ?2 AND gps_lon = ?3",
                rusqlite::params![name, lat, lon],
            )?;
            resolved += 1;
        }
    }
    if !args.silent && resolved > 0 {
        eprintln!("dupe-watch: location stage resolved {resolved} coordinate(s)");
    }
    Ok(())
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p dupe-ml --test watch location_stage_populates_location_name_for_gps_rows`
Expected: PASS

- [ ] **Step 5: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/dupe-ml/src/bin/dupe-watch.rs crates/dupe-ml/tests/watch.rs
git commit -m "feat: implement dupe-watch --location stage"
```

---

### Task 11: `dupe-report` - check the thumbnail cache in `/api/raw` before live conversion

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/dupe/tests/faces_server.rs` (or a suitable existing test file) - this is a unit-level check on the cache-check logic, not a full server round-trip:

```rust
#[test]
fn thumb_cache_hit_avoids_qlmanage_conversion() {
    // Seed a fake cached thumbnail file directly, then confirm handle_raw_file's
    // cache-check path would find it - since handle_raw_file itself needs a
    // running server + real HEIC file to test end-to-end, this instead verifies
    // the shared dupe_core::thumb_cache helpers dupe-report will call.
    let hash = "test-cache-hit-hash";
    std::fs::create_dir_all(dupe_core::thumb_cache::cache_dir()).unwrap();
    std::fs::write(dupe_core::thumb_cache::thumb_path(hash, 240), b"fake-jpeg-bytes").unwrap();
    assert!(dupe_core::thumb_cache::thumb_exists(hash, 240));
    std::fs::remove_file(dupe_core::thumb_cache::thumb_path(hash, 240)).ok();
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dupe --test faces_server thumb_cache_hit_avoids_qlmanage_conversion`
Expected: FAIL - `dupe_core::thumb_cache` isn't imported/used by `dupe_report.rs` yet (this test only exercises the shared helper directly, so it may actually pass immediately since `dupe-core` is already a dependency; the real regression-guard is Step 4 confirming `handle_raw_file`'s behavior manually in Task 12)

- [ ] **Step 3: Update `handle_raw_file` to check the cache first**

Edit `crates/dupe/src/bin/dupe_report.rs`'s `handle_raw_file` (from the "HEIC lazy conversion" work in the previous session) - add a cache-check before the `heic_via_quicklook` fallback:

```rust
async fn handle_raw_file(
    Query(q): Query<RawFileQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let (path, hash) = {
        let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.query_row(
            "SELECT path, hash FROM file_hashes WHERE path = ?1 LIMIT 1",
            [&q.path],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?
    };
    let ext = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    // dupe-watch may have already converted and cached this HEIC thumbnail -
    // serve that directly rather than converting again.
    if ext == "heic" {
        if let Some(size) = q.size {
            let cached_path = dupe_core::thumb_cache::thumb_path(&hash, size);
            if cached_path.exists() {
                let bytes = tokio::fs::read(&cached_path).await.ok();
                if let Some(bytes) = bytes {
                    return Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes));
                }
            }
        }
    }

    let size = q.size;
    let (content_type, bytes) = tokio::task::spawn_blocking(move || -> Option<(&'static str, Vec<u8>)> {
        if ext == "heic" {
            let img = dupe_core::heic::heic_via_quicklook(&path, &format!("raw{}", size.unwrap_or(0)))?;
            let img = match size {
                Some(max_px) if img.width() > max_px || img.height() > max_px => {
                    img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
                }
                _ => img,
            };
            let mut buf = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg).ok()?;
            Some(("image/jpeg", buf))
        } else {
            let bytes = std::fs::read(&path).ok()?;
            Some((mime_for_ext(&ext), bytes))
        }
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes))
}
```

(Note: this also switches `heic_via_quicklook` calls in this function to the `dupe_core::heic` version from Task 3, since that refactor lands before this task.)

- [ ] **Step 4: Run the full test suite**

Run: `cargo test -p dupe`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/tests/faces_server.rs
git commit -m "feat: check thumbnail cache in /api/raw before falling back to live HEIC conversion"
```

---

### Task 12: Documentation

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`

- [ ] **Step 1: Update `CLAUDE.md`**

Add a `dupe-watch` section to the CLI reference, documenting: the four stage flags (default all-on if none given), `--interval` (default 300s), foreground/Ctrl-C process model, the `~/.cache/dupe/thumbnails/` cache directory, and that `/api/raw` now checks this cache before live conversion. Add the WAL-mode migration note to the SQLite schema section (every binary now opens databases via `dupe_core::db::open_wal`).

- [ ] **Step 2: Update `README.md`**

Mirror the same additions in the user-facing usage examples, matching this file's existing tone/format (check current `--show-faces`/`--faces` wording as a template).

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md README.md
git commit -m "docs: document dupe-watch, its cache directory, and WAL mode"
```

---

### Task 13: Manual/browser verification

**Files:** none (verification only)

- [ ] **Step 1: Build release binaries**

Run: `cargo build --release --workspace`
Expected: clean build

- [ ] **Step 2: Prepare a fixture with real HEIC/GPS/duplicate data**

Use (or recreate) the `/tmp/dupe_verify` style fixture from prior verification work, or point at a real photo folder containing HEIC files and GPS-tagged photos.

- [ ] **Step 3: Run `dupe-watch` and confirm all four stages complete**

Run `dupe-watch <dir> --output-sqlite <db> --interval 30` (short interval for testing), let it run one full cycle, confirm via `sqlite3 <db>` that: `file_hashes` is populated, `faces` has rows for any real faces, `location_name` is populated for GPS rows, and `~/.cache/dupe/thumbnails/` has `.jpg` files for HEIC hashes.

- [ ] **Step 4: Confirm `dupe-report --show-faces` is now instant**

With `dupe-watch` having already run at least one cycle, start `dupe-report <db> --show-faces --by-date` and confirm HEIC thumbnails render immediately (no shimmer-loading delay) since `/api/raw` hits the pre-warmed cache - contrast with a fresh/unwatched database where the shimmer placeholder still appears on first load.

- [ ] **Step 5: Confirm concurrent access doesn't error**

With `dupe-watch` running in one terminal and `dupe-report --show-faces --faces` running in another against the same database, label a face via the `/faces` UI while `dupe-watch` is mid-cycle - confirm no "database is locked" errors in either process's output.

- [ ] **Step 6: Record findings**

Use the `superpowers:verify` skill's report format (PASS/FAIL/BLOCKED, steps taken) before considering the feature complete.
