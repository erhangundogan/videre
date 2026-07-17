# Faces Labeling UI Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix seven bugs/UX gaps in `videre report --faces` (the axum-based face-labeling web UI): Enter-to-submit on name inputs, a "Remove person" action, a rename action, a real assign modal instead of `prompt()`, hardened person-name sanitization, a working thumbnail cache for face crops/originals, and referrer-aware back navigation from the lightbox - plus a small related fix to `handle_remove_face` found during spec review.

**Architecture:** Everything lives in one file, `crates/videre/src/commands/report.rs` (2755 lines) - HTML/JS/CSS are Rust string constants (`FACES_HTML`, `CLUSTER_HTML`, `PERSON_HTML`) with no separate frontend build. Backend changes (new routes/handlers, `AppState` fields) are TDD'd with `#[tokio::test]` calling handler functions directly with a hand-built `AppState`. `videre-core/src/thumb_cache.rs` gets two new cache-key functions, TDD'd the same way as its existing tests. Pure JS/HTML template changes have no automated test harness in this project and are covered by a manual smoke test in the final task, matching this project's existing convention (see `crates/videre/tests/faces_server.rs`).

**Tech Stack:** Rust, axum 0.8, rusqlite (bundled SQLite), tokio, hand-written JS/HTML template strings.

---

## Before you start

All work happens in `crates/videre/src/commands/report.rs` unless stated otherwise. Read the spec first: `docs/superpowers/specs/2026-07-17-faces-ui-fixes-design.md`.

Run `cargo test -p videre -p videre-core` before starting to confirm a clean baseline.

---

### Task 1: Harden person-name sanitization (server + client)

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (`sanitize_person_label`, ~line 1975; `FACES_HTML`'s `sanitizeName`, ~line 1375; `CLUSTER_HTML`'s `sanitizeName`, ~line 1535)
- Test: `crates/videre/src/commands/report.rs` (`mod tests` block, ends ~line 2755)

- [ ] **Step 1: Write the failing tests**

Add these to the end of the `mod tests` block (after the existing `parse_bbox_converts_xywh_to_corners` test, before the closing `}` of `mod tests`):

```rust
    #[test]
    fn sanitize_person_label_strips_control_chars() {
        assert_eq!(sanitize_person_label("Al\u{0007}ice"), Some("Alice".to_string()));
    }

    #[test]
    fn sanitize_person_label_strips_bidi_override() {
        assert_eq!(sanitize_person_label("A\u{202E}lice"), Some("Alice".to_string()));
    }

    #[test]
    fn sanitize_person_label_keeps_zwj_emoji_sequences() {
        // family emoji: man + ZWJ + woman + ZWJ + girl - must survive intact,
        // since stripping the ZWJ (U+200D) would split it into three separate emoji.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(sanitize_person_label(family), Some(family.to_string()));
    }

    #[test]
    fn sanitize_person_label_truncates_by_char_not_byte() {
        let long = "e".repeat(65);
        let result = sanitize_person_label(&long).unwrap();
        assert_eq!(result.chars().count(), 60);
    }
```

- [ ] **Step 2: Run tests to verify the new behavior is missing**

Run: `cargo test -p videre sanitize_person_label --lib`

Expected: `sanitize_person_label_strips_control_chars` and `sanitize_person_label_strips_bidi_override` FAIL (current code only collapses whitespace, so the BEL/bidi-override characters pass through unchanged). `sanitize_person_label_keeps_zwj_emoji_sequences` and `sanitize_person_label_truncates_by_char_not_byte` already PASS today (current code neither strips ZWJ nor truncates unsafely) - they're included now as regression guards the upcoming filter step must not break.

- [ ] **Step 3: Implement the server-side filter**

Replace the existing `sanitize_person_label` function:

```rust
/// Trim, collapse internal whitespace, and cap length so a client that
/// bypasses the UI's own sanitization can't stretch card layout or bloat
/// the DB with an unbounded label. Mirrors the client-side sanitizeName().
fn sanitize_person_label(raw: &str) -> Option<String> {
    let filtered: String = raw
        .chars()
        .filter(|c| !c.is_control() && !is_disallowed_format_char(*c))
        .collect();
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(60).collect())
}

/// Zero-width and bidi-override characters that can visually spoof a name
/// (e.g. right-to-left override) without being caught by `char::is_control`.
/// Deliberately excludes U+200C (ZWNJ) and U+200D (ZWJ): both are required
/// for legitimate text - ZWJ joins emoji sequences (a family emoji is three
/// emoji joined by ZWJ; stripping it splits them apart) and ZWNJ is
/// orthographically required in Persian and several Indic scripts.
fn is_disallowed_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}' // zero-width space
        | '\u{200E}'..='\u{200F}' // LRM/RLM
        | '\u{202A}'..='\u{202E}' // LRE/RLE/PDF/LRO/RLO bidi overrides
        | '\u{2060}'..='\u{2069}' // word joiner, invisible operators, isolates
        | '\u{FEFF}' // BOM / zero-width no-break space
    )
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre sanitize_person_label --lib`

Expected: all four tests PASS.

- [ ] **Step 5: Apply the matching client-side filter**

There is no automated test for this step (JS embedded in a Rust string constant, no browser test harness in this project - verified manually in Task 12). Replace `FACES_HTML`'s `sanitizeName` function:

```js
    const MAX_NAME_LEN = 60;

    // Trim, collapse internal whitespace, strip control/bidi-spoofing
    // characters, and cap length by code point (not UTF-16 code unit) so a
    // pasted wall of text or a spoofed name can't stretch card layout,
    // corrupt display order, or bloat the DB.
    function sanitizeName(raw) {
      const filtered = Array.from(raw).filter(function(ch) {
        const cp = ch.codePointAt(0);
        if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false;
        if (cp === 0x200B) return false;
        if (cp === 0x200E || cp === 0x200F) return false;
        // 0x200C (ZWNJ) and 0x200D (ZWJ) are intentionally allowed -
        // required for Persian/Indic text and emoji ZWJ sequences.
        if (cp >= 0x202A && cp <= 0x202E) return false;
        if (cp >= 0x2060 && cp <= 0x2069) return false;
        if (cp === 0xFEFF) return false;
        return true;
      }).join('');
      const collapsed = filtered.trim().replace(/\s+/g, ' ');
      return Array.from(collapsed).slice(0, MAX_NAME_LEN).join('');
    }
```

Apply the exact same replacement to `CLUSTER_HTML`'s copy of `sanitizeName` (identical function body, different surrounding context - both currently read `return raw.trim().replace(/\s+/g, ' ').slice(0, MAX_NAME_LEN);`).

- [ ] **Step 6: Build to confirm no compile errors**

Run: `cargo build -p videre`

Expected: builds cleanly (JS changes are inside string literals, so this only catches Rust-side mistakes).

- [ ] **Step 7: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "fix: harden person-name sanitization against control/bidi characters"
```

---

### Task 2: Fix `handle_remove_face` to reset `is_primary`

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (`handle_remove_face`, ~line 2015)
- Test: `crates/videre/src/commands/report.rs` (`mod tests` block)

This task also introduces two test helpers (`mem_db_with_faces`, `test_state`) that every later backend task in this plan reuses.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block, alongside the other helper functions (near `mem_db()`):

```rust
    fn mem_db_with_faces() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
             size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
             phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
             width INTEGER, height INTEGER);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0,
             is_primary INTEGER DEFAULT 0);",
        )
        .unwrap();
        conn
    }

    fn test_state(conn: Connection, serve_faces_ui: bool) -> Arc<AppState> {
        Arc::new(AppState {
            conn: Mutex::new(conn),
            shutdown_tx: Mutex::new(None),
            report_all: false,
            report_by_date: false,
            report_heic: false,
            report_heic_original: false,
            serve_faces_ui,
        })
    }
```

Then add the test itself:

```rust
    #[tokio::test]
    async fn remove_face_resets_is_primary() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed, is_primary) \
             VALUES (1, 'h1', '0,0,10,10', X'0000', 5, 'Alice', 1, 1)",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);
        let result = handle_remove_face(State(state.clone()), AxumJson(RemoveFaceRequest { face_id: 1 })).await;
        assert_eq!(result, Ok(StatusCode::OK));
        let conn = state.conn.lock().unwrap();
        let is_primary: i64 = conn.query_row("SELECT is_primary FROM faces WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(is_primary, 0, "is_primary must be reset when a face is removed");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre remove_face_resets_is_primary --lib`

Expected: FAIL - `is_primary` is still `1` after the call (current `handle_remove_face` SQL doesn't touch that column).

- [ ] **Step 3: Fix the handler**

Modify `handle_remove_face`'s SQL statement (the only line that changes):

```rust
async fn handle_remove_face(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<RemoveFaceRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    conn.execute(
        "UPDATE faces SET cluster_id = NULL, person_label = NULL, confirmed = 0, is_primary = 0 WHERE id = ?1",
        [req.face_id],
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::OK)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p videre remove_face_resets_is_primary --lib`

Expected: PASS.

- [ ] **Step 5: Run the full test suite to check for regressions**

Run: `cargo test -p videre --lib`

Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "fix: reset is_primary when a face is removed"
```

---

### Task 3: Add "Remove person" backend (`/api/delete-person`)

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (add `DeletePersonRequest` near `AssignRequest` ~line 1772; add `handle_delete_person` near `handle_remove_face`; register route ~line 2467)
- Test: `crates/videre/src/commands/report.rs` (`mod tests` block)

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn delete_person_resets_faces_but_keeps_cluster_id() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed, is_primary) \
             VALUES (1, 'h1', '0,0,10,10', X'0000', 5, 'Alice', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, cluster_id, person_label, confirmed, is_primary) \
             VALUES (2, 'h2', '0,0,10,10', X'0000', NULL, 'Alice', 1, 0)",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);
        let result = handle_delete_person(
            State(state.clone()),
            AxumJson(DeletePersonRequest { label: "Alice".to_string() }),
        )
        .await;
        assert_eq!(result, StatusCode::OK);

        let conn = state.conn.lock().unwrap();
        let (cluster_id, person_label, confirmed, is_primary): (Option<i64>, Option<String>, i64, i64) = conn
            .query_row(
                "SELECT cluster_id, person_label, confirmed, is_primary FROM faces WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(cluster_id, Some(5), "cluster_id must survive removal - the face rejoins its unassigned cluster");
        assert_eq!(person_label, None);
        assert_eq!(confirmed, 0);
        assert_eq!(is_primary, 0);

        let cluster_id2: Option<i64> =
            conn.query_row("SELECT cluster_id FROM faces WHERE id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(cluster_id2, None, "a face that was already a singleton stays a singleton");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre delete_person_resets_faces_but_keeps_cluster_id --lib`

Expected: FAIL to compile - `DeletePersonRequest` and `handle_delete_person` don't exist yet.

- [ ] **Step 3: Implement the request struct and handler**

Add near `AssignRequest`/`NewPersonRequest` (~line 1772):

```rust
#[derive(Deserialize)]
struct DeletePersonRequest {
    label: String,
}
```

Add near `handle_remove_face`:

```rust
/// Resets every face carrying `label` back to unassigned. Deliberately does
/// NOT touch `cluster_id`: a face that came from a DBSCAN cluster rejoins
/// that cluster's unassigned group (picked up by query_faces_data's cluster
/// query) instead of scattering to Singletons; a face that was already a
/// singleton (cluster_id already NULL) stays a singleton. Does not trigger
/// re-clustering - there is no live DBSCAN re-run in this server.
async fn handle_delete_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<DeletePersonRequest>,
) -> StatusCode {
    let conn = match state.conn.lock() {
        Ok(c) => c,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    match conn.execute(
        "UPDATE faces SET person_label = NULL, confirmed = 0, is_primary = 0 WHERE person_label = ?1",
        rusqlite::params![req.label],
    ) {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

Register the route inside the `if state.serve_faces_ui { ... }` block in `serve_faces_async` (~line 2467), alongside the other mutating routes:

```rust
    if state.serve_faces_ui {
        router = router
            .route("/api/faces", get(handle_get_faces))
            .route("/api/assign", post(handle_assign))
            .route("/api/new-person", post(handle_new_person))
            .route("/api/remove-face", post(handle_remove_face))
            .route("/api/delete-person", post(handle_delete_person))
            .route("/api/dissolve-cluster", post(handle_dissolve_cluster))
            .route("/api/set-primary", post(handle_set_primary));
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p videre delete_person_resets_faces_but_keeps_cluster_id --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "feat: add /api/delete-person backend"
```

---

### Task 4: Add "Rename person" backend (`/api/rename-person`)

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (add `RenamePersonRequest` near `DeletePersonRequest`; add `handle_rename_person`; register route)
- Test: `crates/videre/src/commands/report.rs` (`mod tests` block)

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn rename_person_updates_label() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (1, 'h1', '0,0,10,10', X'0000', 'Alice', 1)",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);
        let result = handle_rename_person(
            State(state.clone()),
            AxumJson(RenamePersonRequest { old_label: "Alice".to_string(), new_label: "Alicia".to_string() }),
        )
        .await;
        assert_eq!(result, Ok(StatusCode::OK));
        let conn = state.conn.lock().unwrap();
        let label: String = conn.query_row("SELECT person_label FROM faces WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(label, "Alicia");
    }

    #[tokio::test]
    async fn rename_person_rejects_collision_with_existing_label() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (1, 'h1', '0,0,10,10', X'0000', 'Alice', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding, person_label, confirmed) \
             VALUES (2, 'h2', '0,0,10,10', X'0000', 'Bob', 1)",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);
        let result = handle_rename_person(
            State(state.clone()),
            AxumJson(RenamePersonRequest { old_label: "Alice".to_string(), new_label: "Bob".to_string() }),
        )
        .await;
        assert_eq!(result, Err(StatusCode::CONFLICT));
        let conn = state.conn.lock().unwrap();
        let label: String = conn.query_row("SELECT person_label FROM faces WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(label, "Alice", "rename must not have applied on collision");
    }

    #[tokio::test]
    async fn rename_person_rejects_nonexistent_old_label() {
        let conn = mem_db_with_faces();
        let state = test_state(conn, true);
        let result = handle_rename_person(
            State(state.clone()),
            AxumJson(RenamePersonRequest { old_label: "Ghost".to_string(), new_label: "Someone".to_string() }),
        )
        .await;
        assert_eq!(result, Err(StatusCode::NOT_FOUND));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre rename_person --lib`

Expected: FAIL to compile - `RenamePersonRequest` and `handle_rename_person` don't exist yet.

- [ ] **Step 3: Implement the request struct and handler**

Add near `DeletePersonRequest`:

```rust
#[derive(Deserialize)]
struct RenamePersonRequest {
    old_label: String,
    new_label: String,
}
```

Add near `handle_delete_person`:

```rust
/// Renames every face carrying `old_label` to `new_label`. Rejects a
/// collision with an existing person (409) rather than silently merging -
/// a silent merge would be irreversible and could leave the merged person
/// with two faces marked is_primary = 1, violating the invariant
/// handle_set_primary maintains via its clear-then-set transaction.
async fn handle_rename_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<RenamePersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let Some(sanitized) = sanitize_person_label(&req.new_label) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let old_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![req.old_label],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if old_count == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    let collision_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![sanitized],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if collision_count > 0 && sanitized != req.old_label {
        return Err(StatusCode::CONFLICT);
    }

    conn.execute(
        "UPDATE faces SET person_label = ?1 WHERE person_label = ?2",
        rusqlite::params![sanitized, req.old_label],
    )
    .map(|_| StatusCode::OK)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
```

Register the route inside the `if state.serve_faces_ui { ... }` block, alongside `/api/delete-person`:

```rust
            .route("/api/delete-person", post(handle_delete_person))
            .route("/api/rename-person", post(handle_rename_person))
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre rename_person --lib`

Expected: all three PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "feat: add /api/rename-person backend"
```

---

### Task 5: Extend `thumb_cache` with face-crop and original-image cache keys

**Files:**
- Modify: `crates/videre-core/src/thumb_cache.rs`
- Test: `crates/videre-core/src/thumb_cache.rs` (`mod tests` block)

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` block, after `cache_dir_is_under_videre`:

```rust
    #[test]
    fn face_thumb_path_is_keyed_by_hash_face_id_and_size() {
        let p1 = face_thumb_path("abc123", 1, 140);
        let p2 = face_thumb_path("abc123", 2, 140);
        let p3 = face_thumb_path("def456", 1, 140);
        assert_ne!(p1, p2, "different face ids must produce different paths");
        assert_ne!(p1, p3, "different hashes must produce different paths");
        assert!(p1.to_string_lossy().contains("abc123_face1_140.jpg"));
    }

    #[test]
    fn face_thumb_exists_false_for_missing_file() {
        assert!(!face_thumb_exists("nonexistent-hash-xyz", 99, 140));
    }

    #[test]
    fn original_path_is_keyed_by_hash() {
        let p1 = original_path("abc123");
        let p2 = original_path("def456");
        assert_ne!(p1, p2);
        assert!(p1.to_string_lossy().contains("abc123_original.jpg"));
    }

    #[test]
    fn original_exists_false_for_missing_file() {
        assert!(!original_exists("nonexistent-hash-xyz"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre-core thumb_cache --lib`

Expected: FAIL to compile - `face_thumb_path`, `face_thumb_exists`, `original_path`, `original_exists` don't exist yet.

- [ ] **Step 3: Implement the new cache-key functions**

Add to `crates/videre-core/src/thumb_cache.rs`, after the existing `thumb_exists`:

```rust
/// Cache path for a single face crop. Distinct from `thumb_path` because
/// many faces can share one source `hash` - the face id disambiguates.
pub fn face_thumb_path(hash: &str, face_id: i64, size: u32) -> PathBuf {
    cache_dir().join(format!("{hash}_face{face_id}_{size}.jpg"))
}

/// True if a cached face crop already exists for this hash/face_id/size.
pub fn face_thumb_exists(hash: &str, face_id: i64, size: u32) -> bool {
    face_thumb_path(hash, face_id, size).exists()
}

/// Cache path for a full-resolution HEIC-converted original. One per hash
/// (not per face - the original photo is the same regardless of which face
/// on it was clicked).
pub fn original_path(hash: &str) -> PathBuf {
    cache_dir().join(format!("{hash}_original.jpg"))
}

/// True if a cached full-resolution original already exists for this hash.
pub fn original_exists(hash: &str) -> bool {
    original_path(hash).exists()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p videre-core thumb_cache --lib`

Expected: all four new tests PASS, and the pre-existing `thumb_path`/`thumb_exists`/`cache_dir`/`migrate_dir` tests still PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/videre-core/src/thumb_cache.rs
git commit -m "feat: add face-crop and original-image cache key functions"
```

---

### Task 6: Wire `handle_face_image` into the cache

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (imports; `AppState` struct ~line 1805; `serve_faces_async`'s `AppState` construction ~line 2445; `handle_face_image` ~line 2235; `test_state` helper from Task 2)
- Test: `crates/videre/src/commands/report.rs` (`mod tests` block)

This task adds a `tmp_counter` field to `AppState` so concurrent cache writes for the same uncached face can't collide on the same tmp filename (the existing `thumb_tmp_path` scheme only disambiguates by process id, which was safe for `videre watch`'s single-threaded writer but not for concurrent axum request handlers in this server).

- [ ] **Step 1: Write the failing test**

Add to the top of the file, alongside the other `use` statements:

```rust
use std::sync::atomic::{AtomicU64, Ordering};
```

Update `test_state` (added in Task 2) to include the new field - this is a required edit for the file to compile once Step 3 below adds `tmp_counter` to `AppState`:

```rust
    fn test_state(conn: Connection, serve_faces_ui: bool) -> Arc<AppState> {
        Arc::new(AppState {
            conn: Mutex::new(conn),
            shutdown_tx: Mutex::new(None),
            report_all: false,
            report_by_date: false,
            report_heic: false,
            report_heic_original: false,
            serve_faces_ui,
            tmp_counter: AtomicU64::new(0),
        })
    }
```

Add the test itself, after `remove_face_resets_is_primary`:

```rust
    #[tokio::test]
    async fn face_image_request_populates_and_then_hits_cache() {
        let dir = tempfile::tempdir().unwrap();
        let img_path = dir.path().join("test.jpg");
        let img = image::DynamicImage::new_rgb8(20, 20);
        img.save(&img_path).unwrap();

        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, 'facecachehash', 'jpg')",
            rusqlite::params![img_path.to_str().unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding) VALUES (9001, 'facecachehash', '0,0,10,10', X'0000')",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);

        let cache_path = videre_core::thumb_cache::face_thumb_path("facecachehash", 9001, 140);
        let _ = std::fs::remove_file(&cache_path);
        assert!(!cache_path.exists(), "precondition: no stale cache file");

        let first = handle_face_image(axum::extract::Path(9001), State(state.clone())).await;
        assert!(first.is_ok());
        assert!(cache_path.exists(), "handler must write through to the cache on a miss");

        let second = handle_face_image(axum::extract::Path(9001), State(state.clone())).await;
        assert!(second.is_ok(), "second request must be served from cache");

        let _ = std::fs::remove_file(&cache_path);
    }
```

`tempfile` is already a `videre` dev-dependency (see `crates/videre/Cargo.toml`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre face_image_request_populates_and_then_hits_cache --lib`

Expected: compile FAILS first (`tmp_counter` referenced in the test helper but not yet a field on `AppState`) - this is expected; proceed to Step 3, which adds the field. Once it compiles, the test itself FAILS on the `assert!(cache_path.exists(), ...)` line after the first call, since `handle_face_image` doesn't write to the cache yet.

- [ ] **Step 3: Add `tmp_counter` to `AppState` and wire the handler to the cache**

Add the field to the `AppState` struct definition (~line 1805):

```rust
struct AppState {
    conn: Mutex<Connection>,
    shutdown_tx: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    report_all: bool,
    report_by_date: bool,
    report_heic: bool,
    report_heic_original: bool,
    serve_faces_ui: bool,
    tmp_counter: AtomicU64,
}
```

Add it to the `AppState` construction in `serve_faces_async` (~line 2445):

```rust
    let state = Arc::new(AppState {
        conn: Mutex::new(conn),
        shutdown_tx: Mutex::new(Some(shutdown_tx)),
        report_all: opts.report_all,
        report_by_date: opts.report_by_date,
        report_heic: opts.report_heic,
        report_heic_original: opts.report_heic_original,
        serve_faces_ui: opts.serve_faces_ui,
        tmp_counter: AtomicU64::new(0),
    });
```

Replace `handle_face_image` in full:

```rust
async fn handle_face_image(
    axum::extract::Path(face_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let (bbox_json, file_path, hash) = {
        let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.query_row(
            "SELECT f.bbox, fh.path, f.hash FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash \
             WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?
    };

    const FACE_THUMB_SIZE: u32 = 140;
    if videre_core::thumb_cache::face_thumb_exists(&hash, face_id, FACE_THUMB_SIZE) {
        if let Ok(bytes) =
            tokio::fs::read(videre_core::thumb_cache::face_thumb_path(&hash, face_id, FACE_THUMB_SIZE)).await
        {
            return Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes));
        }
    }

    // bbox stored as "x,y,w,h" → convert to x1,y1,x2,y2
    let parts: Vec<f32> = bbox_json
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    if parts.len() != 4 {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let bbox: [f32; 4] = [parts[0], parts[1], parts[0] + parts[2], parts[1] + parts[3]];

    let tmp_id = state.tmp_counter.fetch_add(1, Ordering::Relaxed);
    let jpeg_bytes = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let thumb = make_face_thumb(&file_path, bbox, face_id)?;
        let mut buf = Vec::new();
        thumb
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .ok()?;
        Some(buf)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Best-effort write-through: a cache write failure (disk full,
    // permissions) must not fail the request - caching is a performance
    // optimization, not a correctness requirement. The per-request `tmp_id`
    // suffix (distinct from the pid-only scheme `thumb_tmp_path` uses)
    // prevents two concurrent requests for the same uncached face from
    // colliding on the same tmp path and corrupting the cache.
    let final_path = videre_core::thumb_cache::face_thumb_path(&hash, face_id, FACE_THUMB_SIZE);
    let tmp_path = final_path.with_extension(format!("tmp{}-{}", std::process::id(), tmp_id));
    if let Some(parent) = final_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if tokio::fs::write(&tmp_path, &jpeg_bytes).await.is_ok() {
        let _ = tokio::fs::rename(&tmp_path, &final_path).await;
    }

    Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], jpeg_bytes))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p videre face_image_request_populates_and_then_hits_cache --lib`

Expected: PASS.

- [ ] **Step 5: Run the full test suite to check for regressions**

Run: `cargo test -p videre --lib`

Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "feat: cache face-crop thumbnails instead of reconverting on every request"
```

---

### Task 7: Wire `handle_original_image` into the cache

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (`handle_original_image`, ~line 2382)
- Test: `crates/videre/src/commands/report.rs` (`mod tests` block)

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn original_image_request_serves_cached_heic_without_reconversion() {
        let conn = mem_db_with_faces();
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) \
             VALUES ('/nonexistent/path/that/would/fail/to/convert.heic', 'origcachehash', 'heic')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (id, hash, bbox, embedding) VALUES (9002, 'origcachehash', '0,0,10,10', X'0000')",
            [],
        )
        .unwrap();
        let state = test_state(conn, true);

        let cache_path = videre_core::thumb_cache::original_path("origcachehash");
        std::fs::create_dir_all(videre_core::thumb_cache::cache_dir()).unwrap();
        std::fs::write(&cache_path, b"fake-cached-jpeg-bytes").unwrap();

        // The source file path doesn't exist, so a live-conversion attempt
        // would fail (NOT_FOUND) - success here proves the cache was used
        // instead of trying to convert the (nonexistent) source file.
        let result = handle_original_image(axum::extract::Path(9002), State(state.clone())).await;
        assert!(result.is_ok(), "must serve from cache instead of failing to convert a nonexistent source file");

        let _ = std::fs::remove_file(&cache_path);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre original_image_request_serves_cached_heic_without_reconversion --lib`

Expected: FAIL - current handler doesn't check the cache at all, so it attempts to convert the nonexistent source path and returns `NOT_FOUND`.

- [ ] **Step 3: Wire the handler to the cache**

Replace `handle_original_image` in full:

```rust
async fn handle_original_image(
    axum::extract::Path(face_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<impl axum::response::IntoResponse, StatusCode> {
    let (file_path, hash) = {
        let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        conn.query_row(
            "SELECT fh.path, f.hash FROM faces f \
             JOIN file_hashes fh ON f.hash = fh.hash \
             WHERE f.id = ?1 LIMIT 1",
            [face_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?
    };

    let ext = std::path::Path::new(&file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    if ext == "heic" {
        if let Ok(bytes) = tokio::fs::read(videre_core::thumb_cache::original_path(&hash)).await {
            return Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], bytes));
        }
    }

    let tmp_id = state.tmp_counter.fetch_add(1, Ordering::Relaxed);
    let hash_for_cache = hash.clone();
    let ext_for_cache = ext.clone();
    let (content_type, bytes) = tokio::task::spawn_blocking(move || -> Option<(&'static str, Vec<u8>)> {
        if ext == "heic" {
            let img = videre_core::heic::heic_via_quicklook(&file_path, &format!("orig{face_id}"))?;
            let mut buf = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Jpeg)
                .ok()?;
            Some(("image/jpeg", buf))
        } else {
            let bytes = std::fs::read(&file_path).ok()?;
            Some((mime_for_ext(&ext), bytes))
        }
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    // Only cache the HEIC-conversion case - non-HEIC files are served as raw
    // bytes with no conversion cost to save, and the read path above only
    // ever checks the cache when ext == "heic".
    if ext_for_cache == "heic" {
        let final_path = videre_core::thumb_cache::original_path(&hash_for_cache);
        let tmp_path = final_path.with_extension(format!("tmp{}-{}", std::process::id(), tmp_id));
        if let Some(parent) = final_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&tmp_path, &bytes).await.is_ok() {
            let _ = tokio::fs::rename(&tmp_path, &final_path).await;
        }
    }

    Ok(([(axum::http::header::CONTENT_TYPE, content_type)], bytes))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p videre original_image_request_serves_cached_heic_without_reconversion --lib`

Expected: PASS.

- [ ] **Step 5: Run the full test suite to check for regressions**

Run: `cargo test -p videre --lib`

Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "feat: cache full-resolution HEIC originals instead of reconverting on every request"
```

---

### Task 8: `PERSON_HTML` - Remove/Rename UI, `FACES_UI_ENABLED` gate, back-link referrer

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (`handle_person_page` ~line 2122; `PERSON_HTML` ~line 1636)
- Test: `crates/videre/src/commands/report.rs` (`mod tests` block)

This is the one task in this plan that mixes a Rust-testable change (the `FACES_UI_ENABLED` flag injection) with JS-only changes that have no automated test (verified manually in Task 12).

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn person_page_injects_faces_ui_enabled_true_when_serve_faces_ui() {
        let conn = mem_db_with_faces();
        let state = test_state(conn, true);
        let axum::response::Html(html) = handle_person_page(State(state)).await;
        assert!(html.contains("const FACES_UI_ENABLED = true;"), "{html}");
    }

    #[tokio::test]
    async fn person_page_injects_faces_ui_enabled_false_when_show_faces_only() {
        let conn = mem_db_with_faces();
        let state = test_state(conn, false);
        let axum::response::Html(html) = handle_person_page(State(state)).await;
        assert!(html.contains("const FACES_UI_ENABLED = false;"), "{html}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p videre person_page_injects --lib`

Expected: FAIL to compile - `handle_person_page` currently takes no arguments and returns `impl IntoResponse` (not the concrete `Html<String>` this test destructures), and the `__FACES_UI_ENABLED__` placeholder doesn't exist in `PERSON_HTML` yet.

- [ ] **Step 3: Update `handle_person_page`**

Replace it in full:

```rust
async fn handle_person_page(State(state): State<Arc<AppState>>) -> axum::response::Html<String> {
    let html = PERSON_HTML.replace(
        "__FACES_UI_ENABLED__",
        if state.serve_faces_ui { "true" } else { "false" },
    );
    axum::response::Html(html)
}
```

- [ ] **Step 4: Update `PERSON_HTML`'s toolbar markup**

Replace:

```html
  <div class="toolbar">
    <a href="/">&larr; Back to labeling</a>
    <strong id="person-title">Person</strong>
    <span id="face-count" style="color:#555;font-size:13px"></span>
    <span id="status"></span>
  </div>
```

with:

```html
  <div class="toolbar">
    <a id="backLink" href="/">&larr; Back to labeling</a>
    <strong id="person-title">Person</strong>
    <span id="face-count" style="color:#555;font-size:13px"></span>
    <span id="status"></span>
    <span id="renameArea" style="display:none">
      <input type="text" id="renameInput" maxlength="60">
      <button onclick="submitRename()">Save</button>
    </span>
    <button id="removeBtn" class="danger" style="display:none;margin-left:auto" onclick="removePerson()">Remove person</button>
  </div>
```

- [ ] **Step 5: Update `PERSON_HTML`'s script**

Replace the whole `<script>` block:

```html
  <script>
    const personName = decodeURIComponent(window.location.pathname.split('/').pop());
    let facesData = [];

    async function load() {
      try {
        document.getElementById('person-title').textContent = personName;
        document.title = personName;
        const r = await fetch(`/api/person/${encodeURIComponent(personName)}`);
        if (!r.ok) throw new Error('person fetch failed');
        const data = await r.json();
        facesData = data.faces;
        document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
        render();
      } catch(e) {
        document.getElementById('status').textContent = 'Error: ' + e;
      }
    }

    function render() {
      const grid = document.getElementById('faces-grid');
      grid.innerHTML = facesData.map(f => `
        <div class="card" id="card-${f.face_id}">
          <a href="/api/original-image/${f.face_id}" target="_blank" title="Open original image">
            <img class="face-img" src="/api/face-image/${f.face_id}" width="180" height="180"
                 onerror="this.removeAttribute('src');this.style.background='#ddd'">
          </a>
          <div class="path" title="${escHtml(f.path)}">${escHtml(basename(f.path))}</div>
          <div class="face-id">#${f.face_id}</div>
          <div class="btns">
            <button class="danger" onclick="removeFace(${f.face_id})">Remove</button>
          </div>
        </div>
      `).join('');
    }

    function basename(p) { return p.split('/').pop() || p; }

    function escHtml(s) {
      return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    }

    async function removeFace(faceId) {
      const r = await fetch('/api/remove-face', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_id: faceId })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: remove failed'; return; }
      document.getElementById(`card-${faceId}`)?.remove();
      facesData = facesData.filter(f => f.face_id !== faceId);
      document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
    }

    load();
  </script>
```

with:

```html
  <script>
    const personName = decodeURIComponent(window.location.pathname.split('/').pop());
    const FACES_UI_ENABLED = __FACES_UI_ENABLED__;
    const MAX_NAME_LEN = 60;
    let facesData = [];

    (function() {
      const params = new URLSearchParams(location.search);
      if (params.get('from') === 'lightbox') {
        const link = document.getElementById('backLink');
        link.textContent = '← Back';
        link.href = '#';
        link.onclick = function(e) { e.preventDefault(); history.back(); };
      }
    })();

    if (FACES_UI_ENABLED) {
      document.getElementById('removeBtn').style.display = 'inline-block';
      document.getElementById('renameArea').style.display = 'inline-flex';
    }

    // Trim, collapse internal whitespace, strip control/bidi-spoofing
    // characters, and cap length by code point - mirrors the sanitization in
    // FACES_HTML/CLUSTER_HTML.
    function sanitizeName(raw) {
      const filtered = Array.from(raw).filter(function(ch) {
        const cp = ch.codePointAt(0);
        if (cp < 0x20 || (cp >= 0x7f && cp <= 0x9f)) return false;
        if (cp === 0x200B) return false;
        if (cp === 0x200E || cp === 0x200F) return false;
        if (cp >= 0x202A && cp <= 0x202E) return false;
        if (cp >= 0x2060 && cp <= 0x2069) return false;
        if (cp === 0xFEFF) return false;
        return true;
      }).join('');
      const collapsed = filtered.trim().replace(/\s+/g, ' ');
      return Array.from(collapsed).slice(0, MAX_NAME_LEN).join('');
    }

    async function load() {
      try {
        document.getElementById('person-title').textContent = personName;
        document.title = personName;
        document.getElementById('renameInput').value = personName;
        const r = await fetch(`/api/person/${encodeURIComponent(personName)}`);
        if (!r.ok) throw new Error('person fetch failed');
        const data = await r.json();
        facesData = data.faces;
        document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
        render();
      } catch(e) {
        document.getElementById('status').textContent = 'Error: ' + e;
      }
    }

    function render() {
      const grid = document.getElementById('faces-grid');
      grid.innerHTML = facesData.map(f => `
        <div class="card" id="card-${f.face_id}">
          <a href="/api/original-image/${f.face_id}" target="_blank" title="Open original image">
            <img class="face-img" src="/api/face-image/${f.face_id}" width="180" height="180"
                 onerror="this.removeAttribute('src');this.style.background='#ddd'">
          </a>
          <div class="path" title="${escHtml(f.path)}">${escHtml(basename(f.path))}</div>
          <div class="face-id">#${f.face_id}</div>
          <div class="btns">
            <button class="danger" onclick="removeFace(${f.face_id})">Remove</button>
          </div>
        </div>
      `).join('');
    }

    function basename(p) { return p.split('/').pop() || p; }

    function escHtml(s) {
      return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    }

    async function removeFace(faceId) {
      const r = await fetch('/api/remove-face', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_id: faceId })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: remove failed'; return; }
      document.getElementById(`card-${faceId}`)?.remove();
      facesData = facesData.filter(f => f.face_id !== faceId);
      document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
    }

    async function removePerson() {
      if (!confirm('Remove ' + personName + '? Their ' + facesData.length + ' photo(s) will become unassigned.')) return;
      const r = await fetch('/api/delete-person', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ label: personName })
      });
      if (!r.ok) { alert('Failed to remove person.'); return; }
      window.location.href = '/';
    }

    async function submitRename() {
      const newLabel = sanitizeName(document.getElementById('renameInput').value);
      if (!newLabel) return;
      const r = await fetch('/api/rename-person', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ old_label: personName, new_label: newLabel })
      });
      if (r.status === 409) { alert('A person named "' + newLabel + '" already exists.'); return; }
      if (!r.ok) { alert('Rename failed.'); return; }
      window.location.href = '/person/' + encodeURIComponent(newLabel);
    }

    load();
  </script>
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p videre person_page_injects --lib`

Expected: both PASS.

- [ ] **Step 7: Run the full test suite to check for regressions**

Run: `cargo test -p videre --lib`

Expected: all tests PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "feat: add remove/rename person UI and lightbox back-link referrer"
```

---

### Task 9: Fix `renderMetaPanel` escaping and add `?from=lightbox`

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (`renderMetaPanel`, ~line 943, inside `generate_html`)
- Test: `crates/videre/src/commands/report.rs` (`mod tests` block)

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn generated_html_links_person_faces_with_from_lightbox_and_escapes_name() {
        let stats = Stats { total_files: 0, duplicate_groups: 0, duplicate_files: 0, wasted_bytes: 0 };
        let html = generate_html("/tmp/test.db", &stats, &[], None, None, None, false, false, &HashMap::new(), true);
        assert!(
            html.contains("'?from=lightbox">'+escH(fc.name)+'</a>"),
            "person link in the lightbox meta panel must carry ?from=lightbox and escape the name"
        );
        assert!(
            html.contains("<img src=\"'+escA(fc.thumb)+'\">"),
            "face thumbnail src must be escaped too"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p videre generated_html_links_person_faces_with_from_lightbox_and_escapes_name --lib`

Expected: FAIL - the substrings don't exist in the current output (no `?from=lightbox`, and `fc.name`/`fc.thumb` are currently unescaped).

- [ ] **Step 3: Fix `renderMetaPanel`**

Replace:

```js
function renderMetaPanel(meta){
  var el = document.getElementById('lbMeta');
  if(!meta || (!meta.faces.length && !meta.location)){
    el.classList.remove('on'); el.innerHTML=''; return;
  }
  var parts = [];
  if(meta.faces.length){
    parts.push(meta.faces.map(function(fc){
      return '<div class="lb-face"><img src="'+fc.thumb+'">'+
        '<a href="/person/'+encodeURIComponent(fc.name)+'">'+fc.name+'</a></div>';
    }).join(''));
  }
```

with:

```js
function renderMetaPanel(meta){
  var el = document.getElementById('lbMeta');
  if(!meta || (!meta.faces.length && !meta.location)){
    el.classList.remove('on'); el.innerHTML=''; return;
  }
  var parts = [];
  if(meta.faces.length){
    parts.push(meta.faces.map(function(fc){
      return '<div class="lb-face"><img src="'+escA(fc.thumb)+'">'+
        '<a href="/person/'+encodeURIComponent(fc.name)+'?from=lightbox">'+escH(fc.name)+'</a></div>';
    }).join(''));
  }
```

Note this function is part of the main report/lightbox script (`out.push_str(r#"..."#)` inside `generate_html`), not `FACES_HTML`/`CLUSTER_HTML`/`PERSON_HTML` - it uses `escA`/`escH` (defined a few lines above it at ~line 798-803), not the separately-defined `escHtml` used inside the labeling-UI templates.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p videre generated_html_links_person_faces_with_from_lightbox_and_escapes_name --lib`

Expected: PASS.

- [ ] **Step 5: Run the full test suite to check for regressions**

Run: `cargo test -p videre --lib`

Expected: all tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "fix: escape person name/thumbnail in lightbox meta panel, add lightbox referrer"
```

---

### Task 10: Enter-to-submit on New Person inputs

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (`FACES_HTML`'s `showNewPersonInput`, ~line 1448; `CLUSTER_HTML`'s script, ~line 1531)

No automated test - pure JS/DOM event wiring with no browser test harness in this project. Verified manually in Task 12.

- [ ] **Step 1: Update `FACES_HTML`'s `showNewPersonInput`**

Replace:

```js
    function showNewPersonInput(btn, faceIds) {
      const area = btn.parentElement;
      const faceIdsJson = JSON.stringify(faceIds);
      area.innerHTML = `
        <input type="text" class="np-input" id="np-input-${faceIds[0]}" placeholder="Person name" maxlength="${MAX_NAME_LEN}" autofocus>
        <div class="np-btn-row">
          <button class="np-create-btn" onclick="submitNewPerson('np-input-${faceIds[0]}', ${faceIdsJson})">Create</button>
          <button class="new-person-btn" onclick="loadFaces()">Cancel</button>
        </div>
      `;
    }
```

with:

```js
    function showNewPersonInput(btn, faceIds) {
      const area = btn.parentElement;
      const faceIdsJson = JSON.stringify(faceIds);
      const inputId = `np-input-${faceIds[0]}`;
      area.innerHTML = `
        <input type="text" class="np-input" id="${inputId}" placeholder="Person name" maxlength="${MAX_NAME_LEN}" autofocus>
        <div class="np-btn-row">
          <button class="np-create-btn" onclick="submitNewPerson('${inputId}', ${faceIdsJson})">Create</button>
          <button class="new-person-btn" onclick="loadFaces()">Cancel</button>
        </div>
      `;
      document.getElementById(inputId).addEventListener('keydown', function(e) {
        if (e.key === 'Enter') { e.preventDefault(); submitNewPerson(inputId, faceIds); }
      });
    }
```

(The input is injected dynamically at click time, so the listener must be attached right after `innerHTML` is set - attaching it at page load, before the element exists, would silently do nothing.)

- [ ] **Step 2: Update `CLUSTER_HTML`'s bulk-assign field**

`#person-input` is static markup (not dynamically injected), so its listener is attached once, at script-load time. Add this line right after the `sanitizeName` function definition (the one updated in Task 1), before `async function load() {`:

```js
    document.getElementById('person-input').addEventListener('keydown', function(e) {
      if (e.key === 'Enter') { e.preventDefault(); assignAll(); }
    });
```

- [ ] **Step 3: Build to confirm no compile errors**

Run: `cargo build -p videre`

Expected: builds cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "feat: Enter key submits New Person inputs"
```

---

### Task 11: Replace `prompt()` with a real assign modal

**Files:**
- Modify: `crates/videre/src/commands/report.rs` (`CLUSTER_HTML`'s `<style>`, markup, and script)

No automated test - pure JS/DOM/CSS with no browser test harness in this project. Verified manually in Task 12.

- [ ] **Step 1: Add modal CSS**

Add to `CLUSTER_HTML`'s `<style>` block, after the existing `#status` rule:

```css
    .modal-backdrop { display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.4); align-items: center; justify-content: center; z-index: 100; }
    .modal-backdrop.on { display: flex; }
    .modal { background: white; border-radius: 8px; padding: 20px; min-width: 280px; }
    .modal h3 { margin: 0 0 12px; font-size: 15px; }
    .modal input { width: 100%; box-sizing: border-box; margin-bottom: 12px; }
    .modal-actions { display: flex; gap: 8px; justify-content: flex-end; }
```

- [ ] **Step 2: Add modal markup**

Add right before the `<script>` tag, after `<div id="faces-grid" class="grid"></div>`:

```html
  <div id="assignModal" class="modal-backdrop">
    <div class="modal">
      <h3>Assign to person</h3>
      <input id="assignInput" list="assign-people-list" placeholder="Person name" maxlength="60">
      <datalist id="assign-people-list"></datalist>
      <div class="modal-actions">
        <button onclick="submitAssignModal()">Assign</button>
        <button onclick="closeAssignModal()">Cancel</button>
      </div>
    </div>
  </div>
```

- [ ] **Step 3: Hoist `mainData` to module scope**

Replace:

```js
    const clusterId = __CLUSTER_ID__;
    let facesData = [];
    const MAX_NAME_LEN = 60;
```

with:

```js
    const clusterId = __CLUSTER_ID__;
    let facesData = [];
    let mainData = { people: [] };
    const MAX_NAME_LEN = 60;
```

Then, inside `load()`, change:

```js
        const mainData = mainRes.ok ? await mainRes.json() : { people: [] };
```

to:

```js
        mainData = mainRes.ok ? await mainRes.json() : { people: [] };
```

(drop the `const` - it now assigns the module-scoped variable declared above instead of shadowing it locally, so `openAssignModal` can read the same people list `load()` already fetched).

- [ ] **Step 4: Add the modal functions and wire up `assignOne`**

Add after `assignAll()`:

```js
    let assignModalFaceId = null;

    function openAssignModal(faceId) {
      assignModalFaceId = faceId;
      document.getElementById('assign-people-list').innerHTML =
        mainData.people.map(p => `<option value="${escHtml(p.label)}">`).join('');
      document.getElementById('assignModal').classList.add('on');
      document.getElementById('assignInput').value = '';
      document.getElementById('assignInput').focus();
    }

    function closeAssignModal() {
      document.getElementById('assignModal').classList.remove('on');
      assignModalFaceId = null;
    }

    async function submitAssignModal() {
      const label = sanitizeName(document.getElementById('assignInput').value);
      if (!label) return;
      const faceId = assignModalFaceId;
      const r = await fetch('/api/assign', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ face_ids: [faceId], person_label: label })
      });
      if (!r.ok) { document.getElementById('status').textContent = 'Error: assign failed'; return; }
      closeAssignModal();
      document.getElementById(`card-${faceId}`)?.remove();
      facesData = facesData.filter(f => f.face_id !== faceId);
      document.getElementById('face-count').textContent = `${facesData.length} face(s)`;
    }

    document.getElementById('assignInput').addEventListener('keydown', function(e) {
      if (e.key === 'Enter') { e.preventDefault(); submitAssignModal(); }
    });
```

Replace `assignOne` in full:

```js
    function assignOne(faceId) {
      openAssignModal(faceId);
    }
```

- [ ] **Step 5: Build to confirm no compile errors**

Run: `cargo build -p videre`

Expected: builds cleanly.

- [ ] **Step 6: Commit**

```bash
git add crates/videre/src/commands/report.rs
git commit -m "feat: replace prompt()-based assign flow with a real modal"
```

---

### Task 12: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Run the full workspace test suite**

Run: `cargo test --workspace`

Expected: all tests PASS, including every new test added in Tasks 1-9.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`

Expected: no warnings. Fix anything flagged before proceeding.

- [ ] **Step 3: Run fmt check**

Run: `cargo fmt --check`

Expected: no diff. Run `cargo fmt` and commit separately if it reformats anything.

- [ ] **Step 4: Build the release binary**

Run: `cargo build --release`

Expected: builds cleanly.

- [ ] **Step 5: Manual smoke test**

Using a real (or freshly seeded test) database with at least one labeled person, one unassigned cluster, and one singleton:

```bash
./target/release/videre report --db <db> --faces
```

Walk through, in order:

1. **Enter-to-submit**: click "New Person" on a cluster/singleton card, type a name, press Enter (not click Create) - person should be created.
2. **Assign modal**: open a cluster detail page, click "Assign" on a singleton face - a real modal should appear (not a browser `prompt()`), with a datalist of existing people; type a name and press Enter - face should be assigned and the card removed.
3. **Remove person**: open a person page (`/person/<name>`), click "Remove person", confirm - should redirect to `/` and the person's faces should reappear under Clusters/Singletons (not disappear).
4. **Rename person**: open a person page, change the name in the rename field, click Save - should redirect to `/person/<new-name>` with all faces intact. Try renaming to an existing person's name - should show an alert and not navigate away.
5. **Sanitization**: try entering an emoji name with a ZWJ sequence (e.g. a family emoji) - should be accepted intact. Try pasting a name containing a right-to-left override character - should be stripped silently.
6. **Face-crop caching**: open a person or cluster page, reload it - face thumbnails on the second load should appear noticeably faster (served from `~/.cache/videre/thumbnails/`, confirm new `<hash>_face<id>_140.jpg` files exist there after the first load).
7. **Back-link referrer**: start the server with both `--faces --show-faces`, open `/`, open the lightbox on a photo with a labeled face, click the face's name link to reach `/person/<name>`, confirm the toolbar shows "← Back" (not "← Back to labeling") and clicking it returns to the lightbox view rather than navigating to `/`. Then visit a person page directly (not via the lightbox) and confirm it still shows "← Back to labeling" linking to `/`.
8. **`--show-faces`-only gap**: start the server with `--show-faces` alone (no `--faces`), navigate to a person page - confirm the Remove-person and Rename-person controls are hidden (not present, not visibly broken).

Report any failures found during this pass before considering the slice done.

- [ ] **Step 6: Commit** (only if `cargo fmt` produced changes in Step 3)

```bash
git add -A
git commit -m "chore: cargo fmt"
```
