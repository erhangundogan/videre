# `videre embed` progress UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `videre embed`'s repeated per-chunk console lines with the same brew/docker/npm-style in-place progress bar already built for `videre faces`, by adding one bulk-advance method to the shared `Progress` module and rewiring `embed.rs` to use it.

**Architecture:** `crates/videre-core/src/progress.rs`'s `Progress` type gains `tick_by(&mut self, n: u64)`, a bulk-advance sibling to the existing `tick()`, since `embed.rs` completes work in whole chunks (default 500 images, decoded in parallel via `rayon`) rather than one image at a time. `crates/videre/src/commands/embed.rs` then constructs a `Progress`, calls `tick_by` once per chunk after that chunk's decode/embed/write finishes, routes its one error message through `progress.println` (already safe to call from the parallel decode closure since it takes `&self`), and replaces its three scattered `eprintln!` sites with one consolidated summary line.

**Tech Stack:** Rust, the existing `Progress` module (no new dependencies - `indicatif` is already a `videre-core` dependency from the prior `videre faces` progress-UI work). Baseline: `cargo test --workspace` = 195 passing on `main` at `dc41298`.

**House rules (mandatory):** never use the em dash character anywhere (code, comments, commit messages); no Co-Authored-By trailer or "Generated with" line; use the exact commit messages given.

**Branch:** work on a new branch `embed-progress-ui` off `main`:

```bash
cd /Users/erhangundogan/projects/rust/videre
git checkout -b embed-progress-ui
```

---

### Task 1: `Progress::tick_by` bulk-advance method

**Files:**
- Modify: `crates/videre-core/src/progress.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` block in `crates/videre-core/src/progress.rs`:

```rust
    #[test]
    fn silent_mode_tick_by_does_not_panic() {
        let mut p = Progress::new(100, true);
        p.tick_by(40);
        p.tick_by(60);
        p.finish();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre-core --lib progress::tests::silent_mode_tick_by_does_not_panic`
Expected: COMPILE ERROR (`tick_by` is not a method on `Progress` yet).

- [ ] **Step 3: Implement**

In `crates/videre-core/src/progress.rs`, add `tick_by` to the `impl Progress` block, immediately after the existing `tick` method and before `println`:

```rust
    /// Advance by `n` items at once (for callers that complete work in
    /// batches rather than one item at a time, e.g. `videre embed`'s
    /// chunked pipeline). `n` must not exceed the number of items remaining
    /// toward `total` (mirrors the same implicit contract `tick()` already
    /// has: callers are responsible for not calling it more times than
    /// `total` allows).
    pub fn tick_by(&mut self, n: u64) {
        let before = self.done;
        self.done += n;
        match &self.mode {
            Mode::Bar(bar) => bar.set_position(self.done),
            Mode::Plain => {
                if self.done / LOG_INTERVAL != before / LOG_INTERVAL || self.done == self.total {
                    eprintln!("{}/{} images processed", self.done, self.total);
                }
            }
            Mode::Silent => {}
        }
    }
```

Do not modify `tick()`, `println()`, `finish()`, or anything else in this file - this is a purely additive method.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-core --lib progress::`
Expected: PASS (5 tests: 4 existing + 1 new).

Run: `cargo test --workspace`
Expected: PASS, 196 total (195 baseline + 1). No compiler warnings (`cargo build --workspace 2>&1 | grep -i warning` empty).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/progress.rs
git commit -m "feat: Progress::tick_by bulk-advances for chunk-based callers"
```

---

### Task 2: `videre embed` uses `Progress`

Replaces the leading count line, the per-chunk progress line, and the final "Done:" line with one bar plus one consolidated summary. Routes the existing decode-skip error message through `progress.println`.

**Files:**
- Modify: `crates/videre/src/commands/embed.rs`

- [ ] **Step 1: Write the failing tests**

Add a `#[cfg(test)] mod tests` block at the end of `crates/videre/src/commands/embed.rs` (the file currently has no test module):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_summary_no_skips() {
        let summary = format_summary(234, 0, std::time::Duration::from_secs(41));
        assert_eq!(summary, "234 image(s) embedded, done in 41s");
    }

    #[test]
    fn format_summary_with_skips() {
        let summary = format_summary(230, 4, std::time::Duration::from_secs(41));
        assert_eq!(summary, "230 image(s) embedded, 4 skipped, done in 41s");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre --bin videre embed::tests`
Expected: COMPILE ERROR (`format_summary` does not exist yet).

- [ ] **Step 3: Implement**

Replace the entire contents of `crates/videre/src/commands/embed.rs` from `use anyhow::{Context, Result};` through the end of `pub fn run(args: EmbedArgs) -> Result<()> { ... }` (i.e. everything before the test module added in Step 1) with:

```rust
use anyhow::{Context, Result};
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, preprocess};
use rayon::prelude::*;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct EmbedArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Inference batch size
    #[arg(long, default_value_t = 32)]
    batch: usize,

    /// Rows written per transaction (resume granularity)
    #[arg(long, default_value_t = 500)]
    chunk: usize,

    /// Suppress progress output on stderr (errors always shown)
    #[arg(long)]
    silent: bool,
}

pub fn run(args: EmbedArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)
        .with_context(|| format!("open {}", db.display()))?;
    embeddings::ensure_embeddings_table(&conn)?;

    let pending = embeddings::pending_images(&conn, model::MODEL_ID)?;
    if pending.is_empty() {
        if !args.silent {
            eprintln!("Nothing to embed: all hashes already have embeddings.");
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let dev = device::best_device();
    let embedder = model::Embedder::load(dev.clone())?;

    let mut progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);

    let mut done = 0usize;
    let mut failed = 0usize;
    for chunk in pending.chunks(args.chunk) {
        // Decode in parallel; None = unreadable, logged and skipped.
        let decoded: Vec<Option<(String, candle_core::Tensor)>> = chunk
            .par_iter()
            .map(|p| {
                match preprocess::image_to_tensor(
                    std::path::Path::new(&p.path),
                    model::IMAGE_SIZE,
                    &candle_core::Device::Cpu, // decode on CPU, move to device in batch
                ) {
                    Ok(t) => Some((p.hash.clone(), t)),
                    Err(e) => {
                        progress.println(&format!("skip {}: {e:#}", p.path));
                        None
                    }
                }
            })
            .collect();
        let decoded: Vec<(String, candle_core::Tensor)> =
            decoded.into_iter().flatten().collect();
        failed += chunk.len() - decoded.len();

        let mut rows: Vec<(String, Vec<u8>)> = Vec::with_capacity(decoded.len());
        for batch in decoded.chunks(args.batch) {
            let tensors: Vec<candle_core::Tensor> = batch
                .iter()
                .map(|(_, t)| t.to_device(&dev))
                .collect::<candle_core::Result<_>>()?;
            let vecs = embedder.embed_images(&tensors)?;
            for ((hash, _), v) in batch.iter().zip(vecs) {
                rows.push((hash.clone(), vectors::to_f16_bytes(&v)));
            }
        }

        embeddings::insert_embeddings(&conn, model::MODEL_ID, &rows)?;
        done += rows.len();
        progress.tick_by(chunk.len() as u64);
    }

    progress.finish();

    if !args.silent {
        eprintln!("{}", format_summary(done, failed, started.elapsed()));
    }
    Ok(())
}

/// Assembles the single consolidated summary line printed after embedding
/// finishes. Not `pub(crate)` (unlike `videre faces`'s equivalent
/// `format_summary`): nothing outside this file calls it - `videre embed`
/// has no `videre watch` stage equivalent that shares this logic.
fn format_summary(done: usize, failed: usize, elapsed: std::time::Duration) -> String {
    if failed > 0 {
        format!("{done} image(s) embedded, {failed} skipped, done in {}s", elapsed.as_secs())
    } else {
        format!("{done} image(s) embedded, done in {}s", elapsed.as_secs())
    }
}
```

Note two deliberate details preserved from the design:
- `progress.tick_by(chunk.len() as u64)` advances by the whole chunk (successes plus decode-skip failures), not just `rows.len()`, so the bar reaches 100% by the end of the run regardless of how many images were skipped.
- `format_summary` omits the `", N skipped"` clause entirely when `failed == 0`, rather than always printing `", 0 skipped"` as the old code did.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre --bin videre embed::tests`
Expected: PASS (2 tests: `format_summary_no_skips`, `format_summary_with_skips`).

Run: `cargo test --workspace`
Expected: PASS, 198 total (196 at the end of Task 1, plus 2 new). No compiler warnings (`cargo build --workspace 2>&1 | grep -i warning` empty).

Verify by hand: `cargo run -q -p videre --bin videre -- embed --help` still shows the same flags (`--db`, `--batch`, `--chunk`, `--silent`) - this task does not change `EmbedArgs`.

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/embed.rs
git commit -m "feat: videre embed reports progress via Progress instead of per-chunk prints"
```

---

### Task 3: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Full suite**

Run: `cargo test --workspace`
Expected: PASS, 198 total, 0 failed, 0 compiler warnings (`cargo build --workspace 2>&1 | grep -i warning` empty).

- [ ] **Step 2: Release build**

Run: `cargo build --release`
Expected: succeeds with no errors.

- [ ] **Step 3: Automated end-to-end smoke test**

This step CAN be automated (unlike the TTY-bar-rendering check in Step 4 below), since it only exercises the non-TTY fallback path and the final summary text, both of which are deterministic:

```bash
H=$(mktemp -d); D=$(mktemp -d)
cp crates/videre/tests/fixtures/sample_with_exif.jpg "$D/a.jpg"
VIDERE_HOME=$H ./target/release/videre scan --silent "$D"
VIDERE_HOME=$H ./target/release/videre embed --db "$H/hashes.db" > /tmp/embed-out.log 2>&1
echo "exit=$?"
cat /tmp/embed-out.log
rm -rf "$H" "$D" /tmp/embed-out.log
```

Expected: exit 0. Output contains no per-chunk `"embedded N/M"` lines and no `"N image(s) to embed"` leading line; ends with a line matching `"1 image(s) embedded, done in Ns"` (or, if the model reports the image unreadable, `"0 image(s) embedded, 1 skipped, done in Ns"` - either is an acceptable pass, since the point of this check is confirming the OLD per-chunk/leading-count lines are gone and the NEW consolidated summary format appears, not the specific embedding outcome for this one fixture). Note: this downloads the SigLIP model (~1.8 GB) on first run if not already cached in `~/.cache/huggingface/` from prior use of `videre embed`/`videre search` on this machine - if that download is not feasible in the environment running this step, skip Step 3 and rely on Task 2's unit tests plus Step 4's manual verification instead.

- [ ] **Step 4: Manual TTY verification**

This step cannot be automated (same reasoning as the `videre faces` progress-UI plan's equivalent step) - it must be run by a human at a real terminal, not by an agent, since an agent's shell is typically not a TTY.

```bash
# Requires a database with pending (not-yet-embedded) images.
# Run in a real terminal, not piped.
./target/release/videre embed --db <path-to-a-db-with-pending-images>
```

Expected: an in-place percentage bar appears and updates in `--chunk`-sized jumps (default 500) during embedding, clears cleanly, and is followed by exactly one summary line matching the format from Task 2 (e.g. `"1842 image(s) embedded, done in 213s"`). No per-chunk `"embedded N/M"` lines and no leading `"N image(s) to embed"` line should appear anywhere in the output.

- [ ] **Step 5: Record results**

PASS/FAIL per step; any FAIL loops back to the owning task.
