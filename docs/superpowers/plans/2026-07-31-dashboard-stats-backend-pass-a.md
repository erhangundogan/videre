# Dashboard Stats Backend (Pass A) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `LibraryStats` aggregate-query module (total files/size, photo/video split, duplicate group/file counts + wasted bytes, faces detected, people named) shared by `videre report`'s existing stats tile and a new Tauri `library_stats` command, so the desktop app's future dashboard has real data to read.

**Architecture:** New `videre-core::library_stats` module (plain query functions over `&rusqlite::Connection`, matching the existing `classify.rs`/`face_db.rs` style) is the single source of truth. `videre report`'s `query_stats` is rewired to delegate to it instead of keeping its own copy of the duplicate-counting SQL. A new `videre-api::library_stats()` facade function wraps it in the shared `Error`/`Result` type, and a thin `#[tauri::command]` in `app/src-tauri/src/commands.rs` exposes it to the desktop app - the same three-layer pattern every existing faces-labeling command already follows.

**Tech Stack:** Rust, `rusqlite` (bundled SQLite), `serde` (new direct dependency for `videre-core`), Tauri v2 commands.

**Out of scope (do not implement in this plan):** `videre mcp`'s `build_stats`/`StatsJson` refactor (field-set mismatch with the MCP JSON contract - deferred), `pipeline_runs`/lockfiles/scan-faces run-status (Pass B - needs a redesign, not ready), any React/UI/component work. See `docs/superpowers/specs/2026-07-31-dashboard-stats-backend-design.md` for the full spec and why these are excluded.

---

## File Structure

- Modify: `crates/videre-core/Cargo.toml` - add `serde` dependency
- Create: `crates/videre-core/src/library_stats.rs` - `LibraryStats` struct + `compute()` + `table_exists()` helper + inline tests
- Modify: `crates/videre-core/src/lib.rs` - register the new module
- Modify: `crates/videre/src/commands/report.rs:345-371` - `query_stats()` delegates to the shared function instead of running its own SQL
- Create: `crates/videre-api/src/stats.rs` - facade function wrapping `videre_core::library_stats::compute`
- Modify: `crates/videre-api/src/lib.rs` - register and re-export the new module
- Modify: `app/src-tauri/src/commands.rs` - new `library_stats` Tauri command
- Modify: `app/src-tauri/src/lib.rs` - register the new command in `generate_handler!`

---

### Task 1: Add `serde` to `videre-core`

**Files:**
- Modify: `crates/videre-core/Cargo.toml`

- [ ] **Step 1: Add the dependency**

`crates/videre-core/Cargo.toml` currently has no `serde`. Add it under `[dependencies]` (matching how `videre-api` already declares it):

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
```

- [ ] **Step 2: Verify the workspace still builds**

Run: `cargo build -p videre-core`
Expected: builds cleanly (no code uses `serde` yet, so this just confirms the dependency resolves).

- [ ] **Step 3: Commit**

```bash
git add crates/videre-core/Cargo.toml Cargo.lock
git commit -m "chore(videre-core): add serde dependency for library_stats"
```

---

### Task 2: `library_stats` module - totals (total_files, total_size_bytes)

**Files:**
- Create: `crates/videre-core/src/library_stats.rs`
- Modify: `crates/videre-core/src/lib.rs`

- [ ] **Step 1: Register the module**

In `crates/videre-core/src/lib.rs`, add a line in alphabetical position with the existing `pub mod` list:

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
pub mod semaphore;
pub mod location;
pub mod person_search;
pub mod progress;
pub mod thumb_cache;
pub mod vectors;
```

- [ ] **Step 2: Write the failing test**

Create `crates/videre-core/src/library_stats.rs` with just the test scaffold and totals test first:

```rust
//! Aggregate library statistics for the desktop app's home dashboard.
//! Plain queries over an open `rusqlite::Connection` - shared source of truth
//! for `videre report`'s stats tile and the Tauri `library_stats` command.
//! See docs/superpowers/specs/2026-07-31-dashboard-stats-backend-design.md
//! (Pass A) for what is and isn't in scope.

use rusqlite::{Connection, Result};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct LibraryStats {
    pub total_files: i64,
    pub total_size_bytes: i64,
}

pub fn compute(conn: &Connection) -> Result<LibraryStats> {
    let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    let total_size_bytes: i64 =
        conn.query_row("SELECT COALESCE(SUM(size_bytes), 0) FROM file_hashes", [], |r| r.get(0))?;

    Ok(LibraryStats { total_files, total_size_bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path        TEXT PRIMARY KEY,
                hash        TEXT NOT NULL,
                size_bytes  INTEGER,
                ext         TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str, hash: &str, size_bytes: i64, ext: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![path, hash, size_bytes, ext],
        )
        .unwrap();
    }

    #[test]
    fn compute_counts_total_files_and_size() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", 1000, "jpg");
        insert_file(&conn, "/a/2.png", "h2", 2500, "png");

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_size_bytes, 3500);
    }

    #[test]
    fn compute_on_empty_db_returns_zeros() {
        let conn = test_db();
        let stats = compute(&conn).unwrap();
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_size_bytes, 0);
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p videre-core library_stats -- --nocapture`
Expected: `compute_counts_total_files_and_size` and `compute_on_empty_db_returns_zeros` both PASS (this task has no separate "write failing test then implement" gap since the function is trivial - the test and implementation were written together; running it now is the verification step).

- [ ] **Step 4: Commit**

```bash
git add crates/videre-core/src/library_stats.rs crates/videre-core/src/lib.rs
git commit -m "feat(videre-core): library_stats totals (total_files, total_size_bytes)"
```

---

### Task 3: `library_stats` - photo/video split

**Files:**
- Modify: `crates/videre-core/src/library_stats.rs`

`ext` is stored lowercased with **no leading dot** (confirmed in `crates/videre/src/hasher.rs:143-147` - values like `"jpg"`, `"mp4"`, not `".jpg"`). Video = `mov`/`mp4`; photo = the rest of the supported list (`jpg`, `jpeg`, `png`, `gif`, `webp`, `bmp`, `tiff`, `heic`, `dng`). Rows with `NULL`/empty/unrecognized `ext` land in neither bucket - `total_photos + total_videos` is not guaranteed to equal `total_files`, which is expected and not a bug.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/videre-core/src/library_stats.rs`:

```rust
    #[test]
    fn compute_splits_photos_and_videos_by_extension() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", 100, "jpg");
        insert_file(&conn, "/a/2.heic", "h2", 100, "heic");
        insert_file(&conn, "/a/3.mov", "h3", 100, "mov");
        insert_file(&conn, "/a/4.mp4", "h4", 100, "mp4");
        insert_file(&conn, "/a/5.unknown", "h5", 100, "xyz");

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.total_photos, 2);
        assert_eq!(stats.total_videos, 2);
        assert_eq!(stats.total_files, 5); // unrecognized ext still counts toward total_files
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p videre-core compute_splits_photos_and_videos_by_extension -- --nocapture`
Expected: FAIL with `no field 'total_photos' on type 'LibraryStats'` (compile error).

- [ ] **Step 3: Implement**

Update `LibraryStats` and `compute` in `crates/videre-core/src/library_stats.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct LibraryStats {
    pub total_files: i64,
    pub total_size_bytes: i64,
    pub total_photos: i64,
    pub total_videos: i64,
}

const PHOTO_EXTS: &str = "'jpg','jpeg','png','gif','webp','bmp','tiff','heic','dng'";
const VIDEO_EXTS: &str = "'mov','mp4'";

pub fn compute(conn: &Connection) -> Result<LibraryStats> {
    let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    let total_size_bytes: i64 =
        conn.query_row("SELECT COALESCE(SUM(size_bytes), 0) FROM file_hashes", [], |r| r.get(0))?;
    let total_photos: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM file_hashes WHERE ext IN ({PHOTO_EXTS})"),
        [],
        |r| r.get(0),
    )?;
    let total_videos: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM file_hashes WHERE ext IN ({VIDEO_EXTS})"),
        [],
        |r| r.get(0),
    )?;

    Ok(LibraryStats { total_files, total_size_bytes, total_photos, total_videos })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p videre-core library_stats -- --nocapture`
Expected: all tests in the module PASS, including the new one and the two from Task 2.

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/library_stats.rs
git commit -m "feat(videre-core): library_stats photo/video split"
```

---

### Task 4: `library_stats` - duplicate group/file counts + wasted bytes

**Files:**
- Modify: `crates/videre-core/src/library_stats.rs`

Reuse the exact query shape from `crates/videre/src/commands/report.rs:345-371`'s `query_stats` (`GROUP BY hash HAVING COUNT(*) > 1`, no on-disk-existence filtering, no reuse of dedupe's KEEP/REMOVE tie-break logic - verified numerically identical to `videre dedupe`'s output since group membership/counts don't depend on which file within a group is "the keep").

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn compute_counts_duplicate_groups_and_wasted_bytes() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "dup-hash", 1000, "jpg");
        insert_file(&conn, "/b/1-copy.jpg", "dup-hash", 1000, "jpg");
        insert_file(&conn, "/a/2.jpg", "dup-hash", 1000, "jpg");
        insert_file(&conn, "/a/3.jpg", "unique-hash", 500, "jpg");

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.duplicate_group_count, 1);
        assert_eq!(stats.duplicate_file_count, 3); // all 3 members of the dup group
        assert_eq!(stats.wasted_bytes, 2000); // (3 - 1) * 1000
    }

    #[test]
    fn compute_with_no_duplicates_reports_zero() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", 500, "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", 500, "jpg");

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.duplicate_group_count, 0);
        assert_eq!(stats.duplicate_file_count, 0);
        assert_eq!(stats.wasted_bytes, 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p videre-core compute_counts_duplicate_groups_and_wasted_bytes compute_with_no_duplicates_reports_zero -- --nocapture`
Expected: FAIL with `no field 'duplicate_group_count' on type 'LibraryStats'` (compile error).

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct LibraryStats {
    pub total_files: i64,
    pub total_size_bytes: i64,
    pub total_photos: i64,
    pub total_videos: i64,
    pub duplicate_group_count: i64,
    pub duplicate_file_count: i64,
    pub wasted_bytes: i64,
}

pub fn compute(conn: &Connection) -> Result<LibraryStats> {
    let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    let total_size_bytes: i64 =
        conn.query_row("SELECT COALESCE(SUM(size_bytes), 0) FROM file_hashes", [], |r| r.get(0))?;
    let total_photos: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM file_hashes WHERE ext IN ({PHOTO_EXTS})"),
        [],
        |r| r.get(0),
    )?;
    let total_videos: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM file_hashes WHERE ext IN ({VIDEO_EXTS})"),
        [],
        |r| r.get(0),
    )?;
    let duplicate_group_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM \
         (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
        [],
        |r| r.get(0),
    )?;
    let duplicate_file_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_hashes \
         WHERE hash IN (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
        [],
        |r| r.get(0),
    )?;
    let wasted_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size_bytes * (cnt - 1)), 0) FROM \
         (SELECT hash, size_bytes, COUNT(*) as cnt \
          FROM file_hashes GROUP BY hash HAVING cnt > 1)",
        [],
        |r| r.get(0),
    )?;

    Ok(LibraryStats {
        total_files,
        total_size_bytes,
        total_photos,
        total_videos,
        duplicate_group_count,
        duplicate_file_count,
        wasted_bytes,
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p videre-core library_stats -- --nocapture`
Expected: all tests in the module PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/library_stats.rs
git commit -m "feat(videre-core): library_stats duplicate counts and wasted bytes"
```

---

### Task 5: `library_stats` - faces_detected, people_named (with table-existence guard)

**Files:**
- Modify: `crates/videre-core/src/library_stats.rs`

The `faces` table only exists once `videre faces` has run at least once. Add a `table_exists` helper (same pattern already used in `crates/videre/src/commands/mcp.rs:95-102`, tested there via "stats must return zero counts without optional tables") so `compute` returns `0` for these fields instead of erroring on a fresh database.

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn compute_counts_faces_and_named_people() {
        let conn = test_db();
        conn.execute_batch(
            "CREATE TABLE faces (
                id            INTEGER PRIMARY KEY,
                hash          TEXT NOT NULL,
                bbox          TEXT NOT NULL,
                landmark      TEXT,
                embedding     BLOB NOT NULL,
                cluster_id    INTEGER,
                person_label  TEXT,
                confirmed     INTEGER DEFAULT 0,
                is_primary    INTEGER DEFAULT 0
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (1, 'h1', '[]', X'00', 'Alice', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (2, 'h1', '[]', X'00', 'Alice', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (3, 'h2', '[]', X'00', NULL, 0)",
            [],
        )
        .unwrap();

        let stats = compute(&conn).unwrap();
        assert_eq!(stats.faces_detected, 3);
        assert_eq!(stats.people_named, 1); // distinct confirmed person_label
    }

    #[test]
    fn compute_without_faces_table_returns_zero_not_error() {
        let conn = test_db(); // no faces table created
        let stats = compute(&conn).unwrap();
        assert_eq!(stats.faces_detected, 0);
        assert_eq!(stats.people_named, 0);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p videre-core compute_counts_faces_and_named_people compute_without_faces_table_returns_zero_not_error -- --nocapture`
Expected: FAIL with `no field 'faces_detected' on type 'LibraryStats'` (compile error).

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct LibraryStats {
    pub total_files: i64,
    pub total_size_bytes: i64,
    pub total_photos: i64,
    pub total_videos: i64,
    pub duplicate_group_count: i64,
    pub duplicate_file_count: i64,
    pub wasted_bytes: i64,
    pub faces_detected: i64,
    pub people_named: i64,
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

pub fn compute(conn: &Connection) -> Result<LibraryStats> {
    let total_files: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    let total_size_bytes: i64 =
        conn.query_row("SELECT COALESCE(SUM(size_bytes), 0) FROM file_hashes", [], |r| r.get(0))?;
    let total_photos: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM file_hashes WHERE ext IN ({PHOTO_EXTS})"),
        [],
        |r| r.get(0),
    )?;
    let total_videos: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM file_hashes WHERE ext IN ({VIDEO_EXTS})"),
        [],
        |r| r.get(0),
    )?;
    let duplicate_group_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM \
         (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
        [],
        |r| r.get(0),
    )?;
    let duplicate_file_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_hashes \
         WHERE hash IN (SELECT hash FROM file_hashes GROUP BY hash HAVING COUNT(*) > 1)",
        [],
        |r| r.get(0),
    )?;
    let wasted_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size_bytes * (cnt - 1)), 0) FROM \
         (SELECT hash, size_bytes, COUNT(*) as cnt \
          FROM file_hashes GROUP BY hash HAVING cnt > 1)",
        [],
        |r| r.get(0),
    )?;

    let (faces_detected, people_named) = if table_exists(conn, "faces")? {
        let faces_detected: i64 = conn.query_row("SELECT COUNT(*) FROM faces", [], |r| r.get(0))?;
        let people_named: i64 = conn.query_row(
            "SELECT COUNT(DISTINCT person_label) FROM faces \
             WHERE confirmed = 1 AND person_label IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        (faces_detected, people_named)
    } else {
        (0, 0)
    };

    Ok(LibraryStats {
        total_files,
        total_size_bytes,
        total_photos,
        total_videos,
        duplicate_group_count,
        duplicate_file_count,
        wasted_bytes,
        faces_detected,
        people_named,
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p videre-core library_stats -- --nocapture`
Expected: all tests in the module PASS (9 tests total across Tasks 2-5).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/library_stats.rs
git commit -m "feat(videre-core): library_stats faces/people counts with table guard"
```

---

### Task 6: Rewire `videre report`'s `query_stats` to use the shared function

**Files:**
- Modify: `crates/videre/src/commands/report.rs:345-371`

`report.rs`'s local `Stats` struct (`total_files`, `duplicate_groups`, `duplicate_files`, `wasted_bytes` - field names it already uses at call sites on lines 661-701 and 2692-2694) stays as-is so no call site needs to change. Only `query_stats`'s body changes, from running its own SQL to delegating to `videre_core::library_stats::compute` and mapping field names.

- [ ] **Step 1: Confirm current behavior via existing tests (baseline)**

Run: `cargo test -p videre report -- --nocapture`
Expected: PASS (establishes the pre-refactor baseline so we know Step 3 doesn't regress anything).

- [ ] **Step 2: Replace `query_stats`'s implementation**

In `crates/videre/src/commands/report.rs`, replace the body of `query_stats` (currently lines 345-371) with:

```rust
fn query_stats(conn: &Connection) -> Stats {
    let s = videre_core::library_stats::compute(conn).unwrap_or_default();
    Stats {
        total_files: s.total_files,
        duplicate_groups: s.duplicate_group_count,
        duplicate_files: s.duplicate_file_count,
        wasted_bytes: s.wasted_bytes,
    }
}
```

Leave the `struct Stats { ... }` definition (line 68) untouched - it keeps its existing field names so every call site (`stats.total_files`, `stats.duplicate_groups`, `stats.duplicate_files`, `stats.wasted_bytes`) continues to compile unchanged.

- [ ] **Step 3: Run the tests to verify nothing regressed**

Run: `cargo test -p videre report -- --nocapture`
Expected: PASS, same test count and results as Step 1's baseline (this is a behavior-preserving refactor, not a behavior change).

- [ ] **Step 4: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "refactor(report): delegate query_stats to videre-core library_stats"
```

---

### Task 7: `videre-api` facade function

**Files:**
- Create: `crates/videre-api/src/stats.rs`
- Modify: `crates/videre-api/src/lib.rs`

- [ ] **Step 1: Write the module with an inline test**

Create `crates/videre-api/src/stats.rs`:

```rust
//! Facade over videre-core's library-wide aggregate stats. Thin wrapper so
//! callers (the Tauri desktop app; `videre mcp` may adopt this later once its
//! own JSON contract question is resolved - see the Pass A design doc) go
//! through one shared `Error`/`Result` type like every other videre-api
//! operation.

use crate::error::Result;
use rusqlite::Connection;
pub use videre_core::library_stats::LibraryStats;

pub fn library_stats(conn: &Connection) -> Result<LibraryStats> {
    Ok(videre_core::library_stats::compute(conn)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_stats_returns_totals_from_an_empty_db() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path        TEXT PRIMARY KEY,
                hash        TEXT NOT NULL,
                size_bytes  INTEGER,
                ext         TEXT
            );",
        )
        .unwrap();

        let stats = library_stats(&conn).unwrap();
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_size_bytes, 0);
        assert_eq!(stats.faces_detected, 0); // no faces table - guarded, not an error
    }
}
```

- [ ] **Step 2: Register the module in `lib.rs`**

In `crates/videre-api/src/lib.rs`:

```rust
mod error;
mod faces;
mod images;
mod label;
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
pub use stats::{library_stats, LibraryStats};
pub use types::{
    ClusterData, ClusterDetail, ClusterFaceData, FacesData, PersonData, PersonDetail,
    PersonFaceData, SingletonData,
};
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p videre-api stats -- --nocapture`
Expected: `library_stats_returns_totals_from_an_empty_db` PASSes.

- [ ] **Step 4: Run the full workspace build to catch the new dependency edge**

Run: `cargo build`
Expected: builds cleanly - confirms `videre-api` (already depending on `videre-core`, per `crates/videre-api/Cargo.toml`) resolves the new module with no additional Cargo.toml changes needed.

- [ ] **Step 5: Commit**

```bash
git add crates/videre-api/src/stats.rs crates/videre-api/src/lib.rs
git commit -m "feat(videre-api): library_stats facade"
```

---

### Task 8: Tauri `library_stats` command

**Files:**
- Modify: `app/src-tauri/src/commands.rs`
- Modify: `app/src-tauri/src/lib.rs`

- [ ] **Step 1: Add the command**

In `app/src-tauri/src/commands.rs`, update the import at the top and add the new command at the end of the file (matching every existing command's shape exactly):

```rust
use crate::state::DbState;
use tauri::State;
use videre_api::{ClusterDetail, FacesData, LibraryStats, PersonDetail};
```

```rust
#[tauri::command]
pub fn library_stats(db: State<DbState>) -> Result<LibraryStats, String> {
    let conn = db.0.lock().map_err(|_| "db lock poisoned".to_string())?;
    videre_api::library_stats(&conn).map_err(err)
}
```

- [ ] **Step 2: Register it in the Tauri builder**

In `app/src-tauri/src/lib.rs`, add `commands::library_stats,` to the `generate_handler!` list:

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
        ])
```

- [ ] **Step 3: Build the Tauri app crate**

Run: `cd app/src-tauri && cargo build`
Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add app/src-tauri/src/commands.rs app/src-tauri/src/lib.rs
git commit -m "feat(app): expose library_stats as a Tauri command"
```

---

### Task 9: Full workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`
Expected: PASS - no failures in `videre-core`, `videre-api`, or `videre` (including the pre-existing `report` tests untouched by Task 6's refactor).

- [ ] **Step 2: Run the full workspace build in release mode**

Run: `cargo build --release`
Expected: builds cleanly (confirms no dev-only feature gaps).

- [ ] **Step 3: Build the Tauri app one more time from the app directory**

Run: `cd app/src-tauri && cargo build --release`
Expected: builds cleanly.

- [ ] **Step 4: Bump crate versions**

Per this project's convention (bump minor/patch on every commit, never 1.0.0 until told), bump `crates/videre-core/Cargo.toml`, `crates/videre-api/Cargo.toml`, and `crates/videre/Cargo.toml` patch versions, and stage the root `Cargo.lock` in the same commit (a stale lock breaks `--locked` builds).

```bash
cargo build --release  # regenerates Cargo.lock with the new version numbers
git add crates/videre-core/Cargo.toml crates/videre-api/Cargo.toml crates/videre/Cargo.toml Cargo.lock
git commit -m "chore: bump versions for library_stats backend (Pass A)"
```
