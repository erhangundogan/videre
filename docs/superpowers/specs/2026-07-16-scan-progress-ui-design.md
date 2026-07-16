# `videre scan` progress UI Design

## Problem

`videre scan` walks the target directory recursively via `scanner::scan`
(`walkdir`-based, `crates/videre/src/scanner.rs:8`), which returns the full
list of discovered paths before hashing begins - so, unlike `videre faces`
before its progress-UI work, the total file count is known up front. Today
`scan.rs` prints only two announcement lines before hashing
(`"Scanning {dir}..."`, `"Found N file(s) to process"`,
`crates/videre/src/commands/scan.rs:58-66`) and nothing during hashing
itself, which runs as a single `paths.par_iter().filter_map(hasher::hash_file)`
call across all files at once (`scan.rs:69-80`), not the chunked-batch shape
`videre embed` used.

## Goal

Give `videre scan` the same brew/docker/npm-style in-place progress bar
already built for `videre faces`/`videre embed`, ticking once per file as
hashing completes - which, because scan hashes everything in one fully
parallel pass rather than a sequential loop (`faces`) or sequential chunk
loop (`embed`), requires making the shared `Progress` type
(`crates/videre-core/src/progress.rs`) itself safe to call from multiple
`rayon` worker threads concurrently, rather than adding another
single-thread-only method.

Scope: `crates/videre-core/src/progress.rs` (the thread-safety change,
applies to every caller), `crates/videre/src/commands/scan.rs` (the new
bar), plus two one-line mechanical updates to the two existing callers
(`crates/videre-ml/src/pipeline.rs`, `crates/videre/src/commands/embed.rs`)
whose `let mut progress = ...` bindings no longer need `mut` once `tick`/
`tick_by` stop requiring `&mut self`.

## Why `Progress` itself must change, not just gain a new method

`indicatif::ProgressBar` (verified by reading its source directly at
`indicatif-0.17.11/src/progress_bar.rs`) is already `Arc<Mutex<...>>`-backed
internally, and every mutating method used by this module
(`ProgressBar::inc`, `set_position`, `println`) already takes `&self`, not
`&mut self` - it is designed to be cloned and shared across threads with no
external synchronization. The only piece of `Progress` that is not already
thread-safe is its own bookkeeping field, `done: u64`
(`crates/videre-core/src/progress.rs:17`), which `tick`/`tick_by` currently
mutate via `&mut self`. Since `scan.rs` needs to call `tick()` from inside a
`rayon` `.par_iter()` closure - where multiple worker threads run the
closure concurrently and a `Fn` closure (which `par_iter().map`/`filter_map`
require) cannot capture anything by unique (`&mut`) reference - `tick()`
must become callable via a shared reference. This is a change to the module
itself, not an additive method the way `tick_by` was for `videre embed`.

## Changes to `crates/videre-core/src/progress.rs`

Replace `done: u64` with `done: AtomicU64`:

```rust
use indicatif::{ProgressBar, ProgressStyle};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct Progress {
    total: u64,
    done: AtomicU64,
    mode: Mode,
}
```

`Progress::new` constructs `done: AtomicU64::new(0)` instead of `done: 0`
(the only other change to `new`; its `Mode` selection logic is untouched).

Replace `tick` and `tick_by` (currently `progress.rs:52-64` and `:66-84`)
with:

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

`tick` collapses to a one-line call into `tick_by`, removing the near-
duplicate logic the two methods had before (they differed only in whether
`fetch_add` advances by `1` or `n`). `Ordering::Relaxed` is sufficient
because the counter does not need to synchronize any other shared memory -
`indicatif::ProgressBar` already handles its own internal synchronization
independently, and the only invariant `Progress` itself needs is that
concurrent `fetch_add` calls combine correctly, which `Ordering::Relaxed`
guarantees for a single atomic variable.

**Accepted trade-off, stated explicitly (not a bug to fix):** under high
concurrency, two or more threads can each cross a `LOG_INTERVAL` (25)
boundary "at the same time" - each computes its own `before`/`after` pair
from an interleaved `fetch_add`, and if their ranges both happen to contain
a multiple of 25, both will independently decide to print. This can
occasionally produce more than one `Mode::Plain` log line for what is
conceptually a single boundary crossing. This is cosmetic (a duplicate-ish
log line under heavy parallel load), not a correctness bug, and is accepted
rather than fixed with a mutex around the print decision, which would
reintroduce the exact contention this change is meant to avoid, for a
purely cosmetic gain.

`println` and `finish` are unchanged (`println` already took `&self`;
`finish` continues to take `self` by value, since it is still called
exactly once, from the single thread that owns the `Progress` value, after
all parallel work has joined back via `.collect()`).

`Progress` becomes automatically `Sync` as a consequence of this change (all
three fields - `AtomicU64`, `u64`, and `Mode`, whose only non-trivial
variant wraps an already-`Sync` `ProgressBar` - are `Sync`, and Rust
auto-derives `Sync` for structs whose fields are all `Sync`). This is what
lets `scan.rs` capture `&progress` directly in a `rayon` closure with no
`Arc`/`Mutex` wrapping at the call site - no explicit `unsafe impl Sync` is
needed or should be added.

## Changes to the two existing callers

`tick`/`tick_by` no longer require `&mut self`, so the existing
`let mut progress = Progress::new(...)` bindings in both call sites now
trigger an `unused_mut` warning if left as-is. Both go from `mut` to
non-`mut`, no other change:

`crates/videre-ml/src/pipeline.rs`'s `run_face_pipeline`:
```rust
    let mut progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);
```
becomes
```rust
    let progress = videre_core::progress::Progress::new(to_process.len() as u64, silent);
```

`crates/videre/src/commands/embed.rs`'s `run`:
```rust
    let mut progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);
```
becomes
```rust
    let progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);
```

Both files still call `tick()`/`tick_by()` sequentially, one thread at a
time - this change is required only to avoid the new compiler warning
(matching this project's established zero-warnings bar from the two prior
progress-UI slices), not because their behavior needs to change.

## Changes to `crates/videre/src/commands/scan.rs`

Current `gather_records` (`scan.rs:57-93`):

```rust
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
```

Replace with:

```rust
fn gather_records(args: &ScanArgs, directory: &std::path::Path) -> Vec<videre::types::FileRecord> {
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
```

Three deliberate behavior changes, all confirmed in brainstorming:

- The leading `"Scanning {dir}..."` and `"Found N file(s) to process"` lines
  are removed - the bar's own appearance communicates that scanning/hashing
  has started, matching the `faces`/`embed` precedent of not duplicating
  information the bar already shows.
- The `"Warning: skipping {:?}: {}"` message is now **unconditional**
  (previously gated by `if !silent`). It routes through `progress.println`,
  whose documented contract is "always prints, regardless of silent" (see
  `progress.rs`'s doc comment on `println`) - matching the `faces`/`embed`
  precedent that a silently-dropped file is data loss, not routine progress,
  and must stay visible even under `--silent`. This is also a correctness
  requirement, not just a style choice: `progress.println` is specifically
  the bar-safe way to print without corrupting an active `Mode::Bar`'s
  rendering, so a bare `eprintln!` here would risk visual corruption exactly
  when a hash failure happens mid-scan while the bar is up. `--silent` still
  fully suppresses the bar itself (`Mode::Silent`), only this one message's
  own gating is removed.
- `progress.tick()` is called once per file inside the closure, directly on
  the shared `&Progress` captured from the enclosing scope - no `Arc`/
  `Mutex` wrapping needed at this call site, since `Progress` is now `Sync`.

`run_text` (`scan.rs:129-171`) and `run_json` (`scan.rs:176-211`) each have
their own `"Wrote {} record(s) to {:?}"` line after writing succeeds (three
occurrences total: one in `run_text`, two in `run_json`'s two `OutputTarget`
match arms). Each gains a conditional skip-count suffix, computed as
`paths.len() - records.len()` (the discovery-vs-successfully-hashed delta -
no separate counter needed, since both lengths are already available at the
print site). `run_text`'s occurrence:

```rust
            if !args.silent {
                eprintln!("Wrote {} record(s) to {:?}", records.len(), db_path);
            }
```

becomes

```rust
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, "to", &format!("{:?}", db_path)));
            }
```

Rather than duplicate a skip-count-suffix formatting expression at all three
print sites (with three different path-formatting styles - `db_path` is a
`PathBuf` printed via `{:?}`, `path` in the JSONL arm is also `PathBuf` via
`{:?}`), extract one small helper, added near `gather_records`:

```rust
/// Formats the "Wrote N record(s) to <path>" summary line, with an
/// "(M skipped)" suffix when `skipped > 0`, omitted entirely when `skipped
/// == 0` (matching `videre embed`'s equivalent omit-when-zero precedent).
fn format_write_summary(written: usize, skipped: usize, preposition: &str, dest: &str) -> String {
    if skipped > 0 {
        format!("Wrote {written} record(s) {preposition} {dest} ({skipped} skipped)")
    } else {
        format!("Wrote {written} record(s) {preposition} {dest}")
    }
}
```

`gather_records` must return the skip count alongside the records for the
three call sites (`run_text`, and `run_json`'s two arms) to use. Change its
return type from `Vec<videre::types::FileRecord>` to
`(Vec<videre::types::FileRecord>, usize)` (records, skipped count), computed
as `let skipped = paths.len() - records.len();` right after the `.collect()`
call, before the `args.similar` branch (which only transforms `records`, not
the count). Both `run_text` and `run_json` destructure the new return value:
`let (records, skipped) = gather_records(&args, &directory);` (and
`gather_records(args, &directory)` in `run_json`, matching its existing
`&ScanArgs` vs `ScanArgs` parameter difference - unchanged by this task).

Update all three print sites (`run_text`'s one, `run_json`'s two) to call
`format_write_summary(records.len(), skipped, "to", &format!("{:?}", db_path))`
(SQLite arms) or `format_write_summary(records.len(), skipped, "to", &format!("{:?}", path))`
(JSONL arms) - `preposition` is always `"to"` in current usage; it exists as
a parameter only so the function reads naturally as a sentence rather than
hard-coding the word `"to"` inside a format string that already contains
other literal words, keeping the whole line as data-driven format arguments
rather than string concatenation.

## Testing

`crates/videre-core/src/progress.rs`: the existing `#[cfg(test)] mod tests`
block's three tests that currently bind `let mut p = Progress::new(...)`
(`silent_mode_tick_does_not_panic`, `zero_total_does_not_panic`,
`silent_mode_tick_by_does_not_panic`) drop `mut` from their `let p` bindings
(matching the `tick`/`tick_by` signature change - `mut` is no longer needed
and would otherwise trigger `unused_mut`). `silent_mode_println_still_prints`
already binds without `mut` and needs no change.

One new test, verifying the actual thread-safety property this task adds
(not "doesn't panic" theater - it asserts the exact final count after real
concurrent access, which only holds if the atomic swap is correct):

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

This test can read `progress.done` directly (a private field) because
`mod tests` is declared inside `progress.rs` itself, and Rust module
privacy allows a child module to see its parent module's private items -
no public getter needs to be added to `Progress` just to make this
assertion possible.

`crates/videre/src/commands/scan.rs` currently has no `#[cfg(test)] mod
tests` block and none is added by this task - `format_write_summary` is a
pure function and a natural candidate for unit tests, but `scan.rs`'s
existing test coverage lives entirely in the separate integration-test
binary `crates/videre/tests/scan.rs` (which spawns the real built binary
and asserts on file/stdout/stderr contents), not in an in-file unit-test
module, and this task follows that established per-file convention rather
than introducing a new one. See the integration-test changes below instead.

`crates/videre/tests/scan.rs` (the existing integration-test binary):
- No existing test asserts on the now-removed `"Scanning "`/`"Found "`
  stderr text (confirmed: `grep -n "\"Scanning\|Found {}" crates/videre/tests/scan.rs`
  returns no matches), so no test needs updating for their removal.
- Any existing test asserting on the old exact `"Wrote {N} record(s) to
  {:?}"` text (no skip-count suffix) still passes unchanged in the
  zero-skipped case, since the suffix is omitted when `skipped == 0` - no
  update needed for tests that don't trigger hash failures.
- One new test is warranted: scan a directory containing a broken symlink
  (created via `std::os::unix::fs::symlink("/nonexistent/target", scan_dir.path().join("broken.jpg"))`)
  alongside one valid file. `hasher::hash_file` fails deterministically on a
  broken symlink (`hasher.rs:114`'s `File::open(path)?` fails to open a
  symlink whose target does not exist), and this is fully portable for this
  project's actual target platforms - confirmed via `README.md`/`CLAUDE.md`
  containing no Windows-specific code paths or CI targets anywhere (this
  project already assumes macOS/Linux only, per its existing `qlmanage`/
  HEIC handling). No existing test in `crates/videre/tests/scan.rs`
  currently constructs a hash-failure case (confirmed by inspection - all
  existing tests scan only valid files), so this is new coverage, not a
  duplicate of anything already tested. Assert the final "Wrote" line
  includes `"(1 skipped)"`.

Manual verification (not automatable, same reasoning as the `faces`/`embed`
progress-UI specs): running `videre scan <a large directory>` in a real
terminal to confirm the bar renders in place and updates smoothly (not in
chunky jumps, since `scan` ticks per file, unlike `embed`'s per-chunk
`tick_by`) and clears cleanly before the final summary; running it piped to
a file to confirm the non-TTY fallback shows periodic `N/total images
processed` lines.

## Out of scope

- `videre watch --scan`'s scan stage (`crates/videre/src/commands/watch.rs`'s
  `run_scan_stage`, `watch.rs:265`) is confirmed unaffected by this change:
  it calls `scanner::scan`/`hasher::hash_file` directly and does not import
  anything from `scan.rs` or call `gather_records` (verified by grepping
  `watch.rs` for both - no match). No change needed there; giving `watch
  --scan` the same bar treatment is a separate, not-requested piece of
  future work, consistent with `videre embed`'s design spec explicitly
  scoping out `watch`'s other stages for the same reason.
- `--json` output for `videre scan` (`ScanJson`) is unaffected - it already
  reports `total_files: records.len()` and does not print human-readable
  summary lines at all in JSON mode (those are gated by `!args.silent`
  `eprintln!` calls that fire regardless of `--json`, since JSON mode's
  `run_json` reuses the same `gather_records`/write-summary path as text
  mode for its stderr progress reporting - only stdout differs). No new
  fields are added to `ScanJson`.
