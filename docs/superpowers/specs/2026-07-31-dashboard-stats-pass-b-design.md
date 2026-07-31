# Dashboard stats backend - Pass B: pipeline run tracking

## Status

Design approved via chat brainstorming 2026-07-31, following a fresh design
round after a high-effort agent review found the original Pass B design (in
`docs/superpowers/specs/2026-07-31-dashboard-stats-backend-design.md`'s "Pass B"
section) unsound. This document supersedes that section entirely - the original
is left in place as a record of what was tried and rejected, and why.

Pass A (library aggregate stats: totals, photo/video split, duplicate counts,
faces/people counts) already shipped and is merged to `main`. Still backend
only - no React/UI work in this pass.

## Problem

The dashboard needs to answer, per pipeline command: when did it last run, did
it succeed, how long did it take, and is it running right now. Nothing in the
schema records this today - `dedupe` doesn't write to the db at all, and
`fix-dates` only touches file mtimes.

## Why the original design failed (do not re-attempt without reading this)

1. `videre watch` reimplements `scan`/`faces` logic inline
   (`run_scan_stage`/`run_faces_stage` in `crates/videre/src/commands/watch.rs`
   call `scanner::scan`/`hasher`/`sqlite_output::write_records` and
   `run_face_pipeline`/`run_clustering` directly) rather than calling
   `commands::scan::run`/`commands::faces::run`. Instrumenting only the
   standalone commands would leave "last scan/faces run" permanently stale for
   the primary always-on workflow.
2. `std::process::exit` calls in `scan.rs`/`faces.rs`/`fix_dates.rs` run no
   destructors, defeating an RAII `Drop`-based guard on every failure path.
3. `videre scan` has no resolved db path in JSONL mode, and (in SQLite mode)
   doesn't touch the db until deep inside `sqlite_output::write_records`.
4. PID-liveness (`libc::kill(pid, 0)`) has PID-reuse and TOCTOU races.
5. Reconciling a stale lock to a persisted `crashed` status from a *read* path
   breaks the read-only assumptions of `videre mcp` and the Tauri app's shared
   `Mutex<Connection>`.
6. Explicit `--db` paths aren't canonicalized, so relative/absolute/symlink
   variants of the same database would derive different lock paths.
7. Concurrent runs of the same command were entirely unmodeled.
8. A normal Ctrl-C (a documented, supported way to stop `videre faces`) would
   misreport as a crash.

## Decisions made this round

- **Concurrency:** a second invocation of a command already running against the
  same database is refused immediately with a clear error, not allowed to race.
- **Ctrl-C:** `scan` and `faces` install a SIGINT handler so a deliberate
  interrupt is recorded as `interrupted`, distinct from a real crash.
- **Scope:** all six pipeline commands are wired in this pass -
  `scan`, `faces`, `embed`, `classify`, `dedupe`, `fix-dates` - not just
  scan/faces. All five of the non-scan/faces commands already resolve an
  existing db path via `resolve_reader_db` before any work starts (confirmed by
  reading `embed.rs`), which makes them simpler to wire than scan/faces.

## Key design insight: no RAII, no Drop-vs-`process::exit` conflict

The original design needed `Drop` to run reliably, and `process::exit` breaks
that. The fix is to not rely on `Drop` for the normal completion path at all.
Reading the actual failure-handling code in this project confirms every
`process::exit(1)` call is a *sequential check after the operation already
returned*, never inside it:

- `faces.rs`: `let result = run_face_pipeline(...)?; ... if result.write_errors > 0 || result.detect_errors > 0 { process::exit(1); }`
- `fix_dates.rs`: the update loop runs to completion, accumulating an `errors`
  counter, prints a summary, *then* `if errors > 0 { process::exit(1); }`

So a tracking function that wraps just the operation and finalizes the
`pipeline_runs` row **before returning its result unchanged** always completes
before any later `process::exit` call - no destructor dependency needed for the
success/failure bookkeeping. Only an actual mid-operation crash (`SIGKILL`,
power loss) skips finalization, which is exactly the case the lock-based
crash-detection below is for.

## Schema

```sql
CREATE TABLE IF NOT EXISTS pipeline_runs (
    command      TEXT PRIMARY KEY,
    started_at   TEXT NOT NULL,
    finished_at  TEXT,
    status       TEXT NOT NULL,   -- 'running' | 'success' | 'failed' | 'interrupted'
    duration_ms  INTEGER,
    summary      TEXT             -- error message on failure; NULL otherwise
);
```

`'crashed'` is never a stored value - it is computed only when reading: a row
with `status = 'running'` whose lock is not currently held is displayed as
`crashed`, without writing anything back. This keeps every read path (`videre
mcp`, the Tauri app) genuinely read-only.

Created idempotently via `ensure_pipeline_runs_table`, called from inside
`track()` (see below) - matches the existing `ensure_classifications_table`/
`create_faces_table` pattern of writer-driven, idempotent table creation.

## Locking

An OS advisory lock (`flock`, via the `fs2` crate - `File::try_lock_exclusive`/
`unlock`) replaces PID-checking entirely. Properties that make this correct
where the PID approach wasn't:

- The kernel releases the lock automatically on **any** process death - normal
  return, `process::exit`, panic, or `SIGKILL` - so there is no PID-reuse race
  and no window where a dead process's lock looks ambiguous.
- Acquisition is atomic (`try_lock_exclusive` either succeeds or fails outright),
  so "is it held" and "acquire it" are one syscall - no TOCTOU gap.
- POSIX `flock` locks are associated with the *open file description*, not the
  owning process - two independent `File::open` calls on the same path
  conflict even from within the same process, which is what makes this testable
  without spawning a real second process (see Testing below).

**Lock path:** `fs::canonicalize(db_path)` with `.{command}.lock` appended,
placed next to the db file - e.g. `photos.db.faces.lock` beside `photos.db`.
Canonicalizing is valid for every command in this design because the db file is
guaranteed to already exist by the time `track()` runs: the five
`resolve_reader_db`-based commands already require it, and `scan`'s own
connection-open (see below) creates it first.

**Accepted behavior change for `scan`:** to start tracking *before* work begins
(needed for the lock and the `running` row), something must open a connection
to the target db - and for a first-ever `scan` in SQLite mode, that open is
what creates the (still-empty) db file, slightly earlier than today's lazy
creation inside `sqlite_output::write_records`. This does not violate the
"readers never create a database" invariant - that invariant is about *read*
commands never fabricating a database from nothing; `scan` is the one command
whose entire purpose is populating this exact path. It is arguably an
improvement: today, a `scan` that crashes 90% through a large library leaves no
signal anything went wrong; with `pipeline_runs`, that crash is now visible
(`status = 'running'` -> displayed as `crashed`) even though `file_hashes` has
partial data. `scan --output` (JSONL-only, no database at all) is simply never
tracked - there is no db path to track against.

## The `track` function

```rust
pub fn track<T>(
    conn: &Connection,
    db_path: &Path,   // already canonicalized by the caller's db resolution
    command: &str,
    f: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    ensure_pipeline_runs_table(conn)?;
    let _lock = acquire_lock(db_path, command)?;  // Err(AlreadyRunning { started_at }) if held
    start_run(conn, command)?;                     // upsert: status='running', started_at=now
    let started = std::time::Instant::now();
    let result = f();
    let duration_ms = started.elapsed().as_millis() as i64;
    match &result {
        Ok(_) => finish_run(conn, command, "success", duration_ms, None)?,
        Err(e) => finish_run(conn, command, "failed", duration_ms, Some(&e.to_string()))?,
    }
    result   // _lock drops here; unchanged from the perspective of any caller-side process::exit check
}
```

`acquire_lock` returns `Err` immediately (refusing the run, per the concurrency
decision) if the lock is already held by a live process - this is the natural
place the "second run refused" behavior lives, with no separate check needed.

## Wiring per command

| Command | Call site(s) | Notes |
|---|---|---|
| `scan` | `commands::scan::run` (SQLite-writing branch only) + `watch::run_scan_stage` | JSONL-only mode never tracked |
| `faces` | `commands::faces::run` + `watch::run_faces_stage` | `--dry-run` still tracked - it's a real completed run |
| `embed` | `commands::embed::run` | single call site; already resolves an existing db |
| `classify` | `commands::classify::run` | single call site |
| `dedupe` | `commands::dedupe::run` | single call site - tracks that dedupe ran even though it writes no `file_hashes` changes |
| `fix-dates` | `commands::fix_dates::run` | single call site |
| `videre watch` (the process itself) | acquires `<db>.watch.lock` directly, not via `track()` | liveness only - no `pipeline_runs` row, since watch has no "finished" moment during normal operation |

Each `scan`/`faces` wiring is two thin call sites around each implementation's
*existing* logic - not a unification of the CLI and `watch` code paths. Unifying
them was considered and rejected for this pass: it would touch core scan/face
business logic in both places (a materially bigger, riskier change) for a
problem that duplicated thin tracking calls already solve.

## Ctrl-C handling

Only `scan` and `faces` install a SIGINT handler (via the `ctrlc` crate),
since those are the two commands documented as normal to interrupt. The handler
closure captures the db path and command name, and on SIGINT:

1. Opens a fresh, short-lived connection to the same db (the main thread's
   connection isn't safely shareable across the handler boundary).
2. Reads the row's existing `started_at` (already written by `start_run` before
   the operation began) and computes `duration_ms` as `now - started_at`, then
   writes `status = 'interrupted'` via the same `finish_run` used by `track()`.
3. Calls `std::process::exit(130)` (the standard SIGINT exit code).

The `flock` releases as part of normal process teardown regardless of which
thread triggers the exit, so no separate lock-release step is needed in the
handler.

## Reading status

```rust
pub struct PipelineRunStatus {
    pub command: String,
    pub last_run_at: Option<String>,
    pub status: Option<String>,       // running | success | failed | interrupted | crashed | None if never run
    pub duration_ms: Option<i64>,
    pub currently_running: bool,
}

pub fn read_all(conn: &Connection, db_path: &Path) -> Result<Vec<PipelineRunStatus>>;
```

For each of the six commands: read its `pipeline_runs` row (if any), then
non-blocking-probe its lock (`try_lock_exclusive` then immediately release if
acquired) to determine `currently_running`. If the stored `status` is
`'running'` but the probe shows the lock is free, the returned `status` is
`"crashed"` - computed in the return value only, never written back to the row.

This is deliberately kept as its own module/facade function
(`videre_core::pipeline_runs::read_all`, and a corresponding
`videre_api::pipeline_status()` facade + Tauri command) rather than folded into
Pass A's `LibraryStats` - it performs a lock-probe syscall per command (a
different cost profile than a single aggregate query) and returns a list
shaped around "one entry per command," not a flat struct. Follows the same
three-layer pattern as Pass A: `videre-core` module -> `videre-api` facade ->
thin Tauri command.

## Non-goals (this pass)

- No UI/React work.
- No unification of `scan`/`faces` business logic between the CLI and `videre
  watch` - only the tracking call sites are duplicated, not the underlying
  implementations.
- No multi-db switcher (unaffected by this design either way - locks are
  per-db by construction via the canonicalized db path).
- No richer `summary` content beyond the raw error message on failure - no
  per-command structured summaries (e.g. "1,204 duplicates found") in this pass.
- `videre mcp`'s `stats` tool is not wired to `pipeline_status` in this pass,
  for the same reason Pass A didn't touch it: a separate decision about growing
  its documented JSON contract, not made here.

## Testing

- `videre-core`: unit tests for `track()` covering success, failure (status
  recorded, error message captured), and duration bookkeeping, using an
  in-memory db and a closure that returns `Ok`/`Err`.
- Lock-contention test: two independent `File::open`+`try_lock_exclusive` calls
  on the same temp path, in the same test process, asserting the second fails
  while the first is held and succeeds after it's released - validates
  "second run refused" without spawning a real process.
- `read_all` tests: a row with `status='running'` and no held lock reports
  `crashed`; a row with `status='running'` and a held lock (acquired by the
  test itself before calling `read_all`) reports `running`/`currently_running:
  true`; a command with no row at all reports `status: None`.
- `videre` integration tests (`crates/videre/tests/`): running `fix-dates`
  (the simplest single-call-site command) twice writes a `pipeline_runs` row
  each time; simulate a crash by pre-seeding a `running` row with no
  corresponding lock held and confirm the next `read_all` reports `crashed` for
  it without modifying the row (assert the row is unchanged in the db after the
  read).
- No dedicated SIGINT-handler test - signal delivery in an automated test
  harness is inherently flaky; this is verified manually per the manual
  verification step in the eventual implementation plan.
