# Dashboard stats backend (first slice)

## Status

Design approved via chat brainstorming 2026-07-31, then reviewed by a separate
high-effort agent review against the real codebase. The review found the
`library_stats` aggregate-queries half sound and ready, but the `pipeline_runs` +
sidecar-lockfile half unsound as originally specified (concrete evidence below).
Revised 2026-07-31: split into **Pass A** (ready to plan/implement now) and
**Pass B** (redesign required - open problems documented, not yet a plan).

Still backend only - no React/UI work in this pass, per explicit user
instruction (UI comes later, once the user supplies their own design/components,
likely sourced from shadcnblocks.com dashboard blocks / chart components).

## Problem

The Tauri desktop app (`app/`) will eventually show a Home dashboard with a long
list of widgets (scan/dedupe/faces status, totals, categories, geography, date
clustering, etc.). That list was deliberately decomposed: this pass builds the
backend data layer for a first slice only - total files/size, photo/video split,
duplicate group/file counts + wasted bytes, faces detected, people named (Pass A);
scan/faces run status (last run, success/failure, duration, currently-running)
was originally scoped into this slice too but is deferred to Pass B pending a
redesign (see below).

Categories, geography, date clustering, and further `pipeline_runs` commands
(embed/classify/dedupe/fix-dates) are out of scope for both passes here and
remain deferred to future slices.

A second, narrower problem: `videre mcp`'s existing `build_stats()`/`StatsJson`
(`crates/videre/src/commands/mcp.rs:104-`) and `videre report`'s `query_stats`
(`crates/videre/src/commands/report.rs:345-371`) both already compute pieces of
this - two independent implementations already exist in the binary crate, both
invisible to `app/src-tauri`. Building a third for the Tauri app without
consolidating would make it four copies over time. Pass A consolidates the
aggregate-query logic; it does not yet touch `build_stats`'s output shape (see
"MCP integration" below - deferred to avoid an unrelated breaking change).

## Non-goals (both passes)

- No React/UI/component code.
- No multi-db switcher UI (the data model stays multi-db-safe for `library_stats`
  since it's a pure per-connection query; the lockfile multi-db story is part of
  what's unresolved in Pass B - see below).
- No categories/geography/date-clustering widgets.
- No CLI-facing `videre stats` subcommand.

---

## Pass A - library aggregate stats (ready now)

### Architecture

Same facade pattern already used for faces-labeling (`videre-api`, called
identically by both the axum server and the Tauri app):

```
videre-core::library_stats   (new module: pure queries over &Connection)
        |
        v
videre-api::library_stats()  (new facade fn, mirrors classify.rs/face_db.rs style)
        |
        +---> app/src-tauri/src/commands.rs: new `library_stats` Tauri command
               (thin wrapper, same pattern as every existing command)
```

`videre-core` for the query logic, `videre-api` for the response-shaped facade -
matches `embeddings.rs`/`face_db.rs`/`classify.rs` exactly: `pub fn f(conn: &Connection, ...) -> rusqlite::Result<T>`,
`CREATE TABLE IF NOT EXISTS` guards where relevant, inline `#[cfg(test)]` modules
over `Connection::open_in_memory()` (confirmed as the actual existing pattern in
`videre-api` - it has no `tests/` dir; `label.rs`, `faces.rs`, `images.rs` all use
inline test modules, so `library_stats` should too, not new test infrastructure).

**Not wired into `videre mcp` in this pass.** The review found `StatsJson`
(`mcp.rs:75-87`: `unique_hashes`, `embedded_count`, `people: Vec<String>`,
`files_with_gps`, `exif_date_range`) and the proposed `LibraryStats` only overlap
on `total_files`/`total_size_bytes`/faces count - refactoring `build_stats` to
call `library_stats` would either change the MCP `stats` tool's documented
output shape (asserted by `crates/videre/tests/mcp.rs:142-190`, part of the
shared `schema_version: 1` contract described in CLAUDE.md) or require
`LibraryStats` to be a strict superset of `StatsJson`, which is a separate design
decision this pass doesn't make. `build_stats` keeps its own implementation for
now; consolidating it is left as a follow-up once someone decides whether the
MCP contract is allowed to grow to match, or whether `library_stats` should grow
to match it instead.

### LibraryStats fields

```rust
pub struct LibraryStats {
    pub total_files: i64,
    pub total_size_bytes: i64,
    pub total_photos: i64,
    pub total_videos: i64,
    pub duplicate_group_count: i64,
    pub duplicate_file_count: i64,   // files beyond the first per group
    pub wasted_bytes: i64,
    pub faces_detected: i64,
    pub people_named: i64,           // DISTINCT confirmed person_label
}
```

Query notes:

- **Duplicate counts / wasted bytes**: reuse the exact query shape already in
  `videre report`'s `query_stats` (`report.rs:345-371`) - `GROUP BY hash HAVING
  COUNT(*) > 1`, no on-disk existence filtering, no reuse of dedupe's KEEP/REMOVE
  tie-break logic. The review verified this is numerically identical to what
  `videre dedupe` reports today, since `find_duplicate_groups` only *sorts*
  within a group by `best_date` and does no filtering that would change group
  membership or counts. This becomes the **second** call site for this exact SQL
  (report.rs has the first); moving it into `videre-core::library_stats` and
  having `report.rs::query_stats` call the shared function too is in scope for
  this pass (removes a duplicate rather than adding a third copy).
- `wasted_bytes` uses `COALESCE(SUM(size_bytes * (cnt - 1)), 0)`, same as
  `report.rs` - rows with `NULL size_bytes` are silently excluded from the sum
  (matches existing behavior; not being fixed here, just not made worse).
- **Photo/video split**: `ext` is stored lowercased with **no leading dot**
  (`hasher.rs:143-147` - `"jpg"`, `"mp4"`, not `".jpg"`/`".mp4"`; confirmed by
  reading the actual extraction code, not assumed). Video = `ext IN ('mov',
  'mp4')`; photo = everything else in the supported list. Rows with `NULL` or
  empty `ext` are counted in neither bucket, so `total_photos + total_videos` is
  not guaranteed to equal `total_files` - that's an accepted, documented gap, not
  a bug to chase in this pass.
- **Totals**: `total_files`/`total_size_bytes` count `file_hashes` rows, which
  include files already deleted from disk until `videre prune` runs (documented
  divergence - `report.rs`'s `--all` gallery explicitly filters `Path::exists()`;
  MCP's existing `stats` tool does not). `library_stats` matches the *existing*
  MCP behavior (row counts, not disk-existence-filtered) rather than introducing
  a third convention - noted here as a conscious choice, not an oversight.
- `faces_detected`: `COUNT(*) FROM faces`, guarded by `table_exists` (see below).
- `people_named`: `COUNT(DISTINCT person_label) FROM faces WHERE confirmed = 1
  AND person_label IS NOT NULL`, same guard.

### Table-existence guarding

`crates/videre-core/src/db.rs` is currently just `open_wal` - there is no
guarantee `faces`/`classifications`/etc. exist on a connection that hasn't run
the relevant writer yet. `mcp.rs`'s `build_stats` already handles this with a
`table_exists` helper (`mcp.rs:95-102`), tested explicitly (`tests/mcp.rs:165`,
"stats must return zero counts without optional tables"). `library_stats` must
use the same guard pattern for `faces`-dependent fields (return 0, not an error,
when the table doesn't exist yet) - `file_hashes` itself is always assumed
present per every other reader's convention.

### New dependency

`videre-core`'s `Cargo.toml` currently has no `serde`/`serde_json` (deps are:
anyhow, half, image, indicatif, reverse_geocoder, rusqlite, toml). `LibraryStats`
needs to be `Serialize` to flow through the Tauri command (every existing
Tauri-returned type is a `videre-api` serde type). Add `serde` (with `derive`)
to `videre-core` - `videre-api` already depends on it, so this isn't a new
transitive dependency for the workspace, just a new direct one for `videre-core`.

### Wiring

- `app/src-tauri/src/commands.rs`: new `library_stats` command, `db.0.lock()`
  then delegate - identical shape to every existing command
  (`faces_list`/`cluster_detail`/etc., `commands.rs:9-80`).
- `app/src-tauri/src/state.rs`: no changes - reuses the existing `DbState`
  connection.

### Testing

- `videre-core`: inline `#[cfg(test)]` in `library_stats.rs`, seeding a temp
  in-memory db with `file_hashes`/`faces` rows, asserting each field including
  the zero-tables case.
- `videre-api`: inline test confirming the facade returns the same values
  `videre-core` produces (matches `label.rs`/`faces.rs`/`images.rs` style -
  no new test infrastructure).
- `report.rs`: existing tests for `query_stats` continue to pass unchanged once
  it's rewired to call the shared `videre-core` function (behavior-preserving
  refactor, not a behavior change).

---

## Pass B - pipeline run tracking (redesign required, NOT ready to plan)

The original spec proposed a `pipeline_runs` table + sidecar lockfiles
(`<db_path>.<command>.lock`) wired into `scan`/`faces` via an RAII `Drop` guard,
to answer "when did scan/faces last run, did it succeed, is it running now."
The high-effort review found this unsound as specified. Recorded here so the
next design pass starts from the real problems instead of re-discovering them.

### Why the original design doesn't work

1. **`videre watch` bypasses the instrumented entry points entirely.**
   `crates/videre/src/commands/watch.rs`'s `run_scan_stage` and
   `run_faces_stage` call `scanner::scan`/`hasher::hash_file`/
   `sqlite_output::write_records` and `run_face_pipeline`/`run_clustering`
   directly - not `commands::scan::run`/`commands::faces::run`. Since `videre
   watch` is the documented way to keep a library fresh in the background, and
   the most likely process running when someone looks at a dashboard,
   instrumenting only the standalone CLI commands means "last scan/faces run"
   would be stale or absent for the primary workflow this feature exists to
   surface. Any redesign must instrument whatever `scan`/`faces`/`watch` all
   actually share (or instrument all three call sites explicitly, with an
   explicit decision on whether a 300s-interval watch loop rewrites the row
   every single cycle).
2. **`std::process::exit` defeats an RAII guard on every failure path that
   matters.** `faces.rs:157-159` calls `std::process::exit(1)` on detect/write
   errors; `scan.rs` has five `process::exit(1)` call sites across both text and
   `--json` error paths. `std::process::exit` runs no destructors, so a `Drop`-
   based guard never fires on exactly the runs that failed - every real failure
   would misreport as `crashed` (via stale-lock detection) rather than `failed`,
   collapsing two of the four intended status values into one. Fixing this means
   either converting those exit sites to normal error returns first (a real,
   separate change with observable exit-code/stderr behavior to preserve) or
   choosing a mechanism that doesn't depend on destructors running.
3. **`videre scan` doesn't have a resolved db path at the point instrumentation
   would need one.** `--output`/bare `--output` is JSONL-only with no database
   at all; even in default SQLite mode, `output_target()` runs after
   `gather_records()`, and the db file/tables are created lazily inside
   `sqlite_output::write_records`. Writing a `pipeline_runs` row and a sidecar
   lock "before work starts" doesn't have anywhere to go in JSONL mode, and in
   SQLite mode would create a db file before any scan work happens - which
   interacts badly with the existing "readers never create a database; a
   missing db means 'never scanned'" invariant multiple readers rely on
   (`commands/mod.rs`, `state.rs`).
4. **PID-liveness (`libc::kill(pid, 0)`) is the wrong primitive.** PID reuse
   makes a stale lock from a recycled PID report "running" forever with no
   recovery path; the check-then-delete-then-write sequence is a TOCTOU race
   between two processes that could both observe "stale" and both proceed;
   `kill` semantics differ for another user's process vs. a zombie. An OS
   advisory lock (`flock`/`fcntl`) on the sidecar file is the right tool - the
   kernel releases it automatically on process death, *including* `SIGKILL` and
   `std::process::exit` (which sidesteps problem 2 as well), and acquisition is
   atomic so "is it held" and "acquire it" collapse into one syscall.
5. **Reconciliation-as-a-write breaks read-only assumptions.** The original
   design has a *read* path (stats lookup) perform a *write* (delete stale lock,
   update the row to `crashed`) - reached from `videre mcp`, which advertises
   itself as read-only, and from the Tauri app's single shared
   `Mutex<Connection>`. Needs either a display-time-only "looks crashed"
   computation with no persistence, or confining any reconciling write to a
   writer-side path.
6. **Explicit `--db` paths aren't canonicalized.** `videre_core::home::resolve_db`
   returns explicit `--db` paths verbatim (`home.rs:71-76`). A relative path, an
   absolute path, and a symlink can all name the same database file but would
   each derive a different sidecar lock path - defeating the multi-db-safety the
   original design claimed. Fixing this needs canonicalization before deriving
   the lock path, which in turn needs the file to already exist
   (`fs::canonicalize` requires it) - collides with problem 3 for a first-ever
   scan.
7. **No defined behavior for two concurrent runs of the same command.** With
   `command TEXT PRIMARY KEY` (one row, not a log), a second run overwriting the
   lock and row while the first is still in flight means the *first* run's
   completion (whichever finishes first) silently overwrites what the *second*
   run is doing - undefined, not just unhandled.
8. **Ordinary Ctrl-C would misreport as a crash.** CLAUDE.md documents
   interrupting `videre faces` as a normal, supported way to stop it ("an
   interrupt loses at most the in-flight image and a rerun continues where it
   left off"). Default `SIGINT` handling terminates without unwinding - no
   `Drop`, so a leaked lock, so the next read reports `crashed` for what was a
   deliberate, successful-by-design interruption.

### What to resolve before Pass B can be planned

- Where should run-instrumentation actually live so `scan`/`faces` standalone
  invocations *and* `videre watch`'s internal stages share one code path,
  without watch rewriting the row every 300s cycle in a way that makes "last
  run" meaningless.
- Whether to convert `scan.rs`/`faces.rs`'s `process::exit` call sites to normal
  `Result` returns (and what that changes for exit codes/stderr output that
  existing tests may assert on), or to find a tracking mechanism that survives
  `process::exit` without needing that refactor.
- Replace PID-liveness with an OS advisory lock (`flock`/`fcntl`) as the
  liveness/crash-detection primitive.
- Decide whether "crashed" reconciliation is ever persisted, or purely computed
  at read time with no write from a read path.
- Canonicalize `--db` paths before any path-derived lock naming, and decide what
  happens on a first-ever scan where the db doesn't exist yet to canonicalize.
- Decide concurrent-run semantics explicitly: refuse a second run, or accept
  last-writer-wins and say so.
- Decide how (or whether) a normal Ctrl-C is distinguished from a real crash.

This section is intentionally a problem list, not a design - the next pass
should treat it as the starting brief for a fresh design/brainstorming round,
not as a plan to execute.
