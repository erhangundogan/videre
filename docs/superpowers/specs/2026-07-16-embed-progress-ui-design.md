# `videre embed` progress UI Design

## Problem

`videre embed` prints a repeated per-chunk progress line during embedding
(`"embedded {done}/{} ({failed} skipped)"`, in
`crates/videre/src/commands/embed.rs:85`), plus a leading count line
(`"{} image(s) to embed"`, `embed.rs:40`) and a final summary
(`"Done: {done} embedded, {failed} skipped."`, `embed.rs:90`). This is a
smaller version of the same problem already fixed for `videre faces`: no
in-place bar, and output that scrolls once per chunk instead of showing live
progress.

## Goal

Reuse the `Progress` module built for `videre faces`
(`crates/videre-core/src/progress.rs`) to give `videre embed` the same
brew/docker/npm-style in-place bar, plus one consolidated summary line after
the run finishes, replacing the three eprintln sites above.

Scope: `videre embed` only (`crates/videre/src/commands/embed.rs`), plus one
new method on the existing `Progress` type
(`crates/videre-core/src/progress.rs`) that `embed.rs` needs and `videre
faces`/`videre watch` do not. No other subcommand is touched.

## Why a new `Progress` method is needed

`embed.rs` decodes images in parallel (`rayon`) within each chunk (default
500 images, `--chunk`), then embeds and writes the whole chunk as a batch,
then (today) prints one line per chunk. Unlike `videre faces`'s
`run_face_pipeline` (which processes and ticks one image at a time in a
sequential loop), `embed.rs`'s natural unit of "progress became visible" is
one whole chunk, not one image. The existing `Progress::tick()` only
advances by one item per call, so a per-chunk caller would need to call it
up to `--chunk` (500) times back-to-back just to move a counter -
mechanically correct but wasteful and awkward at the call site.

**Decision (confirmed in brainstorming): add `Progress::tick_by(&mut self, n:
u64)`, a bulk-advance method, rather than looping `tick()` calls.** This
keeps `tick()` and all of its existing callers (`videre faces`,
`crates/videre-ml/src/pipeline.rs`) completely unchanged - `tick_by` is
purely additive.

**Decision (confirmed in brainstorming): bulk-advance once per chunk, back
in the sequential chunk loop, not per-image from inside the parallel decode
step.** The parallel decode step (`chunk.par_iter().map(...)` at
`embed.rs:50-65`) stays untouched; ticking happens after that step
completes and the chunk's results are known, avoiding any need for
thread-safe/atomic tick state. A smoother per-image-from-worker-threads bar
was considered and explicitly rejected: it would require `Progress` to grow
an `Arc<Mutex<>>`-based or atomic tick path, more moving parts for a
smoothness gain most users would not distinguish from chunk-level jumps at
typical library sizes.

## Changes to `crates/videre-core/src/progress.rs`

Add `tick_by`, immediately after the existing `tick` method (i.e. between
`tick` and `println` in the `impl Progress` block):

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

The `Plain`-mode interval check cannot reuse `tick()`'s `self.done.is_multiple_of(LOG_INTERVAL)`
check unmodified: a single `tick_by(500)` call can jump straight past several
`LOG_INTERVAL` (25) boundaries at once, and `is_multiple_of` only catches an
exact landing on a multiple, not crossing one. The boundary-crossing
comparison (`self.done / LOG_INTERVAL != before / LOG_INTERVAL`) correctly
fires exactly once per `tick_by` call whenever one or more 25-item
boundaries were crossed during that call, printing the bulk-updated total
(not one line per boundary crossed - that would reintroduce spam for a
500-item jump, which is exactly what this change is trying to avoid).

No new unit tests are strictly required for `tick_by` beyond what already
exists for `tick()` (both exercise the same `Mode::Silent` path, the only
mode unit-testable without mocking a TTY - see the `videre faces` progress
spec's Testing section for why `Mode::Bar`/`Mode::Plain` aren't unit tested
directly), but one new test is added anyway since `tick_by`'s `Mode::Plain`
boundary-crossing logic is genuinely new code, not just a call to existing
logic (see Testing section below).

## Changes to `crates/videre/src/commands/embed.rs`

Current relevant structure (read the full file at
`crates/videre/src/commands/embed.rs` for exact context):

- `embed.rs:33-37`: early return with `"Nothing to embed: all hashes already have embeddings."` when `pending.is_empty()` - **unchanged**, this path never reaches the progress bar at all since there is nothing to report progress on.
- `embed.rs:39-41`: `if !args.silent { eprintln!("{} image(s) to embed", pending.len()); }` - **removed**. Per the confirmed brainstorming decision, this matches the `videre faces` precedent (which removed its equivalent `"Processing {} images..."` line): the bar's own appearance communicates that work has started, and a leading count line duplicates information already visible.
- `embed.rs:60`: `eprintln!("skip {}: {e:#}", p.path);` inside the `chunk.par_iter().map(...)` parallel closure - **changed** to `progress.println(&format!("skip {}: {e:#}", p.path));`. `Progress::println` takes `&self` (not `&mut self`) and performs no mutation, so it is already safe to call concurrently from multiple `rayon` worker threads without any new synchronization - no change to `Progress` itself is needed for this. This preserves the existing unconditional (not gated by `--silent`) visibility of decode failures, matching the `videre faces` precedent for `detect failed`/`embed_batch failed`/`write failed` messages.
- `embed.rs:84-86`: the per-chunk `if !args.silent { eprintln!("embedded {done}/{} ({failed} skipped)", pending.len()); }` - **removed**, replaced by a `progress.tick_by(...)` call (see below).
- `embed.rs:89-91`: the final `if !args.silent { eprintln!("Done: {done} embedded, {failed} skipped.", ...) }` - **replaced** with one consolidated summary line via a new `format_summary` function (see below), matching the `videre faces` precedent's single-summary-line design.

Full replacement of `crates/videre/src/commands/embed.rs`'s `run` function
(the entire function body from `pub fn run(args: EmbedArgs) -> Result<()> {`
through its closing `}`), plus a new `format_summary` function added after
it:

```rust
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
/// finishes. `pub(crate)` is not needed here (unlike `videre faces`'s
/// `format_summary`/`format_clustering_only_summary`) since nothing outside
/// `embed.rs` calls this - `videre embed` has no `videre watch` stage
/// equivalent that shares this logic.
fn format_summary(done: usize, failed: usize, elapsed: std::time::Duration) -> String {
    if failed > 0 {
        format!("{done} image(s) embedded, {failed} skipped, done in {}s", elapsed.as_secs())
    } else {
        format!("{done} image(s) embedded, done in {}s", elapsed.as_secs())
    }
}
```

Two behavior notes, both deliberate:

- `progress.tick_by(chunk.len() as u64)` advances by the WHOLE chunk size
  (embedded successes plus decode-skip failures), not just `rows.len()`
  (successes only) - matching the "the bar always reaches 100% by the end"
  requirement from the `videre faces` precedent (`run_face_pipeline` ticks
  once per attempted image regardless of outcome, for the same reason).
- `format_summary` omits the `", N skipped"` clause entirely when
  `failed == 0`, rather than always printing `", 0 skipped"` - this is a
  small deliberate improvement over the old unconditional
  `"({failed} skipped)"` phrasing (which always showed "(0 skipped)" on a
  fully successful run), decided here rather than left as an open question
  since it costs nothing and reads better on the common case.

`format_summary` takes `done: usize, failed: usize, elapsed: Duration`
directly (not a struct like `videre faces`'s `FacesRunResult`) since
`embed.rs` has no equivalent result struct to build - `done`/`failed` are
already local `usize` counters in the existing code, and introducing a new
struct just to pass two integers through one function call would be
needless ceremony for a `crate`-private helper with exactly one call site.

## Testing

`crates/videre-core/src/progress.rs`: add one new test alongside the
existing three, covering `tick_by`'s `Mode::Silent` path (the only mode unit
testable without mocking a TTY, per the existing tests' established pattern
in this file):

```rust
    #[test]
    fn silent_mode_tick_by_does_not_panic() {
        let mut p = Progress::new(100, true);
        p.tick_by(40);
        p.tick_by(60);
        p.finish();
    }
```

`crates/videre/src/commands/embed.rs` currently has no `#[cfg(test)] mod
tests` block. Add one, covering `format_summary`'s two branches (with and
without skipped images):

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

Manual verification (not automatable, same reasoning as the `videre faces`
progress spec): running `videre embed` in a real terminal against a
database with pending images to confirm the bar renders in place, jumping
in `--chunk`-sized increments (default 500), and clears cleanly before the
final summary; running it piped to a file to confirm the non-TTY fallback
shows periodic `N/total images processed` lines (via `tick_by`'s new
boundary-crossing logic) rather than a bar or per-chunk spam.

## Out of scope

- `videre scan` - surveyed and found to have no per-item verbosity to fix (it
  hashes files in parallel with only two summary lines, no per-file loop
  output).
- `videre dedupe` - its only per-line output (`REMOVE` candidate paths on
  stdout) is the command's actual intended output, not progress spam; there
  is nothing to replace.
- `videre watch --heic`/`--location` - each prints exactly one summary line
  per stage per cycle already (no per-item spam to begin with); lower value
  than `embed` and not requested.
- `--json` output for `videre embed` - the command has no `--json` flag
  today and this design doesn't add one; all changes here are to
  human-readable stderr output only.
