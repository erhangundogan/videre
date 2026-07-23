# videre-api Facade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the faces labeling operations currently living inside the `videre report --faces` axum handlers into a new `videre-api` crate of plain, unit-testable functions, and rewrite those handlers to delegate to it - one source of truth for both the existing server and the future Tauri desktop app.

**Architecture:** New `videre-api` crate holds serde response types, a shared `Error` enum, and one function per operation taking `&rusqlite::Connection`. The `videre` crate's axum handlers in `report.rs` become thin wrappers that lock the connection, call `videre-api`, and map `videre_api::Error` to `StatusCode` (preserving today's 200/400/404/409/500 behavior). No behavior change for users; the existing `faces_server` integration tests stay green.

**Tech Stack:** Rust, `rusqlite` (bundled), `serde`, existing `videre-core` (for `person_search`). This is Plan 1 of 3 (see `docs/superpowers/specs/2026-07-23-desktop-app-design.md`); Plans 2-3 add the Tauri app and React UI.

**Scope note:** This plan covers the 11 JSON/data operations plus `sanitize_person_label`. The two image-bytes operations (`face_image_bytes`, `original_image_bytes`) are deferred to Plan 2, where the Tauri image protocol consumes them.

---

## File Structure

- Create: `crates/videre-api/Cargo.toml` - new crate manifest
- Create: `crates/videre-api/src/lib.rs` - crate root: modules, re-exports
- Create: `crates/videre-api/src/error.rs` - `Error` enum, `Result` alias
- Create: `crates/videre-api/src/types.rs` - serde response structs
- Create: `crates/videre-api/src/label.rs` - `sanitize_person_label` + tests
- Create: `crates/videre-api/src/faces.rs` - the 11 operations + tests
- Modify: `Cargo.toml` (workspace root) - add `crates/videre-api` to members
- Modify: `crates/videre/Cargo.toml` - add `videre-api` path dependency
- Modify: `crates/videre/src/commands/report.rs` - handlers delegate; remove the moved type/fn definitions and import them from `videre-api`

---

## Task 1: Scaffold the videre-api crate

**Files:**
- Create: `crates/videre-api/Cargo.toml`
- Create: `crates/videre-api/src/lib.rs`
- Create: `crates/videre-api/src/error.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create the crate manifest**

Create `crates/videre-api/Cargo.toml`:

```toml
[package]
name = "videre-api"
version = "0.3.2"
edition = "2021"

[dependencies]
rusqlite = { version = "0.32", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
videre-core = { path = "../videre-core" }
```

- [ ] **Step 2: Add the crate to the workspace members**

In the root `Cargo.toml`, add `"crates/videre-api"` to the `members` array (keep the existing entries).

- [ ] **Step 3: Create the error type**

Create `crates/videre-api/src/error.rs`:

```rust
/// Errors returned by videre-api operations. Each consumer maps these to its
/// own convention (axum -> StatusCode, Tauri -> a serializable error).
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
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotFound => write!(f, "not found"),
            Error::Conflict => write!(f, "conflict"),
            Error::Invalid => write!(f, "invalid input"),
            Error::Db(e) => write!(f, "database error: {e}"),
        }
    }
}

impl std::error::Error {}

pub type Result<T> = std::result::Result<T, Error>;
```

Note: replace `impl std::error::Error {}` with the correct impl:

```rust
impl std::error::Error for Error {}
```

- [ ] **Step 4: Create the crate root**

Create `crates/videre-api/src/lib.rs`:

```rust
//! Facade over videre's faces-labeling operations. Plain functions over an
//! open `rusqlite::Connection`, returning serde types and a shared `Error`.
//! Called by both the axum `--faces` server and the Tauri desktop app.

mod error;
mod faces;
mod label;
mod types;

pub use error::{Error, Result};
pub use faces::{
    assign, cluster_detail, delete_person, dissolve_cluster, faces_list, new_person,
    person_detail, remove_face, rename_person, search_person, set_primary,
};
pub use label::sanitize_person_label;
pub use types::{
    ClusterData, ClusterDetail, ClusterFaceData, FacesData, PersonData, PersonDetail,
    PersonFaceData, SingletonData,
};
```

- [ ] **Step 5: Create placeholder modules so it compiles**

Create empty `crates/videre-api/src/types.rs`, `crates/videre-api/src/label.rs`, and `crates/videre-api/src/faces.rs` each containing only a line comment for now (the real content lands in later tasks). To make Step 4's `pub use` compile at this point, temporarily comment out the `pub use faces::…`, `pub use label::…`, and `pub use types::…` lines; they are uncommented as each module is filled in.

- [ ] **Step 6: Verify it builds**

Run: `cargo build -p videre-api`
Expected: compiles cleanly (an empty library).

- [ ] **Step 7: Commit**

```bash
git add crates/videre-api Cargo.toml
git commit -m "feat(videre-api): scaffold facade crate with error type"
```

---

## Task 2: Response types

Move the serde response structs from `report.rs` into `videre-api`, adding `Clone`. `report.rs` will import them from `videre-api` instead of defining them.

**Files:**
- Modify: `crates/videre-api/src/types.rs`
- Modify: `crates/videre-api/src/lib.rs` (uncomment the `pub use types::…` line)

- [ ] **Step 1: Write the response types**

Replace the contents of `crates/videre-api/src/types.rs`:

```rust
use serde::Serialize;

/// One labeled person: their confirmed faces plus a representative face id
/// (the primary, or lowest id) used as the card thumbnail.
#[derive(Serialize, Clone)]
pub struct PersonData {
    pub label: String,
    pub face_ids: Vec<i64>,
    pub representative_id: i64,
    pub hashes: Vec<String>,
}

/// One unassigned cluster (green section in the labeling UI).
#[derive(Serialize, Clone)]
pub struct ClusterData {
    pub cluster_id: i64,
    pub face_ids: Vec<i64>,
    pub hashes: Vec<String>,
}

/// One unclustered, unassigned face (orange section).
#[derive(Serialize, Clone)]
pub struct SingletonData {
    pub face_id: i64,
    pub hash: String,
}

/// Top-level payload for the labeling page.
#[derive(Serialize, Clone)]
pub struct FacesData {
    pub people: Vec<PersonData>,
    pub clusters: Vec<ClusterData>,
    pub singletons: Vec<SingletonData>,
}

/// One face row on a cluster detail page.
#[derive(Serialize, Clone)]
pub struct ClusterFaceData {
    pub face_id: i64,
    pub hash: String,
    pub path: String,
}

/// Cluster detail: every face in one unassigned cluster.
#[derive(Serialize, Clone)]
pub struct ClusterDetail {
    pub cluster_id: i64,
    pub faces: Vec<ClusterFaceData>,
}

/// One face row on a person detail page. `is_primary` marks the current
/// default photo (the person's thumbnail on the labeling page).
#[derive(Serialize, Clone)]
pub struct PersonFaceData {
    pub face_id: i64,
    pub hash: String,
    pub path: String,
    pub is_primary: bool,
}

/// Person detail: every confirmed face for one person.
#[derive(Serialize, Clone)]
pub struct PersonDetail {
    pub label: String,
    pub faces: Vec<PersonFaceData>,
}
```

- [ ] **Step 2: Uncomment the types re-export**

In `crates/videre-api/src/lib.rs`, uncomment the `pub use types::{…};` line from Task 1 Step 4.

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p videre-api`
Expected: compiles cleanly.

- [ ] **Step 4: Commit**

```bash
git add crates/videre-api/src/types.rs crates/videre-api/src/lib.rs
git commit -m "feat(videre-api): response types for faces/cluster/person"
```

---

## Task 3: sanitize_person_label

Move label sanitization (and its tests) out of `report.rs` into `videre-api`.

**Files:**
- Modify: `crates/videre-api/src/label.rs`
- Modify: `crates/videre-api/src/lib.rs` (uncomment `pub use label::…`)

- [ ] **Step 1: Write the failing test**

Put this at the bottom of `crates/videre-api/src/label.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::sanitize_person_label;

    #[test]
    fn trims_collapses_and_caps() {
        assert_eq!(sanitize_person_label("  Alice   B  ").as_deref(), Some("Alice B"));
        assert_eq!(sanitize_person_label("   ").as_deref(), None);
        assert_eq!(sanitize_person_label(&"x".repeat(70)).unwrap().chars().count(), 60);
    }

    #[test]
    fn strips_bidi_override() {
        assert_eq!(sanitize_person_label("A\u{202E}lice").as_deref(), Some("Alice"));
    }

    #[test]
    fn keeps_zwj_emoji_sequences() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(sanitize_person_label(family).as_deref(), Some(family));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p videre-api label`
Expected: FAIL to compile - `sanitize_person_label` not defined.

- [ ] **Step 3: Implement (move from report.rs)**

Put this above the test module in `crates/videre-api/src/label.rs`:

```rust
/// Trim, collapse internal whitespace, and cap length (60 code points) so a
/// caller that bypasses UI sanitization can't stretch layout or bloat the DB.
/// Returns None when nothing usable remains. Filters control and bidi/
/// zero-width format characters but deliberately keeps U+200C (ZWNJ) and
/// U+200D (ZWJ), which are required for Persian/Indic text and emoji ZWJ
/// sequences. Not homoglyph-proof, and the cap truncates by code point.
pub fn sanitize_person_label(raw: &str) -> Option<String> {
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

fn is_disallowed_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'
        | '\u{200E}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2069}'
        | '\u{FEFF}'
    )
}
```

Uncomment `pub use label::sanitize_person_label;` in `lib.rs`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p videre-api label`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-api/src/label.rs crates/videre-api/src/lib.rs
git commit -m "feat(videre-api): move sanitize_person_label with tests"
```

---

## Task 4: Read operations (faces_list, cluster_detail, person_detail, search_person)

**Files:**
- Modify: `crates/videre-api/src/faces.rs`
- Modify: `crates/videre-api/src/lib.rs` (uncomment `pub use faces::…`)

- [ ] **Step 1: Write a shared test helper + the first failing test**

Put at the top of `crates/videre-api/src/faces.rs`:

```rust
use crate::error::{Error, Result};
use crate::types::*;
use rusqlite::Connection;

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory db with the faces + file_hashes tables and a few rows:
    /// - face 1: person "Alice", confirmed, is_primary
    /// - face 2: person "Alice", confirmed
    /// - face 3: cluster 7 (unassigned)
    /// - face 4: cluster 7 (unassigned)
    /// - face 5: singleton (no cluster, unassigned)
    pub(super) fn seed() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        videre_core::face_db::create_faces_table(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (hash TEXT PRIMARY KEY, path TEXT);
             INSERT INTO file_hashes VALUES ('h1','/p/1.jpg'),('h2','/p/2.jpg'),
                ('h3','/p/3.jpg'),('h4','/p/4.jpg'),('h5','/p/5.jpg');
             INSERT INTO faces (id,hash,bbox,embedding,cluster_id,person_label,confirmed,is_primary) VALUES
                (1,'h1','0,0,9,9',X'0000',NULL,'Alice',1,1),
                (2,'h2','0,0,9,9',X'0000',NULL,'Alice',1,0),
                (3,'h3','0,0,9,9',X'0000',7,NULL,0,0),
                (4,'h4','0,0,9,9',X'0000',7,NULL,0,0),
                (5,'h5','0,0,9,9',X'0000',NULL,NULL,0,0);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn faces_list_splits_people_clusters_singletons() {
        let conn = seed();
        let d = faces_list(&conn).unwrap();
        assert_eq!(d.people.len(), 1);
        assert_eq!(d.people[0].label, "Alice");
        assert_eq!(d.people[0].representative_id, 1, "primary face is representative");
        assert_eq!(d.clusters.len(), 1);
        assert_eq!(d.clusters[0].cluster_id, 7);
        assert_eq!(d.clusters[0].face_ids, vec![3, 4]);
        assert_eq!(d.singletons.len(), 1);
        assert_eq!(d.singletons[0].face_id, 5);
    }

    #[test]
    fn person_detail_marks_primary() {
        let conn = seed();
        let p = person_detail(&conn, "Alice").unwrap();
        assert_eq!(p.faces.len(), 2);
        assert!(p.faces[0].is_primary, "primary sorts first and is flagged");
        assert!(!p.faces[1].is_primary);
    }

    #[test]
    fn cluster_detail_lists_faces() {
        let conn = seed();
        let c = cluster_detail(&conn, 7).unwrap();
        assert_eq!(c.cluster_id, 7);
        assert_eq!(c.faces.iter().map(|f| f.face_id).collect::<Vec<_>>(), vec![3, 4]);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p videre-api faces`
Expected: FAIL to compile - `faces_list`/`person_detail`/`cluster_detail` not defined.

- [ ] **Step 3: Implement the read operations**

Put above the test module in `crates/videre-api/src/faces.rs`:

```rust
use std::collections::HashMap;

/// People / unassigned clusters / singletons for the labeling page.
pub fn faces_list(conn: &Connection) -> Result<FacesData> {
    let mut people: HashMap<String, PersonData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash, person_label FROM faces \
             WHERE confirmed = 1 AND person_label IS NOT NULL \
             ORDER BY person_label, is_primary DESC, id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (id, hash, label) = row?;
            let person = people.entry(label.clone()).or_insert(PersonData {
                label: label.clone(),
                face_ids: vec![],
                representative_id: id,
                hashes: vec![],
            });
            person.face_ids.push(id);
            if !person.hashes.contains(&hash) {
                person.hashes.push(hash);
            }
        }
    }

    let mut cluster_map: HashMap<i64, ClusterData> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash, cluster_id FROM faces \
             WHERE cluster_id IS NOT NULL AND (confirmed = 0 OR person_label IS NULL) \
             ORDER BY cluster_id, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?;
        for row in rows {
            let (id, hash, cid) = row?;
            let cluster = cluster_map.entry(cid).or_insert(ClusterData {
                cluster_id: cid,
                face_ids: vec![],
                hashes: vec![],
            });
            cluster.face_ids.push(id);
            if !cluster.hashes.contains(&hash) {
                cluster.hashes.push(hash);
            }
        }
    }

    let mut singletons: Vec<SingletonData> = vec![];
    {
        let mut stmt = conn.prepare(
            "SELECT id, hash FROM faces \
             WHERE cluster_id IS NULL AND (confirmed = 0 OR person_label IS NULL) \
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (id, hash) = row?;
            singletons.push(SingletonData { face_id: id, hash });
        }
    }

    Ok(FacesData {
        people: people.into_values().collect(),
        clusters: cluster_map.into_values().collect(),
        singletons,
    })
}

/// Every face in one unassigned cluster (for the cluster detail page).
pub fn cluster_detail(conn: &Connection, cluster_id: i64) -> Result<ClusterDetail> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.hash, fh.path FROM faces f \
         JOIN file_hashes fh ON f.hash = fh.hash \
         WHERE f.cluster_id = ?1 ORDER BY f.id",
    )?;
    let faces = stmt
        .query_map([cluster_id], |r| {
            Ok(ClusterFaceData { face_id: r.get(0)?, hash: r.get(1)?, path: r.get(2)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ClusterDetail { cluster_id, faces })
}

/// Every confirmed face for one person, primary first and flagged.
pub fn person_detail(conn: &Connection, name: &str) -> Result<PersonDetail> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.hash, fh.path, f.is_primary FROM faces f \
         JOIN file_hashes fh ON f.hash = fh.hash \
         WHERE f.person_label = ?1 AND f.confirmed = 1 \
         ORDER BY f.is_primary DESC, f.id",
    )?;
    let faces = stmt
        .query_map([name], |r| {
            Ok(PersonFaceData {
                face_id: r.get(0)?,
                hash: r.get(1)?,
                path: r.get(2)?,
                is_primary: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(PersonDetail { label: name.to_string(), faces })
}

/// Image paths for confirmed faces of a person (prefix match), for the
/// person-name autocomplete. Delegates to the existing core search.
pub fn search_person(conn: &Connection, name: &str) -> Result<Vec<String>> {
    Ok(videre_core::person_search::search_by_person(conn, name, None)?)
}
```

Uncomment `pub use faces::{…};` in `lib.rs`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p videre-api faces`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-api/src/faces.rs crates/videre-api/src/lib.rs
git commit -m "feat(videre-api): read ops (faces_list, cluster_detail, person_detail, search_person)"
```

---

## Task 5: Simple mutations (assign, new_person, remove_face, dissolve_cluster, delete_person)

**Files:**
- Modify: `crates/videre-api/src/faces.rs`

- [ ] **Step 1: Write the failing tests**

Add these tests inside the existing `mod tests` in `crates/videre-api/src/faces.rs`:

```rust
    #[test]
    fn assign_labels_and_confirms() {
        let conn = seed();
        assign(&conn, &[3, 4], "Bob").unwrap();
        let p = person_detail(&conn, "Bob").unwrap();
        assert_eq!(p.faces.len(), 2, "both faces now confirmed under Bob");
    }

    #[test]
    fn assign_rejects_empty_label() {
        let conn = seed();
        assert!(matches!(assign(&conn, &[3], "   "), Err(Error::Invalid)));
    }

    #[test]
    fn remove_face_unassigns_everything() {
        let conn = seed();
        remove_face(&conn, 1).unwrap();
        let (cid, label, confirmed, prim): (Option<i64>, Option<String>, i64, i64) = conn
            .query_row(
                "SELECT cluster_id, person_label, confirmed, is_primary FROM faces WHERE id=1",
                [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        assert_eq!((cid, label, confirmed, prim), (None, None, 0, 0));
    }

    #[test]
    fn dissolve_cluster_nulls_cluster_id() {
        let conn = seed();
        dissolve_cluster(&conn, 7).unwrap();
        assert_eq!(faces_list(&conn).unwrap().clusters.len(), 0);
        assert_eq!(faces_list(&conn).unwrap().singletons.len(), 3, "3,4 join 5 as singletons");
    }

    #[test]
    fn delete_person_unassigns_without_touching_cluster() {
        let conn = seed();
        delete_person(&conn, "Alice").unwrap();
        assert_eq!(faces_list(&conn).unwrap().people.len(), 0);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p videre-api faces`
Expected: FAIL to compile - the mutation functions are not defined.

- [ ] **Step 3: Implement the mutations**

Add to `crates/videre-api/src/faces.rs` (below the read ops):

```rust
/// Assign faces to an existing/new person: sets person_label + confirmed.
/// Rejects an empty label after sanitizing.
pub fn assign(conn: &Connection, face_ids: &[i64], person_label: &str) -> Result<()> {
    let label = crate::label::sanitize_person_label(person_label).ok_or(Error::Invalid)?;
    for id in face_ids {
        conn.execute(
            "UPDATE faces SET person_label = ?1, confirmed = 1 WHERE id = ?2",
            rusqlite::params![label, id],
        )?;
    }
    Ok(())
}

/// Create a person from faces. Same effect as `assign`; kept as a distinct
/// operation because callers treat "new person" and "assign to existing" as
/// separate user intents.
pub fn new_person(conn: &Connection, face_ids: &[i64], label: &str) -> Result<()> {
    assign(conn, face_ids, label)
}

/// Reset one face to fully unassigned (cluster, label, confirmed, primary).
pub fn remove_face(conn: &Connection, face_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE faces SET cluster_id = NULL, person_label = NULL, confirmed = 0, is_primary = 0 WHERE id = ?1",
        [face_id],
    )?;
    Ok(())
}

/// Ungroup a bad cluster: its faces become unassigned singletons (not deleted).
pub fn dissolve_cluster(conn: &Connection, cluster_id: i64) -> Result<()> {
    conn.execute("UPDATE faces SET cluster_id = NULL WHERE cluster_id = ?1", [cluster_id])?;
    Ok(())
}

/// Reset every face of a person back to unassigned. Deliberately does NOT touch
/// cluster_id, so a face rejoins its cluster's unassigned group rather than
/// scattering to singletons.
pub fn delete_person(conn: &Connection, label: &str) -> Result<()> {
    conn.execute(
        "UPDATE faces SET person_label = NULL, confirmed = 0, is_primary = 0 WHERE person_label = ?1",
        rusqlite::params![label],
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p videre-api faces`
Expected: PASS (all faces tests).

- [ ] **Step 5: Commit**

```bash
git add crates/videre-api/src/faces.rs
git commit -m "feat(videre-api): mutations (assign, new_person, remove_face, dissolve_cluster, delete_person)"
```

---

## Task 6: Guarded mutations (set_primary, rename_person)

These carry invariants: `set_primary` keeps exactly one primary per person via a transaction; `rename_person` returns `NotFound`/`Conflict`.

**Files:**
- Modify: `crates/videre-api/src/faces.rs`

- [ ] **Step 1: Write the failing tests**

Add inside `mod tests`:

```rust
    #[test]
    fn set_primary_is_exclusive_per_person() {
        let conn = seed();
        set_primary(&conn, 2, "Alice").unwrap();
        let primaries: Vec<i64> = {
            let mut s = conn.prepare("SELECT id FROM faces WHERE person_label='Alice' AND is_primary=1").unwrap();
            s.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(primaries, vec![2], "exactly one primary, now face 2");
    }

    #[test]
    fn rename_missing_person_is_not_found() {
        let conn = seed();
        assert!(matches!(rename_person(&conn, "Nobody", "X"), Err(Error::NotFound)));
    }

    #[test]
    fn rename_onto_existing_person_conflicts() {
        let conn = seed();
        assign(&conn, &[3], "Bob").unwrap(); // Bob now exists
        assert!(matches!(rename_person(&conn, "Alice", "Bob"), Err(Error::Conflict)));
    }

    #[test]
    fn rename_succeeds() {
        let conn = seed();
        rename_person(&conn, "Alice", "Alicia").unwrap();
        assert_eq!(person_detail(&conn, "Alicia").unwrap().faces.len(), 2);
        assert_eq!(person_detail(&conn, "Alice").unwrap().faces.len(), 0);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p videre-api faces`
Expected: FAIL to compile - `set_primary`/`rename_person` not defined.

- [ ] **Step 3: Implement the guarded mutations**

Add to `crates/videre-api/src/faces.rs`:

```rust
/// Mark one face as the person's primary (their labeling-page thumbnail),
/// clearing any previous primary in the same transaction so exactly one
/// remains. The target update is guarded by person_label so it can't steal a
/// face from another person.
pub fn set_primary(conn: &Connection, face_id: i64, person_label: &str) -> Result<()> {
    conn.execute_batch("BEGIN")?;
    let result = (|| -> rusqlite::Result<()> {
        conn.execute(
            "UPDATE faces SET is_primary = 0 WHERE person_label = ?1",
            rusqlite::params![person_label],
        )?;
        conn.execute(
            "UPDATE faces SET is_primary = 1, confirmed = 1, person_label = ?1 WHERE id = ?2 AND person_label = ?1",
            rusqlite::params![person_label, face_id],
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(Error::Db(e))
        }
    }
}

/// Rename a person. `NotFound` if `old_label` has no faces; `Conflict` if
/// `new_label` (after sanitizing) already belongs to a different person;
/// `Invalid` if the new label sanitizes to empty.
pub fn rename_person(conn: &Connection, old_label: &str, new_label: &str) -> Result<()> {
    let sanitized = crate::label::sanitize_person_label(new_label).ok_or(Error::Invalid)?;

    let old_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![old_label],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if old_count == 0 {
        return Err(Error::NotFound);
    }

    let collision_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM faces WHERE person_label = ?1",
            rusqlite::params![sanitized],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if collision_count > 0 && sanitized != old_label {
        return Err(Error::Conflict);
    }

    conn.execute(
        "UPDATE faces SET person_label = ?1 WHERE person_label = ?2",
        rusqlite::params![sanitized, old_label],
    )?;
    Ok(())
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p videre-api faces`
Expected: PASS.

- [ ] **Step 5: Run clippy on the crate**

Run: `cargo clippy -p videre-api --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/videre-api/src/faces.rs
git commit -m "feat(videre-api): guarded mutations (set_primary, rename_person)"
```

---

## Task 7: Reconcile the axum handlers onto videre-api

Rewrite each faces handler in `report.rs` to delegate to `videre-api`, remove the now-duplicated type/function definitions, and import from `videre-api`. Behavior (status codes) is unchanged.

**Files:**
- Modify: `crates/videre/Cargo.toml`
- Modify: `crates/videre/src/commands/report.rs`

- [ ] **Step 1: Add the dependency**

In `crates/videre/Cargo.toml`, under `[dependencies]`, add:

```toml
videre-api = { path = "../videre-api" }
```

- [ ] **Step 2: Add an error mapper**

Near the top of the faces-handler section in `report.rs`, add a helper that maps facade errors to the exact status codes the current handlers return:

```rust
fn api_status(e: videre_api::Error) -> StatusCode {
    match e {
        videre_api::Error::NotFound => StatusCode::NOT_FOUND,
        videre_api::Error::Conflict => StatusCode::CONFLICT,
        videre_api::Error::Invalid => StatusCode::BAD_REQUEST,
        videre_api::Error::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
```

- [ ] **Step 3: Delete the moved definitions from report.rs**

Remove these items from `report.rs` (now provided by `videre-api`): the structs `PersonData`, `ClusterData`, `SingletonData`, `FacesResponse`, `ClusterFaceData`, `ClusterDetailResponse`, `PersonFaceData`, `PersonDetailResponse`; the functions `query_faces_data`, `sanitize_person_label`, `is_disallowed_format_char`; and the `#[cfg(test)]` tests that covered `sanitize_person_label` (they now live in `videre-api`). Add imports at the top of the file:

```rust
use videre_api::{
    ClusterDetail, FacesData, PersonDetail,
};
```

Note: response bodies now use the `videre-api` type names. Where the code referenced `FacesResponse`, use `FacesData`; `ClusterDetailResponse` -> `ClusterDetail`; `PersonDetailResponse` -> `PersonDetail`. The request structs (`AssignRequest`, `NewPersonRequest`, `RemoveFaceRequest`, `DeletePersonRequest`, `RenamePersonRequest`, `DissolveClusterRequest`, `SetPrimaryRequest`, `PersonSearchQuery`) stay in `report.rs` - they are axum request-body types, not part of the facade.

- [ ] **Step 4: Rewrite the handlers to delegate**

Replace the eleven handler bodies as follows (signatures unchanged):

```rust
async fn handle_get_faces(
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<FacesData>, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::faces_list(&conn).map(AxumJson).map_err(api_status)
}

async fn handle_cluster_api(
    axum::extract::Path(cluster_id): axum::extract::Path<i64>,
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<ClusterDetail>, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::cluster_detail(&conn, cluster_id).map(AxumJson).map_err(api_status)
}

async fn handle_person_api(
    axum::extract::Path(name): axum::extract::Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<AxumJson<PersonDetail>, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::person_detail(&conn, &name).map(AxumJson).map_err(api_status)
}

async fn handle_search_person(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PersonSearchQuery>,
) -> Result<AxumJson<Vec<String>>, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::search_person(&conn, &q.name).map(AxumJson).map_err(api_status)
}

async fn handle_assign(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<AssignRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::assign(&conn, &req.face_ids, &req.person_label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_new_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<NewPersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::new_person(&conn, &req.face_ids, &req.label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_remove_face(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<RemoveFaceRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::remove_face(&conn, req.face_id)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_delete_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<DeletePersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::delete_person(&conn, &req.label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_rename_person(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<RenamePersonRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::rename_person(&conn, &req.old_label, &req.new_label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_dissolve_cluster(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<DissolveClusterRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::dissolve_cluster(&conn, req.cluster_id)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}

async fn handle_set_primary(
    State(state): State<Arc<AppState>>,
    AxumJson(req): AxumJson<SetPrimaryRequest>,
) -> Result<StatusCode, StatusCode> {
    let conn = state.conn.lock().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    videre_api::set_primary(&conn, req.face_id, &req.person_label)
        .map(|_| StatusCode::OK)
        .map_err(api_status)
}
```

Note: `handle_report` (line ~2218) still references `labeled_faces_by_hash` and other report helpers - leave it and all non-faces handlers untouched.

- [ ] **Step 5: Build the whole workspace**

Run: `cargo build --workspace`
Expected: compiles. If the compiler flags a leftover reference to a removed type (e.g. `FacesResponse`), rename it to the `videre-api` equivalent per Step 3.

- [ ] **Step 6: Run the faces server integration tests + report bin tests**

Run: `cargo test -p videre --test faces_server`
Expected: PASS (5 tests) - the server behaves identically.

Run: `cargo test -p videre --bins`
Expected: PASS. Note: the `sanitize_person_label_*` and any `query_faces_data` tests were removed from `report.rs` (they now live in `videre-api`); the `remove_face_resets_is_primary` / set-primary invariant tests in `report.rs` may also be removed if fully covered by `videre-api` Task 5/6 tests, or kept as server-level coverage - keep them if they still compile against the delegated handlers.

- [ ] **Step 7: Run clippy across the workspace**

Run: `cargo clippy --workspace --all-targets`
Expected: no new warnings in `videre-api` or the changed `report.rs` handlers. (Pre-existing warnings in `videre-ml` `search.rs`/`face_embed.rs` are unrelated.)

- [ ] **Step 8: Commit**

```bash
git add crates/videre/Cargo.toml crates/videre/src/commands/report.rs
git commit -m "refactor(videre): faces handlers delegate to videre-api facade"
```

---

## Self-Review (completed while writing)

- **Spec coverage:** all 11 JSON/data operations from the spec's facade table have a `videre-api` function + test + reconciled handler (Tasks 4-7); `sanitize_person_label` moved (Task 3); response types moved (Task 2); error type maps to the existing status codes (Task 1 + Task 7 Step 2). The two image-bytes ops are explicitly deferred to Plan 2 (stated in the scope note).
- **Placeholders:** none - every step has concrete code/commands. (Task 1 Step 3 intentionally shows the one-line correction for `impl std::error::Error for Error {}`.)
- **Type consistency:** facade returns `FacesData`/`ClusterDetail`/`PersonDetail`; Task 7 renames the report.rs usages from the old `FacesResponse`/`ClusterDetailResponse`/`PersonDetailResponse` names accordingly; function names match between `lib.rs` re-exports, tests, and handlers.
