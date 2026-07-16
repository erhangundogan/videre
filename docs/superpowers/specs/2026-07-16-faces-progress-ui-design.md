# `videre faces` progress UI Design

## Problem

`videre faces` currently prints one line per image during detection
(`[faces] {path}: {N} face(s)`, in `crates/videre-ml/src/pipeline.rs:62`),
which floods the terminal on any library larger than a handful of photos.
There is no sense of overall progress (a percentage, an in-place bar) and no
consolidated summary of what happened (images processed, faces found,
clustering outcome, errors) - just a scroll of per-image lines followed by a
single terse `"Done: N new face(s) detected."`.

## Goal

Replace the per-image spam with a brew/docker/npm-style in-place progress
bar (percentage only, no per-item text) during detection, and a single
consolidated summary line after clustering finishes, covering: images
processed, faces found, clustering outcome (people found, eps used), elapsed
time, and error count (only shown when errors occurred).

Scope: `videre faces` only (`crates/videre-ml/src/pipeline.rs`,
`crates/videre/src/commands/faces.rs`). `videre embed` and `videre scan` have
similar per-file verbosity but are explicitly out of scope for this change -
revisit them later if wanted, informed by how this one turns out.

## Approach

Build a small, reusable progress-reporting module in `videre-core` (used by
`videre-ml`'s pipeline today; available to `embed`/`scan` later without
re-deriving the TTY-detection/silent-mode logic) rather than wiring
`indicatif` directly into `pipeline.rs`. `videre-ml` already depends on
`videre-core` (see `crates/videre-ml/Cargo.toml:10`), so this requires no new
inter-crate dependency.

## New dependency

Add `indicatif = "0.17"` to `crates/videre-core/Cargo.toml`'s
`[dependencies]`. TTY detection uses `std::io::IsTerminal` (stable since Rust
1.70, already satisfied by this project's toolchain - `rustc 1.96.0`
confirmed), so no separate detection crate (e.g. `is-terminal`) is needed;
`indicatif` does not require one either for this design, since our own
`Progress::new` decides up front whether to render a real bar or fall back to
periodic plain text, rather than relying on `indicatif`'s own hidden-target
behavior.

## New module: `crates/videre-core/src/progress.rs`

```rust
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::time::Instant;

/// Reports progress for a batch of N items as an in-place bar (brew/docker/
/// npm style) when stderr is a terminal, or periodic plain-text lines when
/// it isn't (piped to a file, CI log) - so a long run never looks hung in a
/// log file, without per-item spam either way. `silent` suppresses the bar
/// and periodic lines entirely, but NOT error output (see `println`) or the
/// caller's own decision about whether to print a final summary.
pub struct Progress {
    total: u64,
    done: u64,
    started: Instant,
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
        Progress { total, done: 0, started: Instant::now(), mode }
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

    /// Elapsed time since `new()`, for the caller's final summary line.
    pub fn elapsed(&self) -> std::time::Duration {
        self.started.elapsed()
    }

    /// Clears the bar (if any) so the final summary prints cleanly below it
    /// rather than being overwritten. Does not print anything itself - the
    /// caller assembles and prints its own summary line(s), since the exact
    /// content (faces found, clustering stats, error count) is known only to
    /// the caller, not to `Progress`.
    pub fn finish(self) {
        if let Mode::Bar(bar) = self.mode {
            bar.finish_and_clear();
        }
    }
}
```

Register in `crates/videre-core/src/lib.rs`: add `pub mod progress;`
(alphabetical: after `person_search`, before `thumb_cache`).

## Changes to `crates/videre-ml/src/pipeline.rs`

### `run_face_pipeline`

- Remove the per-image `if !silent { eprintln!("[faces] {path}: {} face(s)", ...) }` line (`pipeline.rs:62`) entirely - this is the spam being eliminated.
- Remove the leading `if !silent { eprintln!("Processing {} images...", to_process.len()); }` line (`pipeline.rs:28`) - the progress bar's appearance communicates this now; keeping both would be redundant.
- Construct `let mut progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);` right after the empty-check at the top of the function.
- Call `progress.tick()` once per attempted image, inside the `for (path, hash) in chunk` loop, unconditionally (whether that image loaded, detected zero faces, or failed) - so the bar always reflects "images attempted," matching the total passed to `Progress::new`.
- Change the three existing error sites to use `progress.println(...)` instead of bare `eprintln!`, so they surface above an active bar without corrupting it:
  - `detect failed {path}: {e}` (`pipeline.rs:54`)
  - `embed_batch failed: {e}` (`pipeline.rs:72`)
  - `write failed {}: {e}` (`pipeline.rs:99`)
- Call `progress.finish()` once, after the `for chunk in ...` loop ends, before returning `FacesRunResult`.
- Extend `FacesRunResult` with two new fields needed for the caller's summary line:

```rust
pub struct FacesRunResult {
    pub total_faces: usize,
    pub write_errors: usize,
    pub images_processed: usize,
    pub detect_errors: usize,
}
```

  `images_processed` = `to_process.len()` (every image attempted, matching what the bar counted). `detect_errors` counts detection failures (`pipeline.rs:54`'s branch) plus, for each `embed_batch` failure (`pipeline.rs:72`'s branch), the number of images in that chunk whose crops were discarded (`chunk_entries.len()` at that point) - both are failures that prevented faces from being detected/embedded, distinct from `write_errors` (faces were found but couldn't be persisted). Existing tests referencing `FacesRunResult { total_faces, write_errors }` field-by-name construction need updating for the two new fields (see Testing section).

### `run_clustering`

Currently prints its own summary line and returns `Result<()>`. Change to
return data instead of printing, so `faces.rs::run` can fold it into one
consolidated summary:

```rust
pub struct ClusteringResult {
    pub total_faces: usize,
    pub clustered_faces: usize,
    pub cluster_count: usize,
}

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

`run_clustering` drops its `silent` parameter entirely (it no longer prints
anything itself - printing is now the caller's job) and returns
`Option<ClusteringResult>` (`None` when there were no faces to cluster,
replacing the old `"No faces in DB to cluster."` message, which the caller
now decides whether/how to report). Per the earlier design decision,
clustering gets no progress indicator of its own (DBSCAN over in-memory
embeddings is fast; a brief pause before the summary line is acceptable) -
this is a pure data-return refactor, not a UI change.

## Changes to `crates/videre/src/commands/faces.rs`

Replace the two existing summary prints:

```rust
if !args.silent { eprintln!("Done: {} new face(s) detected.", result.total_faces); }
```

and `run_clustering`'s old internal print, with one consolidated summary
assembled after both detection and clustering finish. Exact format:

```
234 image(s) processed, 187 face(s) found, 152/187 clustered into 14 people (eps=0.60), done in 41s
```

With errors:

```
234 image(s) processed, 187 face(s) found, 152/187 clustered into 14 people (eps=0.60), done in 41s, 3 error(s) (see above)
```

When there are no faces at all to cluster (fresh library, nothing detected):

```
234 image(s) processed, 0 face(s) found, done in 41s
```

Implementation:

```rust
pub fn run(args: FacesArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    if !db.exists() {
        anyhow::bail!("{:?} does not exist", db);
    }
    let conn = videre_core::db::open_wal(&db)?;
    face_db::create_faces_table(&conn)?;

    // ... existing hash-selection logic (unchanged) ...

    if args.recluster || to_process.is_empty() {
        if !args.silent && to_process.is_empty() && !args.recluster {
            eprintln!("All hashes already processed.");
        }
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
```

`format_summary` and `format_clustering_only_summary` are small private
functions in `faces.rs` that assemble the exact strings shown above from
`FacesRunResult`, `Option<ClusteringResult>`, `eps`, and elapsed
`Duration` - kept as plain string-building functions (not part of the
`Progress` module, since summary content is caller-specific, per the
`Progress::finish()` doc comment above) so they're independently unit
testable without needing a database or the ML pipeline.

The existing exit-code behavior (`process::exit(1)` when errors occurred)
extends to cover `detect_errors` in addition to `write_errors`, since a
detection or embedding failure is just as much a partial-failure signal as a
write failure - the previous code only checked `write_errors`, which was an
existing gap this change also closes incidentally.

## `--dry-run` and `--silent` interaction (unchanged behavior, stated for clarity)

- `--dry-run`: detection still runs (bar/summary shown normally), but no
  `replace_faces_for_hash` writes happen and clustering is skipped entirely
  (matches current behavior - `run_clustering` is only called when
  `!args.dry_run`).
- `--silent`: suppresses the bar, the non-TTY periodic lines, and the final
  summary line. Error lines (`detect failed`, `embed_batch failed`, `write
  failed`) still print regardless, matching current unconditional behavior
  and the earlier design decision that `Progress::println` always prints.

## Testing

`crates/videre-ml/src/pipeline.rs`'s existing tests
(`run_face_pipeline_on_empty_input_is_a_noop`,
`run_clustering_on_empty_db_does_not_error`) construct `FacesRunResult`
implicitly via the function's return value and call `run_clustering` with a
`silent` argument that this design removes - both need small updates:

- `run_face_pipeline_on_empty_input_is_a_noop`: add assertions for the two
  new fields (`images_processed: 0, detect_errors: 0`) on the empty-input
  early return.
- `run_clustering_on_empty_db_does_not_error`: update the call site to drop
  the now-removed `silent` argument, and assert the return value is
  `Ok(None)` instead of `Ok(())`.

New tests to add:

- `crates/videre-core/src/progress.rs`: a `#[cfg(test)] mod tests` covering
  `Progress::new`/`tick`/`elapsed` in `Mode::Silent` (constructed with
  `silent: true`) produces no panics and `elapsed()` returns a sane
  non-negative duration; this is the only mode reliably testable without
  mocking a TTY, since `Mode::Bar` vs `Mode::Plain` depends on whether the
  test runner's stderr is a terminal (not controllable in a unit test) - the
  design deliberately keeps `Progress`'s branching on `IsTerminal` untested
  directly and relies on manual verification (see below) for the two
  terminal-dependent code paths.
- `crates/videre/src/commands/faces.rs`: unit tests for `format_summary` and
  `format_clustering_only_summary` covering the three example strings shown
  above (no errors, with errors, no faces found).

Manual verification (not automatable, documented for whoever implements
this): run `videre faces` in a real terminal against a small local test
library and confirm the bar renders and clears cleanly; run
`videre faces > /tmp/out.log 2>&1` (or similar redirection) and confirm the
log file shows periodic `N/total images processed` lines instead of a bar
or per-image spam.

## Out of scope

- `videre embed` and `videre scan`'s own per-file verbosity - not touched by
  this change. `Progress` is written to be reusable by them later, but no
  call site changes happen there in this slice.
- `--json` output for `videre faces` - the command has no `--json` flag
  today and this design doesn't add one; all changes here are to
  human-readable stderr output only.
