# `videre scan` progress UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `videre scan` the same brew/docker/npm-style in-place progress bar already built for `videre faces`/`videre embed`, by making the shared `Progress` module thread-safe so it can be ticked directly from inside `scan`'s fully-parallel `rayon` hashing pass.

**Architecture:** `crates/videre-core/src/progress.rs`'s `Progress` type swaps its `done: u64` field for `AtomicU64` and changes `tick`/`tick_by` from `&mut self` to `&self`, since `indicatif::ProgressBar` (verified via its source: `Arc<Mutex<...>>`-backed, all mutating methods already `&self`) is already safe to share across threads and `Progress`'s own counter was the only non-thread-safe piece. This makes `Progress` automatically `Sync`, so `crates/videre/src/commands/scan.rs`'s `gather_records` can call `progress.tick()` directly from inside its `paths.par_iter().filter_map(...)` closure with no `Arc`/`Mutex` wrapping needed at the call site. The two existing sequential callers (`videre faces`'s pipeline, `videre embed`) need one mechanical `mut`-removal each, since `&self` methods no longer require a `mut` binding.

**Tech Stack:** Rust, `std::sync::atomic::AtomicU64` (stdlib, no new dependency). Baseline: `cargo test --workspace` = 198 passing on `main` at `816eba4`.

**House rules (mandatory):** never use the em dash character anywhere (code, comments, commit messages); no Co-Authored-By trailer or "Generated with" line; use the exact commit messages given.

**Branch:** work on a new branch `scan-progress-ui` off `main`:

```bash
cd /Users/erhangundogan/projects/rust/videre
git checkout -b scan-progress-ui
```

---

### Task 1: Make `Progress` thread-safe

**Files:**
- Modify: `crates/videre-core/src/progress.rs`

This task changes `Progress`'s internals only. It will leave the two
existing callers (`crates/videre-ml/src/pipeline.rs`,
`crates/videre/src/commands/embed.rs`) with a new `unused_mut` compiler
warning each, since their `let mut progress = ...` bindings no longer need
`mut` once `tick`/`tick_by` stop requiring `&mut self` - this is expected
and intentional; Task 2 fixes both in a dedicated, separately-committed
step. `cargo test` does not fail on warnings in this project (no
`deny(warnings)` or similar lint configuration exists), so this interim
state is safe to commit.

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` block in
`crates/videre-core/src/progress.rs`:

```rust
    #[test]
    fn concurrent_tick_from_multiple_threads_reaches_correct_total() {
        use std::sync::Arc;
        let progress = Arc::new(Progress::new(1000, true));
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let p = Arc::clone(&progress);
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        p.tick();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(progress.done.load(Ordering::Relaxed), 1000);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre-core --lib progress::tests::concurrent_tick_from_multiple_threads_reaches_correct_total`
Expected: COMPILE ERROR. Two reasons at once: `Ordering` is not yet
imported at the top of the file, and `Progress::new` currently returns a
type whose `tick` method takes `&mut self`, so `p.tick()` cannot be called
through the `Arc<Progress>` (`Arc` only gives shared access) inside the
`move` closure - the compiler will reject calling a `&mut self` method
through a shared reference.

- [ ] **Step 3: Implement**

Replace the top of `crates/videre-core/src/progress.rs`, from
`use indicatif::{ProgressBar, ProgressStyle};` through the `pub struct
Progress { ... }` block (currently lines 1-19), with:

```rust
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};

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
///
/// Safe to share across threads: every method takes `&self`, so a single
/// `Progress` value can be ticked concurrently from multiple `rayon`
/// worker threads (e.g. from inside a `.par_iter()` closure) with no
/// external `Arc`/`Mutex` wrapping needed at the call site.
pub struct Progress {
    total: u64,
    done: AtomicU64,
    mode: Mode,
}
```

Replace `Progress::new`'s body (currently constructing `Progress { total,
done: 0, mode }`) so the last line reads:

```rust
        Progress { total, done: AtomicU64::new(0), mode }
```

Replace `tick` and `tick_by` (currently the two methods between `new` and
`println`) with:

```rust
    /// Advance by one item. Safe to call concurrently from multiple threads
    /// (e.g. from inside a `rayon` `.par_iter()` closure) via a shared
    /// `&Progress` - no external synchronization needed.
    pub fn tick(&self) {
        self.tick_by(1);
    }

    /// Advance by `n` items at once (for callers that complete work in
    /// batches rather than one item at a time, e.g. `videre embed`'s
    /// chunked pipeline). `n` must not exceed the number of items remaining
    /// toward `total` (mirrors the same implicit contract `tick()` already
    /// has: callers are responsible for not calling it more times, or with
    /// a larger cumulative `n`, than `total` allows). Safe to call
    /// concurrently from multiple threads, same as `tick()`.
    pub fn tick_by(&self, n: u64) {
        let before = self.done.fetch_add(n, Ordering::Relaxed);
        let after = before + n;
        match &self.mode {
            Mode::Bar(bar) => bar.set_position(after),
            Mode::Plain => {
                if after / LOG_INTERVAL != before / LOG_INTERVAL || after == self.total {
                    eprintln!("{}/{} images processed", after, self.total);
                }
            }
            Mode::Silent => {}
        }
    }
```

Leave `println` and `finish` completely unchanged (`println` already took
`&self`; `finish` continues to take `self` by value).

Update the three existing tests that currently bind `let mut p =
Progress::new(...)` (`silent_mode_tick_does_not_panic`,
`zero_total_does_not_panic`, `silent_mode_tick_by_does_not_panic`) to drop
`mut`:

```rust
    #[test]
    fn silent_mode_tick_does_not_panic() {
        let p = Progress::new(10, true);
        for _ in 0..10 {
            p.tick();
        }
        p.finish();
    }
```

```rust
    #[test]
    fn zero_total_does_not_panic() {
        let p = Progress::new(0, true);
        p.tick();
        p.finish();
    }
```

```rust
    #[test]
    fn silent_mode_tick_by_does_not_panic() {
        let p = Progress::new(100, true);
        p.tick_by(40);
        p.tick_by(60);
        p.finish();
    }
```

`silent_mode_println_still_prints` already binds without `mut` and needs no
change.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-core --lib progress::`
Expected: PASS (5 tests: 4 existing + 1 new).

Run: `cargo test --workspace`
Expected: PASS, 199 total (198 baseline + 1).

Run: `cargo build --workspace 2>&1 | grep -i warning`
Expected: exactly two lines, both `unused_mut`, one in
`crates/videre-ml/src/pipeline.rs` and one in
`crates/videre/src/commands/embed.rs` - this is the expected interim state
described above, resolved by Task 2. No other warnings should appear; if
any warning outside these two specific `unused_mut` sites appears, that is
a real regression to investigate.

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/progress.rs
git commit -m "feat: Progress is thread-safe via an atomic counter"
```

---

### Task 2: Drop `mut` at the two existing call sites

Resolves the two `unused_mut` warnings introduced by Task 1. Purely
mechanical - no behavior change, since both files still call `tick()`/
`tick_by()` sequentially, one thread at a time.

**Files:**
- Modify: `crates/videre-ml/src/pipeline.rs:34`, `crates/videre/src/commands/embed.rs:44`

- [ ] **Step 1: Implement**

In `crates/videre-ml/src/pipeline.rs`, change line 34:

```rust
    let mut progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);
```

to:

```rust
    let progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);
```

In `crates/videre/src/commands/embed.rs`, change line 44:

```rust
    let mut progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);
```

to:

```rust
    let progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);
```

No other line in either file changes - both files already only call
`tick()`, `tick_by()`, `println()`, and `finish()` on `progress` (verified:
no other method or field access exists), all of which now work identically
through a non-`mut` binding.

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p videre-ml --lib pipeline::`
Expected: PASS (2 tests, unchanged).

Run: `cargo test -p videre --bin videre embed::tests`
Expected: PASS (2 tests, unchanged).

Run: `cargo test --workspace`
Expected: PASS, 199 total (unchanged from Task 1 - no tests added or
removed, this task is a pure mechanical fix).

Run: `cargo build --workspace 2>&1 | grep -i warning`
Expected: empty - this confirms both `unused_mut` warnings from Task 1 are
now resolved and no other warning was introduced.

- [ ] **Step 3: Commit**

```bash
git add crates/videre-ml/src/pipeline.rs crates/videre/src/commands/embed.rs
git commit -m "fix: drop now-unnecessary mut on Progress bindings"
```

---

### Task 3: `videre scan` uses `Progress`

Rewires `gather_records` to tick a bar once per file during hashing
(directly from inside the existing `rayon` parallel pass), routes the
hash-failure warning through `progress.println` (now unconditional, no
longer gated by `--silent`), and adds a skip count to the final "Wrote"
summary line via a new `format_write_summary` helper.

**Files:**
- Modify: `crates/videre/src/commands/scan.rs`
- Test: `crates/videre/tests/scan.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/videre/tests/scan.rs` (append after the existing tests, before
the closing of the file):

```rust
#[test]
fn skipped_files_are_reported_in_wrote_summary() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"valid content").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/nonexistent/target", scan_dir.path().join("broken.jpg")).unwrap();

    let out = Command::new(videre_bin())
        .arg("scan")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .output()
        .expect("failed to run videre");

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Wrote 1 record(s)"), "{stderr}");
    assert!(stderr.contains("(1 skipped)"), "{stderr}");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "only the valid file should have been written");
}
```

The `#[cfg(unix)]` guard matches this project's macOS/Linux-only scope (no
Windows-specific code or CI target exists anywhere in this codebase); on a
non-Unix target the symlink line compiles out and the test would see 0
skipped files, but since this project has no non-Unix target, that
fallback path is not exercised.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre --test scan skipped_files_are_reported_in_wrote_summary`
Expected: FAIL. The current `"Wrote {} record(s) to {:?}"` line has no
skip-count suffix at all, so the `stderr.contains("(1 skipped)")` assertion
fails.

- [ ] **Step 3: Implement**

Replace `gather_records` (currently `crates/videre/src/commands/scan.rs`
lines 57-93) with:

```rust
fn gather_records(args: &ScanArgs, directory: &std::path::Path) -> (Vec<videre::types::FileRecord>, usize) {
    let paths = scanner::scan(directory);
    let progress = videre_core::progress::Progress::new(paths.len() as u64, args.silent);

    let records: Vec<_> = paths
        .par_iter()
        .filter_map(|path| {
            let result = hasher::hash_file(path)
                .map_err(|e| {
                    progress.println(&format!("Warning: skipping {:?}: {}", path, e));
                })
                .ok();
            progress.tick();
            result
        })
        .collect();

    progress.finish();

    let skipped = paths.len() - records.len();

    let records = if args.similar {
        records
            .into_iter()
            .map(|mut r| {
                r.phash = hasher::compute_dhash(std::path::Path::new(&r.path));
                r
            })
            .collect()
    } else {
        records
    };

    (records, skipped)
}
```

Add a new function immediately after `gather_records`:

```rust
/// Formats the "Wrote N record(s) to <path>" summary line, with an
/// "(M skipped)" suffix when `skipped > 0`, omitted entirely when `skipped
/// == 0` (matching `videre embed`'s equivalent omit-when-zero precedent).
fn format_write_summary(written: usize, skipped: usize, dest: &str) -> String {
    if skipped > 0 {
        format!("Wrote {written} record(s) to {dest} ({skipped} skipped)")
    } else {
        format!("Wrote {written} record(s) to {dest}")
    }
}
```

In `run_text` (currently `crates/videre/src/commands/scan.rs:129-171`),
change the call site and both print statements. Current:

```rust
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
```

becomes:

```rust
    let (records, skipped) = gather_records(&args, &directory);

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
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", db_path)));
            }
        }
        Ok(OutputTarget::Jsonl(path)) => {
            if let Err(e) = output::append_records(&records, &path) {
                eprintln!("Error writing to {:?}: {}", path, e);
                process::exit(1);
            }
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", path)));
            }
        }
    }

    Ok(())
}
```

In `run_json` (currently `crates/videre/src/commands/scan.rs:176-211`),
apply the same pattern. Current:

```rust
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

becomes:

```rust
    let (records, skipped) = gather_records(args, &directory);

    let output = match output_target(args)? {
        OutputTarget::Sqlite(db_path) => {
            sqlite_output::write_records(&records, &db_path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))?;
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", db_path)));
            }
            ScanOutputJson { kind: "sqlite", path: db_path.display().to_string() }
        }
        OutputTarget::Jsonl(path) => {
            output::append_records(&records, &path)
                .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", path, e))?;
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", path)));
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

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --test scan`
Expected: PASS (16 tests: 15 existing + 1 new
`skipped_files_are_reported_in_wrote_summary`).

Run: `cargo test --workspace`
Expected: PASS, 200 total (199 at the end of Task 2, plus 1 new).

Run: `cargo build --workspace 2>&1 | grep -i warning`
Expected: empty.

Verify by hand:
```bash
cargo run -q -p videre --bin videre -- scan --help
```
Expected: unchanged flags (`--output`, `--output-sqlite`, `--similar`,
`--silent`, `--json`) - this task does not change `ScanArgs`.

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/scan.rs crates/videre/tests/scan.rs
git commit -m "feat: videre scan reports progress via Progress during hashing"
```

---

### Task 4: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full suite**

Run: `cargo test --workspace`
Expected: PASS, 200 total, 0 failed, 0 compiler warnings (`cargo build
--workspace 2>&1 | grep -i warning` empty).

- [ ] **Step 2: Release build**

Run: `cargo build --release`
Expected: succeeds with no errors.

- [ ] **Step 3: Automated end-to-end smoke test**

```bash
D=$(mktemp -d)
printf same > "$D/a.jpg"; printf same > "$D/b.jpg"; printf different > "$D/c.jpg"

H1=$(mktemp -d)
VIDERE_HOME=$H1 ./target/release/videre scan --silent "$D" > /tmp/scan-out.log 2>&1
echo "silent exit=$?"
cat /tmp/scan-out.log

echo "--- non-silent, non-TTY ---"
H2=$(mktemp -d)
VIDERE_HOME=$H2 ./target/release/videre scan "$D" > /tmp/scan-out2.log 2>&1
echo "exit=$?"
cat /tmp/scan-out2.log

rm -rf "$H1" "$H2" "$D" /tmp/scan-out.log /tmp/scan-out2.log
```

Expected: `--silent` run produces completely empty output and exit 0.
Each run uses its own fresh `VIDERE_HOME` (`$H1`, `$H2`), so both scan the
same 3 fresh files independently rather than the second seeing an
already-populated database from the first. The non-silent run's output
contains no `"Scanning "` or `"Found "` lines
(removed by this plan), and no per-file `"[faces]"`-style spam (scan never
had that); it shows periodic `N/total images processed` lines (the non-TTY
`Mode::Plain` fallback, since this script's stderr is redirected to a file)
followed by a `"Wrote 3 record(s) to ..."` line with no skip-count suffix
(all 3 files hash successfully).

- [ ] **Step 4: Manual TTY verification**

This step cannot be automated (same reasoning as the `faces`/`embed`
progress-UI plans' equivalent steps) - it must be run by a human at a real
terminal, not by an agent, since an agent's shell is typically not a TTY.

```bash
# Requires a directory with many image files.
# Run in a real terminal, not piped.
./target/release/videre scan --output-sqlite /tmp/verify.db <a large directory>
```

Expected: an in-place percentage bar appears and updates smoothly (per
file, not in chunky jumps - `scan` ticks once per file directly from the
parallel hashing pass, unlike `embed`'s per-chunk `tick_by`), clears
cleanly, and is followed by a `"Wrote N record(s) to ..."` line. No
`"Scanning "`/`"Found "` lines and no per-file spam should appear anywhere
in the output.

- [ ] **Step 5: Record results**

PASS/FAIL per step; any FAIL loops back to the owning task.
