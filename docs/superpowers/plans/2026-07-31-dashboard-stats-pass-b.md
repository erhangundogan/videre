# Dashboard Stats Backend (Pass B: Pipeline Run Tracking) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Track per-command run history (last run time, success/failure/interrupted, duration, currently-running) for `scan`, `faces`, `embed`, `classify`, `dedupe`, and `fix-dates`, using a `pipeline_runs` table plus per-db `flock` sidecar lockfiles, and expose it to the Tauri desktop app.

**Architecture:** A new `videre_core::pipeline_runs` module provides `track()` (wraps an operation, records success/failure, refuses concurrent runs via a non-blocking OS advisory lock) and `read_all()` (reads status, computing a `crashed` label at read time when a `running` row's lock isn't actually held - never persisting that label). `track()` is called at each command's existing call site - twice for `scan`/`faces` (once standalone, once from inside `videre watch`'s matching stage), once each for the other four. `videre watch` itself takes its own lock for liveness only, with no `pipeline_runs` row. A `videre-api::pipeline_status()` facade and a thin Tauri command expose `read_all()`'s result, following the exact three-layer pattern Pass A established.

**Tech Stack:** Rust, `rusqlite`, the new `fs2` crate (`flock`-based advisory file locking) in `videre-core`, the new `ctrlc` crate (SIGINT handling) in `videre`, Tauri v2 commands.

**Read first:** `docs/superpowers/specs/2026-07-31-dashboard-stats-pass-b-design.md` - especially the "why the original design failed" and "key design insight" sections, which explain why this plan does *not* use RAII/`Drop` for the success/failure bookkeeping (only for tidiness) and why every `process::exit` call site was individually checked before deciding whether it needed restructuring.

**Out of scope (do not implement):** any React/UI work; unifying `scan`/`faces` business logic between the CLI and `videre watch` (only the tracking call is duplicated, not the underlying implementations); richer per-command `summary` content beyond the raw error message; wiring `videre mcp`'s `stats` tool to `pipeline_status`.

---

## File Structure

- Modify: `crates/videre-core/Cargo.toml` - add `fs2`
- Modify: `crates/videre/Cargo.toml` - add `ctrlc`
- Create: `crates/videre-core/src/pipeline_runs.rs` - table, locking, `track()`, `read_all()`, `PipelineRunStatus`
- Modify: `crates/videre-core/src/lib.rs` - register the module
- Modify: `crates/videre/src/commands/fix_dates.rs` - wire `track()`
- Modify: `crates/videre/src/commands/embed.rs` - wire `track()`
- Modify: `crates/videre/src/commands/classify.rs` - wire `track()`
- Modify: `crates/videre/src/commands/dedupe.rs` - restructure the `load_records` failure path, wire `track()` in both `run_text`/`run_json`
- Modify: `crates/videre/src/commands/scan.rs` - restructure `run_text`'s SQLite-write failure path, wire `track()` in both SQLite branches (text + json), install SIGINT handler
- Modify: `crates/videre/src/commands/faces.rs` - wire `track()`, install SIGINT handler
- Modify: `crates/videre/src/commands/watch.rs` - wire `track()` into `run_scan_stage`/`run_faces_stage`, add `videre_watch` liveness lock held for the loop's life
- Create: `crates/videre-api/src/pipeline_status.rs` - facade
- Modify: `crates/videre-api/src/lib.rs` - register and re-export
- Modify: `app/src-tauri/src/commands.rs` - new `pipeline_status` Tauri command
- Modify: `app/src-tauri/src/lib.rs` - register the command
- Create: `crates/videre/tests/pipeline_runs.rs` - integration coverage across the wired commands

---

### Task 1: Add `fs2` dependency

**Files:**
- Modify: `crates/videre-core/Cargo.toml`

`ctrlc` and `chrono` are added later, in Task 14, to the crate that actually calls them (`videre-core`, where `install_sigint_handler` is defined) - adding them here would just be premature.

- [ ] **Step 1: Add `fs2` to `videre-core`**

```toml
[dependencies]
anyhow = "1"
half = "2"
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "gif", "webp", "bmp", "tiff"] }
indicatif = "0.17"
reverse_geocoder = "4"
rusqlite = { version = "0.32", features = ["bundled"] }
toml = { version = "0.8", features = ["preserve_order"] }
serde = { version = "1", features = ["derive"] }
fs2 = "0.4"
```

- [ ] **Step 2: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: builds cleanly (the dependency isn't used by any code yet).

- [ ] **Step 3: Commit**

```bash
git add crates/videre-core/Cargo.toml Cargo.lock
git commit -m "chore: add fs2 dependency for pipeline run tracking"
```

---

### Task 2: `pipeline_runs` table + `start_run`/`finish_run`

**Files:**
- Create: `crates/videre-core/src/pipeline_runs.rs`
- Modify: `crates/videre-core/src/lib.rs`

- [ ] **Step 1: Register the module**

In `crates/videre-core/src/lib.rs`, add in alphabetical position:

```rust
pub mod classify;
pub mod db;
pub mod embeddings;
pub mod face_cluster;
pub mod face_db;
pub mod heic;
pub mod home;
pub mod io_timeout;
pub mod library_stats;
pub mod pipeline_runs;
pub mod semaphore;
pub mod location;
pub mod person_search;
pub mod progress;
pub mod thumb_cache;
pub mod vectors;
```

- [ ] **Step 2: Write the module with a failing test**

Create `crates/videre-core/src/pipeline_runs.rs`:

```rust
//! Per-command pipeline run history and liveness, for the desktop app's home
//! dashboard. See docs/superpowers/specs/2026-07-31-dashboard-stats-pass-b-design.md
//! for the full design, and in particular why `track()` below does not rely
//! on Drop/RAII for the success/failure bookkeeping - only the lock's
//! release does, and even that is backstopped by the OS releasing `flock` on
//! any process death.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// The six commands tracked in this pass. `videre watch` itself is
/// deliberately not in this list - it has no "finished" moment during normal
/// operation, so it gets its own liveness lock (see `watch_lock_path`) but no
/// `pipeline_runs` row.
pub const TRACKED_COMMANDS: [&str; 6] =
    ["scan", "faces", "embed", "classify", "dedupe", "fix-dates"];

pub fn ensure_pipeline_runs_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pipeline_runs (
            command      TEXT PRIMARY KEY,
            started_at   TEXT NOT NULL,
            finished_at  TEXT,
            status       TEXT NOT NULL,
            duration_ms  INTEGER,
            summary      TEXT
        );",
    )
}

pub fn start_run(conn: &Connection, command: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO pipeline_runs (command, started_at, status)
         VALUES (?1, datetime('now'), 'running')
         ON CONFLICT(command) DO UPDATE SET
             started_at = excluded.started_at,
             status = 'running',
             finished_at = NULL,
             duration_ms = NULL,
             summary = NULL",
        params![command],
    )?;
    Ok(())
}

pub fn finish_run(
    conn: &Connection,
    command: &str,
    status: &str,
    duration_ms: i64,
    summary: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE pipeline_runs SET
             finished_at = datetime('now'),
             status = ?2,
             duration_ms = ?3,
             summary = ?4
         WHERE command = ?1",
        params![command, status, duration_ms, summary],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_pipeline_runs_table(&conn).unwrap();
        conn
    }

    #[test]
    fn ensure_pipeline_runs_table_is_idempotent() {
        let conn = test_db();
        ensure_pipeline_runs_table(&conn).unwrap();
    }

    #[test]
    fn start_run_then_finish_run_records_success() {
        let conn = test_db();
        start_run(&conn, "embed").unwrap();

        let status: String = conn
            .query_row("SELECT status FROM pipeline_runs WHERE command = 'embed'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "running");

        finish_run(&conn, "embed", "success", 1234, None).unwrap();

        let (status, duration_ms, summary): (String, i64, Option<String>) = conn
            .query_row(
                "SELECT status, duration_ms, summary FROM pipeline_runs WHERE command = 'embed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "success");
        assert_eq!(duration_ms, 1234);
        assert_eq!(summary, None);
    }

    #[test]
    fn start_run_upserts_resetting_prior_finish_fields() {
        let conn = test_db();
        start_run(&conn, "embed").unwrap();
        finish_run(&conn, "embed", "failed", 500, Some("boom")).unwrap();

        start_run(&conn, "embed").unwrap(); // second run begins

        let (status, duration_ms, summary): (String, Option<i64>, Option<String>) = conn
            .query_row(
                "SELECT status, duration_ms, summary FROM pipeline_runs WHERE command = 'embed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "running");
        assert_eq!(duration_ms, None);
        assert_eq!(summary, None);

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM pipeline_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "upsert, not a second row");
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p videre-core pipeline_runs -- --nocapture`
Expected: all 3 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/videre-core/src/pipeline_runs.rs crates/videre-core/src/lib.rs
git commit -m "feat(videre-core): pipeline_runs table + start_run/finish_run"
```

---

### Task 3: Locking (`acquire_lock`, `is_locked`)

**Files:**
- Modify: `crates/videre-core/src/pipeline_runs.rs`

An OS advisory lock (`flock`, via `fs2::FileExt`) at `<canonicalized db path>.<command>.lock`. POSIX `flock` is tied to the *open file description*, not the owning process - two independent `File::open` calls on the same path conflict even within a single test process, which is what makes "second run refused" testable without spawning a real process.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn acquire_lock_refuses_a_second_concurrent_acquisition() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let db_path = db_file.path();

        let _first = acquire_lock(db_path, "faces").unwrap();
        let second = acquire_lock(db_path, "faces");
        assert!(second.is_err(), "a second concurrent lock on the same command must be refused");
    }

    #[test]
    fn acquire_lock_allows_different_commands_concurrently() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let db_path = db_file.path();

        let _faces_lock = acquire_lock(db_path, "faces").unwrap();
        let embed_lock = acquire_lock(db_path, "embed");
        assert!(embed_lock.is_ok(), "different commands must not contend for the same lock");
    }

    #[test]
    fn acquire_lock_is_available_again_after_release() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let db_path = db_file.path();

        {
            let _lock = acquire_lock(db_path, "scan").unwrap();
        } // dropped here, releasing the flock

        let second = acquire_lock(db_path, "scan");
        assert!(second.is_ok(), "lock must be available again once the guard is dropped");
    }
```

Add `tempfile` to `crates/videre-core/Cargo.toml`'s `[dev-dependencies]` if not already present (it already is, from Pass A).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p videre-core acquire_lock -- --nocapture`
Expected: FAIL with `cannot find function 'acquire_lock' in this scope` (compile error).

- [ ] **Step 3: Implement**

Add to `crates/videre-core/src/pipeline_runs.rs` (above the `#[cfg(test)]` module):

```rust
/// Holds an open, flock'd file for as long as it's alive. Dropping it closes
/// the file, which releases the flock - the OS does the same thing
/// automatically if the process dies without ever dropping this (SIGKILL,
/// power loss), so there is no correctness dependency on Drop actually
/// running; it's just the tidy path.
pub struct LockGuard(#[allow(dead_code)] File);

fn lock_path_for(db_path: &Path, command: &str) -> Result<PathBuf> {
    let canonical = db_path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", db_path.display()))?;
    Ok(PathBuf::from(format!("{}.{command}.lock", canonical.display())))
}

/// Acquires an exclusive, non-blocking advisory lock scoped to this exact
/// database file and command. Fails immediately (refusing the run, per the
/// concurrency decision in the design doc) if another live process already
/// holds it - never blocks waiting for it to free up.
pub fn acquire_lock(db_path: &Path, command: &str) -> Result<LockGuard> {
    use fs2::FileExt;
    let lock_path = lock_path_for(db_path, command)?;
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock file {}", lock_path.display()))?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("{command} is already running against {}", db_path.display()))?;
    Ok(LockGuard(file))
}

/// True if another live process currently holds `command`'s lock for
/// `db_path`. Never blocks: probes with a non-blocking try-lock and releases
/// immediately if it succeeds, so this is safe to call from a read path.
pub fn is_locked(db_path: &Path, command: &str) -> Result<bool> {
    use fs2::FileExt;
    let lock_path = lock_path_for(db_path, command)?;
    if !lock_path.exists() {
        return Ok(false);
    }
    let file = OpenOptions::new()
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock file {}", lock_path.display()))?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            FileExt::unlock(&file).ok();
            Ok(false)
        }
        Err(_) => Ok(true),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p videre-core pipeline_runs -- --nocapture`
Expected: all tests in the module PASS (6 total so far).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/pipeline_runs.rs
git commit -m "feat(videre-core): flock-based lock acquisition and liveness probe"
```

---

### Task 4: `track()`

**Files:**
- Modify: `crates/videre-core/src/pipeline_runs.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn track_records_success_and_returns_the_value() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let result = track(&conn, db_file.path(), "embed", || Ok(42)).unwrap();
        assert_eq!(result, 42);

        let status: String = conn
            .query_row("SELECT status FROM pipeline_runs WHERE command = 'embed'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "success");
    }

    #[test]
    fn track_records_failure_with_the_error_message() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let result: Result<()> = track(&conn, db_file.path(), "classify", || {
            Err(anyhow::anyhow!("something broke"))
        });
        assert!(result.is_err());

        let (status, summary): (String, Option<String>) = conn
            .query_row(
                "SELECT status, summary FROM pipeline_runs WHERE command = 'classify'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(summary.as_deref(), Some("something broke"));
    }

    #[test]
    fn track_refuses_when_already_locked() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let _held = acquire_lock(db_file.path(), "scan").unwrap();
        let result: Result<()> = track(&conn, db_file.path(), "scan", || Ok(()));
        assert!(result.is_err(), "track must refuse to run while the lock is already held");

        // No row should have been written for a run that never started.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pipeline_runs WHERE command = 'scan'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p videre-core track_ -- --nocapture`
Expected: FAIL with `cannot find function 'track' in this scope` (compile error).

- [ ] **Step 3: Implement**

```rust
/// Wraps `f` with pipeline-run bookkeeping: refuses to start if `command` is
/// already running against `db_path`, records a `running` row before calling
/// `f`, then records `success`/`failed` (with `f`'s error message, if any)
/// once `f` returns - all before this function itself returns. Every
/// `std::process::exit` call site this design touches happens strictly after
/// its wrapped operation already returned a `Result` (see the design doc's
/// "key design insight"), so this finalization is never skipped by an exit
/// call - only an actual crash mid-`f()` skips it, which is exactly what the
/// lock-based `crashed` detection in `read_all` is for.
pub fn track<T>(
    conn: &Connection,
    db_path: &Path,
    command: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    ensure_pipeline_runs_table(conn)?;
    let _lock = acquire_lock(db_path, command)?;
    start_run(conn, command)?;
    let started = std::time::Instant::now();
    let result = f();
    let duration_ms = started.elapsed().as_millis() as i64;
    match &result {
        Ok(_) => finish_run(conn, command, "success", duration_ms, None)?,
        Err(e) => finish_run(conn, command, "failed", duration_ms, Some(&e.to_string()))?,
    }
    result
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p videre-core pipeline_runs -- --nocapture`
Expected: all tests in the module PASS (9 total so far).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/pipeline_runs.rs
git commit -m "feat(videre-core): track() wraps an operation with run bookkeeping"
```

---

### Task 5: `read_all` + `PipelineRunStatus`

**Files:**
- Modify: `crates/videre-core/src/pipeline_runs.rs`

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn read_all_reports_none_for_a_never_run_command() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let statuses = read_all(&conn, db_file.path()).unwrap();
        let embed = statuses.iter().find(|s| s.command == "embed").unwrap();
        assert_eq!(embed.last_run_at, None);
        assert_eq!(embed.status, None);
        assert!(!embed.currently_running);
    }

    #[test]
    fn read_all_reports_success_after_a_completed_run() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        track(&conn, db_file.path(), "embed", || Ok(())).unwrap();

        let statuses = read_all(&conn, db_file.path()).unwrap();
        let embed = statuses.iter().find(|s| s.command == "embed").unwrap();
        assert_eq!(embed.status.as_deref(), Some("success"));
        assert!(embed.last_run_at.is_some());
        assert!(!embed.currently_running);
    }

    #[test]
    fn read_all_reports_currently_running_while_locked() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        start_run(&conn, "faces").unwrap();
        let _held = acquire_lock(db_file.path(), "faces").unwrap();

        let statuses = read_all(&conn, db_file.path()).unwrap();
        let faces = statuses.iter().find(|s| s.command == "faces").unwrap();
        assert_eq!(faces.status.as_deref(), Some("running"));
        assert!(faces.currently_running);
    }

    #[test]
    fn read_all_reports_crashed_when_running_but_not_locked() {
        let conn = test_db();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        // Simulate a process that started a run and then died without
        // finishing or releasing its lock (the lock dies with the process).
        start_run(&conn, "faces").unwrap();

        let statuses = read_all(&conn, db_file.path()).unwrap();
        let faces = statuses.iter().find(|s| s.command == "faces").unwrap();
        assert_eq!(faces.status.as_deref(), Some("crashed"));
        assert!(!faces.currently_running);

        // The computed label must never be persisted back to the row.
        let stored_status: String = conn
            .query_row("SELECT status FROM pipeline_runs WHERE command = 'faces'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored_status, "running", "read_all must not write back the crashed label");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p videre-core read_all -- --nocapture`
Expected: FAIL with `cannot find function 'read_all' in this scope` (compile error).

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PipelineRunStatus {
    pub command: String,
    pub last_run_at: Option<String>,
    /// "running" | "success" | "failed" | "interrupted" | "crashed" | None if never run.
    /// "crashed" is computed here, never a stored value - see the design doc.
    pub status: Option<String>,
    pub duration_ms: Option<i64>,
    pub currently_running: bool,
}

pub fn read_all(conn: &Connection, db_path: &Path) -> Result<Vec<PipelineRunStatus>> {
    ensure_pipeline_runs_table(conn)?;
    let mut out = Vec::with_capacity(TRACKED_COMMANDS.len());
    for command in TRACKED_COMMANDS {
        let row: Option<(String, Option<i64>, String)> = conn
            .query_row(
                "SELECT started_at, duration_ms, status FROM pipeline_runs WHERE command = ?1",
                params![command],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let currently_running = is_locked(db_path, command)?;

        let (last_run_at, status, duration_ms) = match row {
            None => (None, None, None),
            Some((started_at, duration_ms, stored_status)) => {
                let status = if stored_status == "running" && !currently_running {
                    "crashed".to_string()
                } else {
                    stored_status
                };
                (Some(started_at), Some(status), duration_ms)
            }
        };

        out.push(PipelineRunStatus {
            command: command.to_string(),
            last_run_at,
            status,
            duration_ms,
            currently_running,
        });
    }
    Ok(out)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p videre-core pipeline_runs -- --nocapture`
Expected: all tests in the module PASS (13 total).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/pipeline_runs.rs
git commit -m "feat(videre-core): read_all reports pipeline status with read-time crash detection"
```

---

### Task 6: Wire `fix-dates` (simplest command)

**Files:**
- Modify: `crates/videre/src/commands/fix_dates.rs`

`fix-dates` already computes its `errors` count and checks it *after* the loop finishes (`if errors > 0 { process::exit(1); }` at the end) - no restructuring needed, `track()` slots in around the existing loop.

- [ ] **Step 1: Wire it in**

Replace the body of `crates/videre/src/commands/fix_dates.rs`'s `run` from the `let conn = ...` line through the final `Ok(())`:

```rust
pub fn run(args: FixDatesArgs) -> anyhow::Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;

    if !db.exists() {
        eprintln!("Error: {:?} does not exist", db);
        std::process::exit(1);
    }

    if args.dry_run && !args.silent {
        eprintln!("Dry run: no files will be modified.");
    }

    let conn = videre_core::db::open_wal(&db).expect("failed to open database");

    let errors = videre_core::pipeline_runs::track(&conn, &db, "fix-dates", || {
        run_fix_dates(&args, &conn)
    })?;

    if errors > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// The actual fix-dates work, wrapped by `track()` above. Returns the error
/// count so the caller can decide the exit code after tracking has already
/// finalized the run.
fn run_fix_dates(args: &FixDatesArgs, conn: &rusqlite::Connection) -> anyhow::Result<usize> {
    let mut stmt = conn
        .prepare(
            "SELECT path, exif_date FROM file_hashes \
             WHERE exif_date IS NOT NULL \
             ORDER BY path",
        )
        .expect("failed to prepare query");

    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("failed to execute query")
        .filter_map(|r| r.ok())
        .collect();

    let total = rows.len();
    let mut changed = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for (path, exif_date) in &rows {
        let ndt = match chrono::NaiveDateTime::parse_from_str(exif_date, "%Y-%m-%dT%H:%M:%S") {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Error: {path}: bad exif_date {exif_date:?}: {e}");
                errors += 1;
                continue;
            }
        };

        use chrono::TimeZone;
        let local_dt = match chrono::Local.from_local_datetime(&ndt).single() {
            Some(d) => d,
            None => {
                eprintln!("Error: {path}: ambiguous local time for {exif_date}");
                errors += 1;
                continue;
            }
        };

        let ft = FileTime::from_unix_time(local_dt.timestamp(), 0);

        if !args.dry_run {
            if let Err(e) = filetime::set_file_mtime(path, ft) {
                if e.kind() == std::io::ErrorKind::NotFound {
                    skipped += 1;
                    continue;
                }
                eprintln!("Error: {path}: {e}");
                errors += 1;
                continue;
            }
        }

        if !args.silent {
            let prefix = if args.dry_run { "[dry-run]" } else { "[updated]" };
            println!("{prefix} {path}  →  {exif_date}");
        }
        changed += 1;
    }

    if !args.silent {
        let skipped_note = if skipped > 0 {
            format!(", {skipped} no longer on disk (skipped)")
        } else {
            String::new()
        };
        eprintln!(
            "{} file(s) with exif_date, {} {}, {} error(s){}.",
            total,
            changed,
            if args.dry_run { "would be updated" } else { "updated" },
            errors,
            skipped_note,
        );
    }

    Ok(errors)
}
```

- [ ] **Step 2: Build and run existing tests**

Run: `cargo build -p videre && cargo test -p videre fix_dates`
Expected: builds cleanly; no `fix_dates`-named test file exists yet, so this just confirms compilation (no regressions to check beyond that - `fix-dates` has no dedicated integration test file today).

- [ ] **Step 3: Manual smoke check**

```bash
mkdir -p /tmp/fixdates-smoke && cd /tmp/fixdates-smoke
/path/to/target/debug/videre scan --output-sqlite ./test.db . 2>&1 | tail -3
/path/to/target/debug/videre fix-dates --db ./test.db --dry-run
sqlite3 ./test.db "SELECT command, status, duration_ms FROM pipeline_runs WHERE command = 'fix-dates';"
```
Expected: a `fix-dates|success|<some number>` row.

- [ ] **Step 4: Commit**

```bash
git add crates/videre/src/commands/fix_dates.rs
git commit -m "feat(fix-dates): track pipeline runs"
```

---

### Task 7: Wire `embed`

**Files:**
- Modify: `crates/videre/src/commands/embed.rs`

No restructuring needed - `embed.rs` already uses `?`-based error propagation with no inline `process::exit`.

- [ ] **Step 1: Wire it in**

Replace `crates/videre/src/commands/embed.rs`'s `run` function body:

```rust
pub fn run(args: EmbedArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)
        .with_context(|| format!("open {}", db.display()))?;

    videre_core::pipeline_runs::track(&conn, &db, "embed", || run_embed(&args, &conn))
}

/// The actual embedding work, wrapped by `track()` above.
fn run_embed(args: &EmbedArgs, conn: &rusqlite::Connection) -> Result<()> {
    embeddings::ensure_embeddings_table(conn)?;

    let pending = embeddings::pending_images(conn, model::MODEL_ID)?;
    if pending.is_empty() {
        if !args.silent {
            eprintln!("Nothing to embed: all hashes already have embeddings.");
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let dev = device::best_device();
    let embedder = model::Embedder::load(dev.clone())?;

    let progress = videre_core::progress::Progress::new(pending.len() as u64, args.silent);

    let mut done = 0usize;
    let mut failed = 0usize;
    for chunk in pending.chunks(args.chunk) {
        let decoded: Vec<Option<(String, candle_core::Tensor)>> = chunk
            .par_iter()
            .map(|p| {
                match preprocess::image_to_tensor(
                    std::path::Path::new(&p.path),
                    model::IMAGE_SIZE,
                    &candle_core::Device::Cpu,
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

        embeddings::insert_embeddings(conn, model::MODEL_ID, &rows)?;
        done += rows.len();
        progress.tick_by(chunk.len() as u64);
    }

    progress.finish();

    if !args.silent {
        eprintln!("{}", format_summary(done, failed, started.elapsed()));
    }
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p videre`
Expected: builds cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/videre/src/commands/embed.rs
git commit -m "feat(embed): track pipeline runs"
```

---

### Task 8: Wire `classify`

**Files:**
- Modify: `crates/videre/src/commands/classify.rs`

Same shape as `embed` - no restructuring needed.

- [ ] **Step 1: Wire it in**

Replace `crates/videre/src/commands/classify.rs`'s `run` function body:

```rust
pub fn run(args: ClassifyArgs) -> Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)
        .with_context(|| format!("open {}", db.display()))?;

    videre_core::pipeline_runs::track(&conn, &db, "classify", || run_classify(&args, &conn))
}

/// The actual classification work, wrapped by `track()` above.
fn run_classify(args: &ClassifyArgs, conn: &rusqlite::Connection) -> Result<()> {
    classify_core::ensure_classifications_table(conn)?;

    let all_embeddings: std::collections::HashMap<String, Vec<u8>> =
        embeddings::load_embeddings(conn, model::MODEL_ID)?.into_iter().collect();

    let hashes: Vec<String> = if args.reprocess {
        all_embeddings.keys().cloned().collect()
    } else {
        classify_core::pending_hashes(conn, model::MODEL_ID)?
    };

    if hashes.is_empty() {
        if !args.silent {
            eprintln!("Nothing to classify: all embedded hashes already classified.");
        }
        return Ok(());
    }

    let started = std::time::Instant::now();
    let embedder = model::Embedder::load(device::best_device())?;

    let prompt_vecs: Vec<(&'static str, Vec<f32>)> = classify_ml::CATEGORY_PROMPTS
        .iter()
        .map(|(name, prompt)| Ok((*name, embedder.embed_text(prompt)?)))
        .collect::<Result<_>>()?;

    let progress = videre_core::progress::Progress::new(hashes.len() as u64, args.silent);
    let mut rows: Vec<(String, &str, f32)> = Vec::with_capacity(hashes.len());
    for hash in &hashes {
        let Some(blob) = all_embeddings.get(hash) else {
            progress.println(&format!("skipping {hash}: embedding vanished mid-run"));
            progress.tick();
            continue;
        };
        let vec = vectors::from_f16_bytes(blob);
        let scores: Vec<(&'static str, f32)> = prompt_vecs
            .iter()
            .map(|(name, prompt_vec)| {
                let dot: f32 = vec.iter().zip(prompt_vec.iter()).map(|(a, b)| a * b).sum();
                (*name, dot)
            })
            .collect();
        let (category, confidence) = classify_ml::classify_from_scores(&scores, args.margin);
        rows.push((hash.clone(), category, confidence));
        progress.tick();
    }
    progress.finish();

    classify_core::insert_classifications(conn, &rows)?;

    if !args.silent {
        eprintln!("{}", format_summary(rows.len(), started.elapsed()));
    }
    Ok(())
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p videre`
Expected: builds cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/videre/src/commands/classify.rs
git commit -m "feat(classify): track pipeline runs"
```

---

### Task 9: Wire `dedupe` (requires a small restructure)

**Files:**
- Modify: `crates/videre/src/commands/dedupe.rs`

`run_text`'s `load_records` failure currently calls `process::exit(1)` directly inside the match, before any later check - unlike `faces.rs`/`fix_dates.rs`, this one *is* embedded mid-operation, so it must become a `return Err(...)` so `track()`'s finalization actually runs before the process exits. Both `run_text` and `run_json` need their own `Connection` opened solely for tracking (`sqlite_output::load_records` opens its own separate connection internally).

- [ ] **Step 1: Rewrite `run_text` and `run_json`**

Replace `crates/videre/src/commands/dedupe.rs`'s `run_text` and `run_json` functions:

```rust
fn run_text(args: DedupeArgs) -> anyhow::Result<()> {
    let db = match super::resolve_reader_db_must_exist(args.db) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error: {e:#}");
            process::exit(1);
        }
    };
    let conn = match videre_core::db::open_wal(&db) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error opening {:?}: {}", db, e);
            process::exit(1);
        }
    };

    let result = videre_core::pipeline_runs::track(&conn, &db, "dedupe", || run_dedupe_text(&args, &db));
    if let Err(e) = result {
        eprintln!("Error: {e:#}");
        process::exit(1);
    }
    Ok(())
}

/// The actual dedupe-reporting work, wrapped by `track()` above.
fn run_dedupe_text(args: &DedupeArgs, db: &std::path::Path) -> anyhow::Result<()> {
    let records = videre::sqlite_output::load_records(db)
        .map_err(|e| anyhow::anyhow!("reading {:?}: {}", db, e))?;

    let groups = videre::output::find_duplicate_groups(&records);
    if !args.silent {
        if groups.is_empty() {
            eprintln!("No exact duplicates found.");
        } else {
            eprintln!(
                "{} duplicate group(s), {} file(s) to remove.",
                groups.len(),
                groups.iter().map(|g| g.files.len() - 1).sum::<usize>()
            );
        }
    }
    videre::output::print_losers(&groups);

    if args.similar {
        let similar = videre::output::find_similar_groups(&records, 10);
        if !args.silent && !similar.is_empty() {
            eprintln!(
                "{} visually similar group(s) found: review with videre report before deleting.",
                similar.len()
            );
        }
    }

    Ok(())
}

fn run_json(args: &DedupeArgs) -> anyhow::Result<videre::types::FindDuplicatesJson> {
    let db = super::resolve_reader_db_must_exist(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db)?;
    videre_core::pipeline_runs::track(&conn, &db, "dedupe", || {
        super::build_find_duplicates(&db, args.similar)
    })
}
```

Note: `run_json`'s existing caller in `run()` already does `Ok(doc) => ... Err(e) => { println!(json error); process::exit(1); }` *after* `run_json` returns - already the correct pattern, untouched.

- [ ] **Step 2: Build and run existing tests**

Run: `cargo build -p videre && cargo test -p videre --test mcp`
Expected: builds cleanly; `mcp` tests still pass (they call `build_find_duplicates` directly via the MCP `find_duplicates` tool, not through `dedupe`'s CLI wrapper, so they're unaffected by this change).

- [ ] **Step 3: Commit**

```bash
git add crates/videre/src/commands/dedupe.rs
git commit -m "feat(dedupe): track pipeline runs"
```

---

### Task 10: Wire `scan` (requires a small restructure)

**Files:**
- Modify: `crates/videre/src/commands/scan.rs`

Only the SQLite-writing branch of `run_text` needs restructuring (its `sqlite_output::write_records` failure currently calls `process::exit(1)` inline, mid-operation). `run_json`'s SQLite branch already uses `?`, so it just needs wrapping. The JSONL branches in both are never tracked - no db path exists to track against.

- [ ] **Step 1: Rewrite the SQLite branches**

In `crates/videre/src/commands/scan.rs`, replace the `Ok(OutputTarget::Sqlite(db_path))` arm inside `run_text`:

```rust
        Ok(OutputTarget::Sqlite(db_path)) => {
            let conn = match videre_core::db::open_wal(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error opening {:?}: {}", db_path, e);
                    process::exit(1);
                }
            };
            let write_result = videre_core::pipeline_runs::track(&conn, &db_path, "scan", || {
                sqlite_output::write_records(&records, &db_path)
                    .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))
            });
            if let Err(e) = write_result {
                eprintln!("Error: {e:#}");
                process::exit(1);
            }
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", db_path)));
            }
        }
```

And replace the `OutputTarget::Sqlite(db_path)` arm inside `run_json`:

```rust
        OutputTarget::Sqlite(db_path) => {
            let conn = videre_core::db::open_wal(&db_path)
                .map_err(|e| anyhow::anyhow!("opening {:?}: {}", db_path, e))?;
            videre_core::pipeline_runs::track(&conn, &db_path, "scan", || {
                sqlite_output::write_records(&records, &db_path)
                    .map_err(|e| anyhow::anyhow!("writing to {:?}: {}", db_path, e))
            })?;
            if !args.silent {
                eprintln!("{}", format_write_summary(records.len(), skipped, &format!("{:?}", db_path)));
            }
            ScanOutputJson { kind: "sqlite", path: db_path.display().to_string() }
        }
```

- [ ] **Step 2: Run the existing scan test suite**

Run: `cargo test -p videre --test scan`
Expected: all existing tests still PASS (they check written record counts and stderr summaries, both unchanged by this refactor).

- [ ] **Step 3: Add one new integration test**

Add to `crates/videre/tests/scan.rs`:

```rust
#[test]
fn sqlite_scan_records_a_pipeline_run() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let db_path = out_dir.path().join("hashes.db");

    fs::write(scan_dir.path().join("a.jpg"), b"content").unwrap();

    let status = Command::new(videre_bin())
        .arg("scan")
        .arg("--silent")
        .arg("--output-sqlite")
        .arg(&db_path)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run videre");
    assert!(status.success());

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let (status, duration_ms): (String, Option<i64>) = conn
        .query_row(
            "SELECT status, duration_ms FROM pipeline_runs WHERE command = 'scan'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "success");
    assert!(duration_ms.is_some());
}
```

- [ ] **Step 4: Run it**

Run: `cargo test -p videre --test scan sqlite_scan_records_a_pipeline_run`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/scan.rs crates/videre/tests/scan.rs
git commit -m "feat(scan): track pipeline runs (SQLite mode only)"
```

---

### Task 11: Wire `faces`

**Files:**
- Modify: `crates/videre/src/commands/faces.rs`

`faces.rs` already checks `result.write_errors > 0 || result.detect_errors > 0` *after* `run_face_pipeline` returns - no restructuring needed, `track()` wraps the whole detection-plus-clustering block.

- [ ] **Step 1: Wire it in**

In `crates/videre/src/commands/faces.rs`, replace from `let started = std::time::Instant::now();` through the `if result.write_errors > 0 ...` block (i.e. everything after the early-return `--recluster`/empty-input branch) with:

```rust
    let outcome = videre_core::pipeline_runs::track(&conn, &db, "faces", || {
        run_detection_and_clustering(&args, &conn, &to_process)
    })?;

    if outcome.write_errors > 0 || outcome.detect_errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

struct FacesOutcome {
    write_errors: usize,
    detect_errors: usize,
}

/// The detection-plus-clustering work, wrapped by `track()` above.
fn run_detection_and_clustering(
    args: &FacesArgs,
    conn: &rusqlite::Connection,
    to_process: &[(String, String)],
) -> Result<FacesOutcome> {
    let started = std::time::Instant::now();
    let mut profile_stats = ProfileStats::default();
    let workers = args.workers.unwrap_or_else(|| {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        cores * 2
    });
    let result = run_face_pipeline(
        conn, to_process, args.batch, args.dry_run, args.silent,
        if args.profile { Some(&mut profile_stats) } else { None },
        workers,
    )?;

    let clustering = if !args.dry_run && args.limit.is_none() {
        run_clustering(conn, args.eps, args.min_cluster_size, args.merge_sim, args.min_face_size, args.max_generic_sim, args.silent)?
    } else {
        None
    };

    if !args.silent {
        eprintln!("{}", format_summary(&result, clustering, args.eps, started.elapsed()));
        if args.limit.is_some() && !args.dry_run {
            let remaining = face_db::scanned_hashes(conn)?.len();
            eprintln!(
                "partial run (--limit): {remaining} image(s) scanned so far; rerun to continue, then 'videre faces --recluster' to cluster"
            );
        }
    }

    if args.profile {
        eprintln!("{}", format_profile_report(&profile_stats));
    }

    Ok(FacesOutcome { write_errors: result.write_errors, detect_errors: result.detect_errors })
}
```

The function now ends after the closing `}` of `run_detection_and_clustering` - the original `pub fn run(...)`'s closing brace moves to right after `Ok(())` above (i.e. the `run` function body ends there; `FacesOutcome` and `run_detection_and_clustering` are separate top-level items after it). Leave `format_summary` and `format_clustering_only_summary` (and their tests) untouched below.

- [ ] **Step 2: Run the existing faces tests**

Run: `cargo test -p videre faces`
Expected: all existing tests (`format_summary_*`, `format_clustering_only_summary_*`) still PASS - unaffected, since only `run`'s internals changed.

- [ ] **Step 3: Commit**

```bash
git add crates/videre/src/commands/faces.rs
git commit -m "feat(faces): track pipeline runs"
```

---

### Task 12: Wire `videre watch`'s `scan`/`faces` stages

**Files:**
- Modify: `crates/videre/src/commands/watch.rs`

`watch`'s `run_scan_stage` and `run_faces_stage` reimplement the same operations inline - each gets its own `track()` call, using the same `"scan"`/`"faces"` command names so the dashboard's "last run" reflects whichever of `videre scan`/`videre watch` (or `videre faces`/`videre watch`) ran most recently, whichever that is.

- [ ] **Step 1: Wire `run_scan_stage`**

Replace `crates/videre/src/commands/watch.rs`'s `run_scan_stage`:

```rust
fn run_scan_stage(args: &WatchArgs, directory: &std::path::Path, db: &std::path::Path) -> Result<()> {
    let conn = db::open_wal(db)?;
    videre_core::pipeline_runs::track(&conn, db, "scan", || {
        let paths = scanner::scan(directory);
        let records: Vec<types::FileRecord> = paths
            .par_iter()
            .filter_map(|path| hasher::hash_file(path).ok())
            .collect();
        sqlite_output::write_records(&records, db)?;
        if !args.silent {
            eprintln!("videre watch: scan stage wrote {} record(s)", records.len());
        }
        Ok(())
    })
}
```

- [ ] **Step 2: Wire `run_faces_stage`**

Replace `crates/videre/src/commands/watch.rs`'s `run_faces_stage` (it already receives an open `conn` from `run_cycle`, reused directly - no separate connection open needed):

```rust
fn run_faces_stage(args: &WatchArgs, conn: &rusqlite::Connection) -> Result<()> {
    let db_path = conn.path().map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("faces stage requires a file-backed database"))?;
    videre_core::pipeline_runs::track(conn, &db_path, "faces", || {
        let all_paths = dedup_paths_by_hash(
            conn,
            "ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')",
        )?;
        let mut skip_hashes: std::collections::HashSet<String> =
            face_db::scanned_hashes(conn)?.into_iter().collect();
        skip_hashes.extend(face_db::hashes_with_faces(conn)?);
        let to_process: Vec<(String, String)> = all_paths
            .into_iter()
            .filter(|(_, hash)| !skip_hashes.contains(hash))
            .collect();

        if !to_process.is_empty() {
            let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            let result = run_face_pipeline(conn, &to_process, 8, false, args.silent, None, workers)?;
            if !args.silent {
                eprintln!(
                    "videre watch: faces stage processed {} new hash(es), {} face(s)",
                    to_process.len(),
                    result.total_faces
                );
            }
        }
        let clustering = run_clustering(conn, 0.6, 3, videre_core::face_cluster::DEFAULT_MERGE_SIM, videre_core::face_cluster::DEFAULT_MIN_FACE_PX, videre_core::face_cluster::DEFAULT_MAX_GENERIC_SIM, args.silent)?;
        if !args.silent {
            eprintln!("videre watch: {}", format_clustering_only_summary(clustering, 0.6));
        }
        Ok(())
    })
}
```

`rusqlite::Connection::path()` returns `Option<&str>` for a file-backed connection (`None` for `:memory:`) - safe here since `watch` always opens a real file path.

- [ ] **Step 3: Build**

Run: `cargo build -p videre`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/videre/src/commands/watch.rs
git commit -m "feat(watch): track scan/faces stage runs through the same pipeline_runs rows"
```

---

### Task 13: `videre watch`'s own liveness lock

**Files:**
- Modify: `crates/videre/src/commands/watch.rs`

`watch` itself takes a lock for the life of the whole loop (not per-cycle) - liveness only, no `pipeline_runs` row, since it has no "finished" moment during normal operation.

- [ ] **Step 1: Acquire the lock at startup, held for the loop's life**

In `crates/videre/src/commands/watch.rs`'s `run` function, right after the `db` variable is resolved and before the `loop { ... }`:

```rust
    // Held for the entire life of this process - releases automatically
    // (even on kill) when the process exits, same mechanism as every other
    // command's lock. No pipeline_runs row: watch has no "finished" moment
    // during normal operation, only "currently running or not".
    let _watch_lock = videre_core::pipeline_runs::acquire_lock(&db, "watch")
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    loop {
```

- [ ] **Step 2: Build**

Run: `cargo build -p videre`
Expected: builds cleanly.

- [ ] **Step 3: Manual smoke check for refusal**

```bash
mkdir -p /tmp/watch-smoke && cd /tmp/watch-smoke
/path/to/target/debug/videre watch --output-sqlite ./test.db --scan --interval 60 /some/small/dir &
WATCH_PID=$!
sleep 1
/path/to/target/debug/videre watch --output-sqlite ./test.db --scan --interval 60 /some/small/dir
# Expected: second invocation exits nonzero immediately with an "already running" error
kill $WATCH_PID
```

- [ ] **Step 4: Commit**

```bash
git add crates/videre/src/commands/watch.rs
git commit -m "feat(watch): acquire a liveness lock for the life of the process"
```

---

### Task 14: SIGINT handler in `scan` and `faces`

**Files:**
- Modify: `crates/videre/src/commands/scan.rs`
- Modify: `crates/videre/src/commands/faces.rs`

Only these two commands, per the design decision - they're the ones documented as normal to interrupt. On Ctrl-C: open a fresh connection, compute duration from the row's existing `started_at`, mark `interrupted`, exit(130).

- [ ] **Step 1: Add a shared helper**

Add to `crates/videre-core/src/pipeline_runs.rs` (after `finish_run`):

```rust
/// Installs a SIGINT handler that marks `command`'s row `interrupted` (using
/// its already-recorded `started_at` to compute duration) and exits 130, the
/// standard SIGINT exit code. Call this once, after `track()`'s `start_run`
/// has already written the `running` row for `command` - the handler opens
/// its own fresh connection since the main thread's `Connection` isn't
/// safely shareable across the handler boundary. Best-effort: any error
/// inside the handler is swallowed (there's no useful way to report it once
/// the process is already exiting on a signal).
pub fn install_sigint_handler(db_path: &Path, command: &'static str) -> Result<()> {
    let db_path = db_path.to_path_buf();
    ctrlc::set_handler(move || {
        if let Ok(conn) = Connection::open(&db_path) {
            let started_at: Option<String> = conn
                .query_row(
                    "SELECT started_at FROM pipeline_runs WHERE command = ?1",
                    params![command],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten();
            let duration_ms = started_at
                .and_then(|s| chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").ok())
                .map(|started| {
                    (chrono::Utc::now().naive_utc() - started).num_milliseconds().max(0)
                })
                .unwrap_or(0);
            let _ = finish_run(&conn, command, "interrupted", duration_ms, None);
        }
        std::process::exit(130);
    })
    .context("installing SIGINT handler")
}
```

Add `ctrlc` and `chrono` to `crates/videre-core/Cargo.toml`'s `[dependencies]` (chrono for parsing SQLite's `datetime('now')` format, matching `fix_dates.rs`'s existing use of `chrono::NaiveDateTime::parse_from_str` elsewhere in the `videre` crate - this is the first `videre-core` use, so it's a new direct dependency there):

```toml
ctrlc = "3"
chrono = "0.4"
```

- [ ] **Step 2: Write a test for the handler installation (not the signal itself - see Task-level note below)**

```rust
    #[test]
    fn install_sigint_handler_does_not_error_when_called_once() {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        // Only one handler can be installed per process for the life of the
        // test binary; this just confirms the call itself succeeds.
        // (ctrlc::set_handler errors if called twice in the same process,
        // so this is deliberately the only test that calls it in this suite.)
        let result = install_sigint_handler(db_file.path(), "scan");
        assert!(result.is_ok());
    }
```

- [ ] **Step 3: Run it**

Run: `cargo test -p videre-core install_sigint_handler -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Call it from `scan` and `faces`**

In `crates/videre/src/commands/scan.rs`'s `run_text`, right after the SQLite branch's connection is opened (inside the `Ok(OutputTarget::Sqlite(db_path))` arm, before calling `track`):

```rust
            let conn = match videre_core::db::open_wal(&db_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Error opening {:?}: {}", db_path, e);
                    process::exit(1);
                }
            };
            if let Err(e) = videre_core::pipeline_runs::install_sigint_handler(&db_path, "scan") {
                eprintln!("Warning: could not install interrupt handler: {e:#}");
            }
            let write_result = videre_core::pipeline_runs::track(&conn, &db_path, "scan", || {
```

In `crates/videre/src/commands/faces.rs`'s `run`, right before the `track` call added in Task 11:

```rust
    if let Err(e) = videre_core::pipeline_runs::install_sigint_handler(&db, "faces") {
        eprintln!("Warning: could not install interrupt handler: {e:#}");
    }
    let outcome = videre_core::pipeline_runs::track(&conn, &db, "faces", || {
```

- [ ] **Step 5: Build**

Run: `cargo build -p videre`
Expected: builds cleanly.

- [ ] **Step 6: Manual verification (no automated test - signal delivery is inherently flaky in CI)**

```bash
mkdir -p /tmp/sigint-smoke && cd /tmp/sigint-smoke
/path/to/target/debug/videre scan --output-sqlite ./test.db /some/large/dir &
SCAN_PID=$!
sleep 1
kill -INT $SCAN_PID
sleep 1
sqlite3 ./test.db "SELECT status FROM pipeline_runs WHERE command = 'scan';"
```
Expected: `interrupted`, not `running` or a crash-detected `crashed` (verify by also reading the row with a `read_all`-equivalent query, or just trust the stored value here since `interrupted` overwrote `running` before exit).

- [ ] **Step 7: Commit**

```bash
git add crates/videre-core/src/pipeline_runs.rs crates/videre-core/Cargo.toml crates/videre/src/commands/scan.rs crates/videre/src/commands/faces.rs Cargo.lock
git commit -m "feat(scan,faces): SIGINT marks the run interrupted instead of crashed"
```

---

### Task 15: `videre-api::pipeline_status()` facade

**Files:**
- Create: `crates/videre-api/src/pipeline_status.rs`
- Modify: `crates/videre-api/src/lib.rs`

- [ ] **Step 1: Write the module with an inline test**

Create `crates/videre-api/src/pipeline_status.rs`:

```rust
//! Facade over videre-core's pipeline run tracking. See
//! docs/superpowers/specs/2026-07-31-dashboard-stats-pass-b-design.md.

use crate::error::Result;
use rusqlite::Connection;
use std::path::Path;
pub use videre_core::pipeline_runs::PipelineRunStatus;

pub fn pipeline_status(conn: &Connection, db_path: &Path) -> Result<Vec<PipelineRunStatus>> {
    Ok(videre_core::pipeline_runs::read_all(conn, db_path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_status_reports_all_six_tracked_commands_never_run() {
        let conn = Connection::open_in_memory().unwrap();
        let db_file = tempfile::NamedTempFile::new().unwrap();

        let statuses = pipeline_status(&conn, db_file.path()).unwrap();
        assert_eq!(statuses.len(), 6);
        assert!(statuses.iter().all(|s| s.status.is_none()));
    }
}
```

Note: `videre_core::pipeline_runs::read_all` returns `anyhow::Result<T>`, and `videre-api::Error` doesn't have a `From<anyhow::Error>` impl yet (only `From<rusqlite::Error>`) - the `?` inside `Ok(...?)` needs one. Add a dedicated `Error::Other(String)` variant rather than forcing an awkward conversion into an existing variant.

In `crates/videre-api/src/error.rs`, update the enum and its `Display`/`From` impls:

```rust
#[derive(Debug)]
pub enum Error {
    /// The target row/label does not exist (e.g. rename of an unknown person).
    NotFound,
    /// The requested change collides with existing state (e.g. rename onto an
    /// existing person).
    Conflict,
    /// Caller-supplied input was rejected (e.g. an empty label after sanitizing).
    Invalid,
    /// Underlying database failure.
    Db(rusqlite::Error),
    /// Any other failure surfaced as a plain message (e.g. from videre-core
    /// functions that return anyhow::Error, like pipeline_runs).
    Other(String),
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Other(e.to_string())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::Conflict => write!(f, "conflict"),
            Error::Invalid => write!(f, "invalid input"),
            Error::Db(e) => write!(f, "database error: {e}"),
            Error::Other(msg) => write!(f, "{msg}"),
        }
    }
}
```

Add `anyhow = "1"` to `crates/videre-api/Cargo.toml`'s `[dependencies]` (not currently a direct dependency there).

- [ ] **Step 2: Register the module in `lib.rs`**

In `crates/videre-api/src/lib.rs`:

```rust
mod error;
mod faces;
mod images;
mod label;
mod pipeline_status;
mod stats;
mod types;

pub use error::{Error, Result};
pub use faces::{
    assign, cluster_detail, delete_person, dissolve_cluster, faces_list, new_person, person_detail,
    remove_face, rename_person, search_person, set_primary,
};
pub use images::{
    face_bytes_from_lookup, face_image_bytes, face_lookup, mime_for_ext, original_bytes_from_lookup,
    original_image_bytes, original_lookup, FaceLookup, OriginalLookup,
};
pub use label::sanitize_person_label;
pub use pipeline_status::{pipeline_status, PipelineRunStatus};
pub use stats::{library_stats, LibraryStats};
pub use types::{
    ClusterData, ClusterDetail, ClusterFaceData, FacesData, PersonData, PersonDetail,
    PersonFaceData, SingletonData,
};
```

Also add `tempfile` to `crates/videre-api/Cargo.toml`'s `[dev-dependencies]` if not already present (needed by the test above).

- [ ] **Step 3: Run the tests**

Run: `cargo test -p videre-api pipeline_status -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Run the full workspace build**

Run: `cargo build --workspace`
Expected: builds cleanly.

- [ ] **Step 5: Commit**

```bash
git add crates/videre-api/src/pipeline_status.rs crates/videre-api/src/lib.rs crates/videre-api/src/error.rs crates/videre-api/Cargo.toml Cargo.lock
git commit -m "feat(videre-api): pipeline_status facade"
```

---

### Task 16: Tauri `pipeline_status` command

**Files:**
- Modify: `app/src-tauri/src/commands.rs`
- Modify: `app/src-tauri/src/lib.rs`

- [ ] **Step 1: Add the command**

In `app/src-tauri/src/commands.rs`, update the import and add the command at the end of the file:

```rust
use videre_api::{ClusterDetail, FacesData, LibraryStats, PersonDetail, PipelineRunStatus};
```

```rust
#[tauri::command]
pub fn pipeline_status(db: State<DbState>) -> Result<Vec<PipelineRunStatus>, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    let db_path = videre_core::home::resolve_db(None).map_err(|e| e.to_string())?;
    videre_api::pipeline_status(&conn, &db_path).map_err(err)
}
```

Note: `DbState` only holds an open `Connection`, not the path it was opened from - `pipeline_status` needs the path too (for the lock probe), so it re-resolves it the same way `DbState::open` did. Since `DbState::open` always opens with no explicit `--db` override in the Tauri app, `videre_core::home::resolve_db(None)` returns the same path.

- [ ] **Step 2: Register it in the Tauri builder**

In `app/src-tauri/src/lib.rs`, add `commands::pipeline_status,` to the `generate_handler!` list:

```rust
        .invoke_handler(tauri::generate_handler![
            commands::faces_list,
            commands::cluster_detail,
            commands::person_detail,
            commands::search_person,
            commands::assign,
            commands::new_person,
            commands::remove_face,
            commands::dissolve_cluster,
            commands::delete_person,
            commands::set_primary,
            commands::rename_person,
            commands::library_stats,
            commands::pipeline_status,
        ])
```

- [ ] **Step 3: Build the Tauri app crate**

Run: `cd app/src-tauri && cargo build`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add app/src-tauri/src/commands.rs app/src-tauri/src/lib.rs
git commit -m "feat(app): expose pipeline_status as a Tauri command"
```

---

### Task 17: Full workspace verification + version bump

**Files:** none (verification + version bumps only)

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS - no failures anywhere, including the new `pipeline_runs`/`pipeline_status` tests and the untouched pre-existing suites.

- [ ] **Step 2: Run the full workspace build in release mode**

Run: `cargo build --release`
Expected: builds cleanly.

- [ ] **Step 3: Build the Tauri app in release mode**

Run: `cd app/src-tauri && cargo build --release`
Expected: builds cleanly.

- [ ] **Step 4: Manual end-to-end check against a real (small) directory**

```bash
mkdir -p /tmp/pipeline-runs-e2e && cd /tmp/pipeline-runs-e2e
/path/to/target/release/videre scan --output-sqlite ./test.db ~/Pictures/some-small-folder
/path/to/target/release/videre embed --db ./test.db
/path/to/target/release/videre classify --db ./test.db
/path/to/target/release/videre faces --db ./test.db
/path/to/target/release/videre fix-dates --db ./test.db --dry-run
/path/to/target/release/videre dedupe --db ./test.db > /dev/null
sqlite3 ./test.db "SELECT command, status, duration_ms FROM pipeline_runs ORDER BY command;"
```
Expected: six rows (`classify`, `dedupe`, `embed`, `faces`, `fix-dates`, `scan`), each `success` with a non-null `duration_ms`. Re-run `videre scan` a second time and confirm the row's `started_at`/`duration_ms` update rather than a new row appearing (`SELECT COUNT(*) FROM pipeline_runs;` stays 6).

- [ ] **Step 5: Bump crate versions**

Per this project's convention (bump minor/patch on every commit, never 1.0.0 until told), bump `crates/videre-core`, `crates/videre-api`, `crates/videre` patch versions, and stage the root `Cargo.lock` in the same commit.

```bash
cargo build --release  # regenerates Cargo.lock with the new version numbers
git add crates/videre-core/Cargo.toml crates/videre-api/Cargo.toml crates/videre/Cargo.toml Cargo.lock
git commit -m "chore: bump versions for pipeline run tracking (Pass B)"
```
