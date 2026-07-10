# dupe-faces Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add face detection, embedding, DBSCAN clustering, an interactive localhost labeling server, and `dupe-search --person` to the dupe toolkit.

**Architecture:** `dupe-core` adds faces table schema, DBSCAN, and person search SQL. `dupe-ml` adds SCRFD detection, landmark alignment, ArcFace embedding (both via `ort`), and a `dupe-faces` pipeline binary. `dupe` adds an axum HTTP labeling server behind `dupe-report --faces`. Only `confirmed=1` face rows (user-approved via the server) are used by `dupe-search --person`.

**Tech Stack:** `ort 2` (ONNX Runtime), `axum 0.8` + `tokio 1` (HTTP server), `hf-hub` (model download, already in dupe-ml), `half 2` (f16 storage, already in dupe-core), `image 0.25` (decoding, already in dupe-ml), `rusqlite 0.32` (already everywhere)

---

## File map

**dupe-core (create):**
- `crates/dupe-core/src/face_db.rs` - faces table DDL + insert/load/update helpers
- `crates/dupe-core/src/face_cluster.rs` - DBSCAN over L2-normalized embeddings
- `crates/dupe-core/src/person_search.rs` - confirmed-face path queries

**dupe-ml (create):**
- `crates/dupe-ml/src/face_models.rs` - download buffalo_l models via hf-hub
- `crates/dupe-ml/src/face_detect.rs` - SCRFD ONNX wrapper + NMS
- `crates/dupe-ml/src/face_align.rs` - Umeyama similarity transform + bilinear warp to 112x112
- `crates/dupe-ml/src/face_embed.rs` - ArcFace ONNX wrapper + L2 normalization
- `crates/dupe-ml/src/bin/dupe-faces.rs` - pipeline binary

**dupe-core (modify):**
- `crates/dupe-core/src/lib.rs` - export new modules

**dupe-ml (modify):**
- `crates/dupe-ml/src/lib.rs` - export new modules
- `crates/dupe-ml/Cargo.toml` - add ort
- `crates/dupe-ml/src/bin/dupe-search.rs` - add --person flag

**dupe (modify):**
- `crates/dupe/Cargo.toml` - add axum, tokio
- `crates/dupe/src/bin/dupe_report.rs` - add --faces flag, axum server, labeling HTML

---

## Task 1: Add ort, axum, and tokio dependencies

**Files:**
- Modify: `crates/dupe-ml/Cargo.toml`
- Modify: `crates/dupe/Cargo.toml`

- [ ] **Step 1: Add ort to dupe-ml**

Open `crates/dupe-ml/Cargo.toml` and add to `[dependencies]`:

```toml
ort = { version = "2", features = ["download-binaries"] }
```

- [ ] **Step 2: Add axum and tokio to dupe**

Open `crates/dupe/Cargo.toml` and add to `[dependencies]`:

```toml
axum = "0.8"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
```

- [ ] **Step 3: Verify the workspace builds**

```bash
cargo build -p dupe -p dupe-ml 2>&1 | tail -5
```

Expected: no errors (warnings about unused deps are fine at this stage).

- [ ] **Step 4: Commit**

```bash
git add crates/dupe-ml/Cargo.toml crates/dupe/Cargo.toml
git commit -m "chore: add ort, axum, tokio dependencies"
```

---

## Task 2: faces table schema and DB helpers (dupe-core)

**Files:**
- Create: `crates/dupe-core/src/face_db.rs`
- Modify: `crates/dupe-core/src/lib.rs`

- [ ] **Step 1: Write the tests first**

Create `crates/dupe-core/src/face_db.rs` with tests only:

```rust
use half::f16;
use rusqlite::Connection;

pub struct FaceRow {
    pub hash: String,
    pub bbox: String,
    pub landmark: Option<String>,
    pub embedding: Vec<u8>,      // 512 f16 values as little-endian bytes (1024 bytes)
    pub cluster_id: Option<i64>,
    pub person_label: Option<String>,
    pub confirmed: i64,
}

pub fn create_faces_table(conn: &Connection) -> rusqlite::Result<()> {
    todo!()
}

pub fn replace_faces_for_hash(conn: &Connection, hash: &str, faces: &[FaceRow]) -> rusqlite::Result<()> {
    todo!()
}

pub fn load_face_embeddings(conn: &Connection) -> rusqlite::Result<Vec<(i64, Vec<f32>)>> {
    todo!()
}

pub fn update_cluster_assignments(conn: &Connection, assignments: &[(i64, Option<i64>)]) -> rusqlite::Result<()> {
    todo!()
}

pub fn hashes_with_faces(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    todo!()
}

fn make_embedding(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|&v| f16::from_f32(v).to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        create_faces_table(&conn).unwrap();
        conn
    }

    #[test]
    fn create_table_idempotent() {
        let conn = open();
        create_faces_table(&conn).unwrap(); // second call must not error
    }

    #[test]
    fn insert_and_load_embedding() {
        let conn = open();
        let emb = make_embedding(&vec![0.5f32; 512]);
        replace_faces_for_hash(&conn, "habc", &[FaceRow {
            hash: "habc".into(), bbox: "0,0,50,50".into(), landmark: None,
            embedding: emb, cluster_id: None, person_label: None, confirmed: 0,
        }]).unwrap();
        let rows = load_face_embeddings(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        let (id, emb_f32) = &rows[0];
        assert!(*id > 0);
        assert_eq!(emb_f32.len(), 512);
        assert!((emb_f32[0] - 0.5).abs() < 0.01);
    }

    #[test]
    fn replace_removes_old_rows_for_same_hash() {
        let conn = open();
        let emb = make_embedding(&vec![0.0f32; 512]);
        replace_faces_for_hash(&conn, "h1", &[
            FaceRow { hash: "h1".into(), bbox: "0,0,10,10".into(), landmark: None, embedding: emb.clone(), cluster_id: None, person_label: None, confirmed: 0 },
            FaceRow { hash: "h1".into(), bbox: "20,0,10,10".into(), landmark: None, embedding: emb.clone(), cluster_id: None, person_label: None, confirmed: 0 },
        ]).unwrap();
        replace_faces_for_hash(&conn, "h1", &[
            FaceRow { hash: "h1".into(), bbox: "99,0,10,10".into(), landmark: None, embedding: emb, cluster_id: None, person_label: None, confirmed: 0 },
        ]).unwrap();
        let rows = load_face_embeddings(&conn).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn update_cluster_assignments() {
        let conn = open();
        let emb = make_embedding(&vec![0.0f32; 512]);
        replace_faces_for_hash(&conn, "h1", &[FaceRow { hash: "h1".into(), bbox: "0,0,10,10".into(), landmark: None, embedding: emb, cluster_id: None, person_label: None, confirmed: 0 }]).unwrap();
        let rows = load_face_embeddings(&conn).unwrap();
        let id = rows[0].0;
        update_cluster_assignments(&conn, &[(id, Some(3))]).unwrap();
        let n: i64 = conn.query_row("SELECT cluster_id FROM faces WHERE id=?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }

    #[test]
    fn hashes_with_faces_returns_inserted_hash() {
        let conn = open();
        let emb = make_embedding(&vec![0.0f32; 512]);
        replace_faces_for_hash(&conn, "myhash", &[FaceRow { hash: "myhash".into(), bbox: "0,0,10,10".into(), landmark: None, embedding: emb, cluster_id: None, person_label: None, confirmed: 0 }]).unwrap();
        let hashes = hashes_with_faces(&conn).unwrap();
        assert_eq!(hashes, vec!["myhash"]);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p dupe-core face_db 2>&1 | grep -E "FAILED|error"
```

Expected: compile error or panics on `todo!()`.

- [ ] **Step 3: Implement the functions**

Replace the `todo!()` stubs in `face_db.rs`:

```rust
pub fn create_faces_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS faces (
            id            INTEGER PRIMARY KEY,
            hash          TEXT NOT NULL,
            bbox          TEXT NOT NULL,
            landmark      TEXT,
            embedding     BLOB NOT NULL,
            cluster_id    INTEGER,
            person_label  TEXT,
            confirmed     INTEGER DEFAULT 0
        );"
    )
}

pub fn replace_faces_for_hash(conn: &Connection, hash: &str, faces: &[FaceRow]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM faces WHERE hash = ?1", rusqlite::params![hash])?;
    for face in faces {
        conn.execute(
            "INSERT INTO faces (hash, bbox, landmark, embedding, cluster_id, person_label, confirmed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                face.hash, face.bbox, face.landmark, face.embedding,
                face.cluster_id, face.person_label, face.confirmed
            ],
        )?;
    }
    Ok(())
}

pub fn load_face_embeddings(conn: &Connection) -> rusqlite::Result<Vec<(i64, Vec<f32>)>> {
    let mut stmt = conn.prepare("SELECT id, embedding FROM faces")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        Ok((id, blob))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, blob) = row?;
        let emb: Vec<f32> = blob
            .chunks_exact(2)
            .map(|b| f16::from_le_bytes([b[0], b[1]]).to_f32())
            .collect();
        out.push((id, emb));
    }
    Ok(out)
}

pub fn update_cluster_assignments(conn: &Connection, assignments: &[(i64, Option<i64>)]) -> rusqlite::Result<()> {
    for (face_id, cluster_id) in assignments {
        conn.execute(
            "UPDATE faces SET cluster_id = ?1 WHERE id = ?2",
            rusqlite::params![cluster_id, face_id],
        )?;
    }
    Ok(())
}

pub fn hashes_with_faces(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT hash FROM faces ORDER BY hash")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}
```

- [ ] **Step 4: Export the module**

In `crates/dupe-core/src/lib.rs`:

```rust
pub mod embeddings;
pub mod face_cluster;
pub mod face_db;
pub mod person_search;
pub mod vectors;
```

(person_search will be created in Task 4; adding the export now is fine because it won't compile until that file exists - add the export in Task 4 instead if you prefer.)

- [ ] **Step 5: Run tests**

```bash
cargo test -p dupe-core face_db 2>&1 | tail -10
```

Expected: 5 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/dupe-core/src/face_db.rs crates/dupe-core/src/lib.rs
git commit -m "feat: faces table schema and DB helpers in dupe-core"
```

---

## Task 3: DBSCAN clustering (dupe-core)

**Files:**
- Create: `crates/dupe-core/src/face_cluster.rs`

- [ ] **Step 1: Write the tests**

Create `crates/dupe-core/src/face_cluster.rs`:

```rust
/// DBSCAN on L2-normalized embeddings using cosine distance.
/// Returns Vec<(face_id, cluster_id)> where cluster_id=None means outlier.
pub fn dbscan_cosine(
    points: &[(i64, Vec<f32>)],
    eps: f32,
    min_samples: usize,
) -> Vec<(i64, Option<i64>)> {
    todo!()
}

fn cosine_dist(a: &[f32], b: &[f32]) -> f32 {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l2(v: Vec<f32>) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    #[test]
    fn two_close_vectors_form_cluster() {
        let v1 = l2(vec![1.0f32, 0.01, 0.0]);
        let v2 = l2(vec![1.0f32, 0.02, 0.0]);
        let v3 = l2(vec![0.0f32, 1.0, 0.0]);
        let result = dbscan_cosine(&[(1, v1), (2, v2), (3, v3)], 0.1, 2);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_eq!(map[&1], map[&2], "close vectors must share cluster");
        assert_eq!(map[&3], None, "distant vector must be outlier");
    }

    #[test]
    fn identical_vectors_cluster_together() {
        let v = l2(vec![1.0f32, 0.0, 0.0]);
        let result = dbscan_cosine(&[(1, v.clone()), (2, v.clone()), (3, v)], 0.05, 2);
        let ids: Vec<_> = result.iter().map(|(_, c)| *c).collect();
        assert!(ids.iter().all(|c| c.is_some()), "all must be clustered");
        assert_eq!(ids[0], ids[1]);
        assert_eq!(ids[1], ids[2]);
    }

    #[test]
    fn all_noise_when_min_samples_too_high() {
        let v = l2(vec![1.0f32, 0.0]);
        let result = dbscan_cosine(&[(1, v.clone()), (2, v)], 0.05, 10);
        assert!(result.iter().all(|(_, c)| c.is_none()));
    }

    #[test]
    fn empty_input_returns_empty() {
        let result = dbscan_cosine(&[], 0.4, 2);
        assert!(result.is_empty());
    }

    #[test]
    fn two_distinct_clusters() {
        let a1 = l2(vec![1.0f32, 0.0, 0.0]);
        let a2 = l2(vec![0.99f32, 0.01, 0.0]);
        let b1 = l2(vec![0.0f32, 1.0, 0.0]);
        let b2 = l2(vec![0.0f32, 0.99, 0.01]);
        let result = dbscan_cosine(&[(1, a1), (2, a2), (3, b1), (4, b2)], 0.1, 2);
        let map: std::collections::HashMap<_, _> = result.into_iter().collect();
        assert_ne!(map[&1], map[&3]);
        assert_eq!(map[&1], map[&2]);
        assert_eq!(map[&3], map[&4]);
    }
}
```

- [ ] **Step 2: Confirm tests fail**

```bash
cargo test -p dupe-core face_cluster 2>&1 | grep -E "FAILED|error\[" | head -5
```

Expected: compile errors on `todo!()`.

- [ ] **Step 3: Implement DBSCAN**

Replace `todo!()` stubs:

```rust
pub fn dbscan_cosine(
    points: &[(i64, Vec<f32>)],
    eps: f32,
    min_samples: usize,
) -> Vec<(i64, Option<i64>)> {
    let n = points.len();
    if n == 0 { return Vec::new(); }

    // Precompute neighbor lists (indices within eps)
    let neighbors: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            (0..n)
                .filter(|&j| i != j && cosine_dist(&points[i].1, &points[j].1) <= eps)
                .collect()
        })
        .collect();

    let mut labels: Vec<Option<i64>> = vec![None; n];
    let mut visited = vec![false; n];
    let mut cluster_id: i64 = 0;

    for i in 0..n {
        if visited[i] { continue; }
        visited[i] = true;
        // Need self + neighbors >= min_samples to be a core point
        if neighbors[i].len() + 1 < min_samples { continue; }
        labels[i] = Some(cluster_id);
        let mut queue = neighbors[i].clone();
        let mut qi = 0;
        while qi < queue.len() {
            let q = queue[qi];
            qi += 1;
            if !visited[q] {
                visited[q] = true;
                if neighbors[q].len() + 1 >= min_samples {
                    for &nb in &neighbors[q] {
                        if !queue.contains(&nb) { queue.push(nb); }
                    }
                }
            }
            if labels[q].is_none() { labels[q] = Some(cluster_id); }
        }
        cluster_id += 1;
    }

    points.iter().zip(labels).map(|((id, _), lbl)| (*id, lbl)).collect()
}

fn cosine_dist(a: &[f32], b: &[f32]) -> f32 {
    1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
}
```

- [ ] **Step 4: Add to lib.rs export and run tests**

Add `pub mod face_cluster;` to `crates/dupe-core/src/lib.rs`.

```bash
cargo test -p dupe-core face_cluster 2>&1 | tail -8
```

Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-core/src/face_cluster.rs crates/dupe-core/src/lib.rs
git commit -m "feat: DBSCAN cosine clustering in dupe-core"
```

---

## Task 4: Person search query (dupe-core)

**Files:**
- Create: `crates/dupe-core/src/person_search.rs`

- [ ] **Step 1: Write the tests**

Create `crates/dupe-core/src/person_search.rs`:

```rust
use rusqlite::Connection;

/// File paths containing confirmed faces for the given person label.
pub fn search_by_person(conn: &Connection, name: &str, limit: Option<usize>) -> rusqlite::Result<Vec<String>> {
    todo!()
}

/// All distinct person labels with at least one confirmed face.
pub fn list_persons(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
             size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
             phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
             width INTEGER, height INTEGER);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0);
             INSERT INTO file_hashes VALUES ('/a.jpg','h1',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO file_hashes VALUES ('/b.jpg','h2',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO file_hashes VALUES ('/c.jpg','h3',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO faces VALUES (1,'h1','0,0,50,50',NULL,X'0000',0,'Alice',1);
             INSERT INTO faces VALUES (2,'h2','0,0,50,50',NULL,X'0000',0,'Alice',0);
             INSERT INTO faces VALUES (3,'h2','60,0,50,50',NULL,X'0000',1,'Bob',1);
             INSERT INTO faces VALUES (4,'h3','0,0,50,50',NULL,X'0000',NULL,NULL,0);"
        ).unwrap();
    }

    #[test]
    fn returns_only_confirmed_paths_for_person() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        let paths = search_by_person(&conn, "Alice", None).unwrap();
        // h1 has confirmed=1 for Alice; h2 has confirmed=0, so skipped
        assert_eq!(paths, vec!["/a.jpg"]);
    }

    #[test]
    fn limit_is_respected() {
        let conn = Connection::open_in_memory().unwrap();
        // Insert 3 confirmed Alice faces across different files
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
             size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
             phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL,
             bbox TEXT NOT NULL, landmark TEXT, embedding BLOB NOT NULL,
             cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0);
             INSERT INTO file_hashes VALUES ('/x.jpg','hx',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO file_hashes VALUES ('/y.jpg','hy',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO file_hashes VALUES ('/z.jpg','hz',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
             INSERT INTO faces VALUES (1,'hx','0,0,10,10',NULL,X'0000',0,'Alice',1);
             INSERT INTO faces VALUES (2,'hy','0,0,10,10',NULL,X'0000',0,'Alice',1);
             INSERT INTO faces VALUES (3,'hz','0,0,10,10',NULL,X'0000',0,'Alice',1);"
        ).unwrap();
        let paths = search_by_person(&conn, "Alice", Some(2)).unwrap();
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn unknown_person_returns_empty() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        assert!(search_by_person(&conn, "Nobody", None).unwrap().is_empty());
    }

    #[test]
    fn list_persons_returns_confirmed_labels() {
        let conn = Connection::open_in_memory().unwrap();
        setup(&conn);
        let names = list_persons(&conn).unwrap();
        assert_eq!(names, vec!["Alice", "Bob"]);
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p dupe-core person_search 2>&1 | grep -E "error\[|FAILED" | head -5
```

- [ ] **Step 3: Implement**

```rust
pub fn search_by_person(conn: &Connection, name: &str, limit: Option<usize>) -> rusqlite::Result<Vec<String>> {
    let limit_sql = limit.map(|n| format!(" LIMIT {n}")).unwrap_or_default();
    let sql = format!(
        "SELECT DISTINCT fh.path
         FROM faces f
         JOIN file_hashes fh ON fh.hash = f.hash
         WHERE f.person_label = ?1 AND f.confirmed = 1
         ORDER BY fh.path{limit_sql}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![name], |r| r.get(0))?;
    rows.collect()
}

pub fn list_persons(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT person_label FROM faces
         WHERE person_label IS NOT NULL AND confirmed = 1
         ORDER BY person_label"
    )?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    rows.collect()
}
```

- [ ] **Step 4: Export from lib.rs and run tests**

Add `pub mod person_search;` to `crates/dupe-core/src/lib.rs`.

```bash
cargo test -p dupe-core person_search 2>&1 | tail -8
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-core/src/person_search.rs crates/dupe-core/src/lib.rs
git commit -m "feat: person search query in dupe-core"
```

---

## Task 5: Model download helper (dupe-ml)

**Files:**
- Create: `crates/dupe-ml/src/face_models.rs`
- Modify: `crates/dupe-ml/src/lib.rs`

The buffalo_l models live at `deepinsight/insightface` on HuggingFace under `models/buffalo_l/`.

- [ ] **Step 1: Create face_models.rs**

```rust
use anyhow::Result;
use std::path::PathBuf;

/// Download (or return cached) SCRFD detector and ArcFace recognizer weights.
/// Uses hf-hub blocking API; downloads ~200 MB on first run into ~/.cache/huggingface/.
pub fn buffalo_l_paths() -> Result<(PathBuf, PathBuf)> {
    let api = hf_hub::api::sync::Api::new()?;
    let repo = api.model("deepinsight/insightface".to_string());
    let det = repo.get("models/buffalo_l/det_10g.onnx")?;
    let rec = repo.get("models/buffalo_l/w600k_r50.onnx")?;
    Ok((det, rec))
}
```

- [ ] **Step 2: Export from lib.rs**

Add to `crates/dupe-ml/src/lib.rs`:

```rust
pub mod device;
pub mod face_align;
pub mod face_detect;
pub mod face_embed;
pub mod face_models;
pub mod model;
pub mod preprocess;
pub mod search;
```

(face_align, face_detect, face_embed files will be created in Tasks 6-8; adding the exports now is fine if the files exist, or add them incrementally.)

- [ ] **Step 3: Confirm the crate compiles**

```bash
cargo build -p dupe-ml 2>&1 | grep "^error" | head -5
```

Expected: no errors (face_align etc. don't exist yet so comment out those exports until their tasks).

- [ ] **Step 4: Commit**

```bash
git add crates/dupe-ml/src/face_models.rs crates/dupe-ml/src/lib.rs
git commit -m "feat: buffalo_l model download helper"
```

---

## Task 6: SCRFD face detection (dupe-ml)

**Files:**
- Create: `crates/dupe-ml/src/face_detect.rs`

SCRFD-10GF expects 640x640 BGR float32 normalized by `(pixel - 127.5) / 128.0`. Outputs 9 tensors: score, bbox, kps for strides 8, 16, 32. Each anchor set has 2 anchors per grid cell.

- [ ] **Step 1: Write the struct and a shape test**

Create `crates/dupe-ml/src/face_detect.rs`:

```rust
use anyhow::{Context, Result};
use image::{DynamicImage, imageops::FilterType};
use ort::{Session, inputs};
use std::path::Path;

const INPUT_SIZE: u32 = 640;
const CONF_THRESHOLD: f32 = 0.5;
const NMS_THRESHOLD: f32 = 0.4;
const STRIDES: [u32; 3] = [8, 16, 32];
const ANCHORS_PER_CELL: usize = 2;

#[derive(Debug, Clone)]
pub struct Detection {
    pub bbox: [f32; 4],           // x1, y1, x2, y2 in original image coords
    pub score: f32,
    pub landmarks: [[f32; 2]; 5], // 5 points, original image coords
}

pub struct FaceDetector {
    session: Session,
    orig_w: u32,
    orig_h: u32,
}

impl FaceDetector {
    pub fn new(model_path: &Path) -> Result<Self> {
        let session = Session::builder()
            .context("ort session builder")?
            .commit_from_file(model_path)
            .context("load SCRFD model")?;
        Ok(Self { session, orig_w: 0, orig_h: 0 })
    }

    pub fn detect(&mut self, img: &DynamicImage) -> Result<Vec<Detection>> {
        self.orig_w = img.width();
        self.orig_h = img.height();
        let input_tensor = preprocess(img);
        let outputs = self.session.run(inputs!["input.1" => input_tensor.view()]?)?;
        postprocess(&outputs, self.orig_w, self.orig_h)
    }
}

/// Resize to 640x640, convert to BGR float32, normalize to [-1, 1].
fn preprocess(img: &DynamicImage) -> ndarray::Array4<f32> {
    let resized = img.resize_exact(INPUT_SIZE, INPUT_SIZE, FilterType::Bilinear);
    let rgb = resized.to_rgb8();
    let mut tensor = ndarray::Array4::<f32>::zeros([1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize]);
    for (x, y, pix) in rgb.enumerate_pixels() {
        // BGR channel order, normalized
        tensor[[0, 0, y as usize, x as usize]] = (pix[2] as f32 - 127.5) / 128.0;
        tensor[[0, 1, y as usize, x as usize]] = (pix[1] as f32 - 127.5) / 128.0;
        tensor[[0, 2, y as usize, x as usize]] = (pix[0] as f32 - 127.5) / 128.0;
    }
    tensor
}

/// Decode SCRFD output tensors into Detection structs with NMS applied.
/// Output tensor order (by index): score8, bbox8, kps8, score16, bbox16, kps16, score32, bbox32, kps32.
fn postprocess(outputs: &ort::SessionOutputs, orig_w: u32, orig_h: u32) -> Result<Vec<Detection>> {
    let scale_x = orig_w as f32 / INPUT_SIZE as f32;
    let scale_y = orig_h as f32 / INPUT_SIZE as f32;
    let mut detections: Vec<Detection> = Vec::new();

    for (stride_idx, &stride) in STRIDES.iter().enumerate() {
        let grid = (INPUT_SIZE / stride) as usize;
        let n = grid * grid * ANCHORS_PER_CELL;
        let base = stride_idx * 3;

        let scores = outputs[base].try_extract_tensor::<f32>()?;
        let bboxes = outputs[base + 1].try_extract_tensor::<f32>()?;
        let kps    = outputs[base + 2].try_extract_tensor::<f32>()?;

        let scores = scores.view();
        let bboxes = bboxes.view();
        let kps    = kps.view();

        for anchor_idx in 0..n {
            let score = scores[[0, anchor_idx, 0]];
            if score < CONF_THRESHOLD { continue; }

            // Grid cell + anchor offset
            let grid_y = (anchor_idx / ANCHORS_PER_CELL) / grid;
            let grid_x = (anchor_idx / ANCHORS_PER_CELL) % grid;
            let cx = (grid_x as f32 + 0.5) * stride as f32;
            let cy = (grid_y as f32 + 0.5) * stride as f32;

            // Bbox in 640x640 space
            let x1 = (cx - bboxes[[0, anchor_idx, 0]] * stride as f32) * scale_x;
            let y1 = (cy - bboxes[[0, anchor_idx, 1]] * stride as f32) * scale_y;
            let x2 = (cx + bboxes[[0, anchor_idx, 2]] * stride as f32) * scale_x;
            let y2 = (cy + bboxes[[0, anchor_idx, 3]] * stride as f32) * scale_y;

            // Landmarks in original image space
            let mut landmarks = [[0.0f32; 2]; 5];
            for p in 0..5 {
                landmarks[p][0] = (cx + kps[[0, anchor_idx, p * 2    ]] * stride as f32) * scale_x;
                landmarks[p][1] = (cy + kps[[0, anchor_idx, p * 2 + 1]] * stride as f32) * scale_y;
            }

            detections.push(Detection { bbox: [x1, y1, x2, y2], score, landmarks });
        }
    }

    Ok(nms(detections, NMS_THRESHOLD))
}

fn nms(mut dets: Vec<Detection>, iou_thresh: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
    let mut keep = Vec::new();
    let mut suppressed = vec![false; dets.len()];
    for i in 0..dets.len() {
        if suppressed[i] { continue; }
        keep.push(dets[i].clone());
        for j in (i + 1)..dets.len() {
            if iou(&dets[i].bbox, &dets[j].bbox) > iou_thresh {
                suppressed[j] = true;
            }
        }
    }
    keep
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = a[2].min(b[2]);
    let iy2 = a[3].min(b[3]);
    let inter = (ix2 - ix1).max(0.0) * (iy2 - iy1).max(0.0);
    let area_a = (a[2] - a[0]) * (a[3] - a[1]);
    let area_b = (b[2] - b[0]) * (b[3] - b[1]);
    inter / (area_a + area_b - inter)
}
```

Add `ndarray` to `crates/dupe-ml/Cargo.toml` (ort 2 uses it for tensor I/O):

```toml
ndarray = "0.16"
```

- [ ] **Step 2: Add unit tests for iou and nms**

Append to `face_detect.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_identical_boxes() {
        assert!((iou(&[0.0, 0.0, 10.0, 10.0], &[0.0, 0.0, 10.0, 10.0]) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn iou_non_overlapping() {
        assert_eq!(iou(&[0.0, 0.0, 5.0, 5.0], &[10.0, 10.0, 20.0, 20.0]), 0.0);
    }

    #[test]
    fn nms_removes_duplicate() {
        let d1 = Detection { bbox: [0.0, 0.0, 10.0, 10.0], score: 0.9, landmarks: [[0.0; 2]; 5] };
        let d2 = Detection { bbox: [1.0, 1.0, 11.0, 11.0], score: 0.8, landmarks: [[0.0; 2]; 5] };
        let result = nms(vec![d1, d2], 0.4);
        assert_eq!(result.len(), 1);
        assert!((result[0].score - 0.9).abs() < 1e-5);
    }

    #[test]
    fn nms_keeps_non_overlapping() {
        let d1 = Detection { bbox: [0.0, 0.0, 5.0, 5.0], score: 0.9, landmarks: [[0.0; 2]; 5] };
        let d2 = Detection { bbox: [100.0, 100.0, 110.0, 110.0], score: 0.8, landmarks: [[0.0; 2]; 5] };
        let result = nms(vec![d1, d2], 0.4);
        assert_eq!(result.len(), 2);
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p dupe-ml face_detect 2>&1 | tail -8
```

Expected: 4 tests pass (iou and nms; full pipeline test requires a model file).

- [ ] **Step 4: Export from lib.rs**

Add `pub mod face_detect;` to `crates/dupe-ml/src/lib.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-ml/src/face_detect.rs crates/dupe-ml/src/lib.rs crates/dupe-ml/Cargo.toml
git commit -m "feat: SCRFD face detection wrapper with NMS"
```

---

## Task 7: 5-point landmark alignment (dupe-ml)

**Files:**
- Create: `crates/dupe-ml/src/face_align.rs`

Aligns a detected face crop to the canonical 112x112 ArcFace template using the Umeyama similarity transform (scale + rotation + translation, no shear).

- [ ] **Step 1: Write tests**

Create `crates/dupe-ml/src/face_align.rs`:

```rust
use image::{DynamicImage, RgbImage, Rgb};

/// Canonical ArcFace 112x112 template landmarks (x, y).
const DST: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// Warp src image so detected landmarks map to the 112x112 ArcFace template.
pub fn align_face(img: &DynamicImage, landmarks: &[[f32; 2]; 5]) -> RgbImage {
    todo!()
}

/// Umeyama 2D similarity transform: returns 2x3 matrix M such that dst ≈ M * [src | 1].
pub fn umeyama(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    todo!()
}

fn warp_affine(img: &DynamicImage, m: [[f32; 3]; 2], out_w: u32, out_h: u32) -> RgbImage {
    todo!()
}

fn bilinear(img: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn umeyama_identity_when_src_equals_dst() {
        let m = umeyama(&DST, &DST);
        // M should be close to [[1,0,0],[0,1,0]]
        assert!((m[0][0] - 1.0).abs() < 1e-3, "m00={}", m[0][0]);
        assert!((m[1][1] - 1.0).abs() < 1e-3, "m11={}", m[1][1]);
        assert!(m[0][1].abs() < 1e-3);
        assert!(m[1][0].abs() < 1e-3);
        assert!(m[0][2].abs() < 1e-3);
        assert!(m[1][2].abs() < 1e-3);
    }

    #[test]
    fn umeyama_pure_translation() {
        let src: [[f32; 2]; 5] = [[0.0,0.0],[10.0,0.0],[5.0,5.0],[2.0,9.0],[8.0,9.0]];
        let mut dst = src;
        for p in dst.iter_mut() { p[0] += 20.0; p[1] += 30.0; }
        let m = umeyama(&src, &dst);
        // Scale ~1, tx ~20, ty ~30
        assert!((m[0][0] - 1.0).abs() < 0.01);
        assert!((m[0][2] - 20.0).abs() < 0.5);
        assert!((m[1][2] - 30.0).abs() < 0.5);
    }

    #[test]
    fn align_face_returns_112x112() {
        let img = DynamicImage::new_rgb8(200, 200);
        let lm: [[f32; 2]; 5] = [[40.0,60.0],[80.0,60.0],[60.0,80.0],[45.0,100.0],[75.0,100.0]];
        let out = align_face(&img, &lm);
        assert_eq!(out.width(), 112);
        assert_eq!(out.height(), 112);
    }
}
```

- [ ] **Step 2: Confirm tests fail**

```bash
cargo test -p dupe-ml face_align 2>&1 | grep "FAILED\|panicked" | head -5
```

- [ ] **Step 3: Implement**

```rust
pub fn align_face(img: &DynamicImage, landmarks: &[[f32; 2]; 5]) -> RgbImage {
    let m = umeyama(landmarks, &DST);
    warp_affine(img, m, 112, 112)
}

pub fn umeyama(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    let n = src.len() as f32;

    // Centroids
    let (mu_sx, mu_sy) = src.iter().fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p[0], ay + p[1]));
    let (mu_dx, mu_dy) = dst.iter().fold((0.0f32, 0.0f32), |(ax, ay), p| (ax + p[0], ay + p[1]));
    let (mu_sx, mu_sy) = (mu_sx / n, mu_sy / n);
    let (mu_dx, mu_dy) = (mu_dx / n, mu_dy / n);

    // Variance of src
    let var_s: f32 = src.iter().map(|p| (p[0] - mu_sx).powi(2) + (p[1] - mu_sy).powi(2)).sum::<f32>() / n;

    // Cross-covariance 2x2
    let mut cov = [[0.0f32; 2]; 2];
    for (s, d) in src.iter().zip(dst.iter()) {
        let ds = [s[0] - mu_sx, s[1] - mu_sy];
        let dd = [d[0] - mu_dx, d[1] - mu_dy];
        cov[0][0] += dd[0] * ds[0];
        cov[0][1] += dd[0] * ds[1];
        cov[1][0] += dd[1] * ds[0];
        cov[1][1] += dd[1] * ds[1];
    }
    cov[0][0] /= n; cov[0][1] /= n; cov[1][0] /= n; cov[1][1] /= n;

    // SVD of 2x2: [[a,b],[c,d]] -> use closed-form for 2x2
    // det(cov) determines sign flip
    let det = cov[0][0] * cov[1][1] - cov[0][1] * cov[1][0];
    let s_sign = if det >= 0.0 { 1.0f32 } else { -1.0f32 };

    // Frobenius norm of cov as proxy for singular values product
    let fro = (cov[0][0].powi(2) + cov[0][1].powi(2) + cov[1][0].powi(2) + cov[1][1].powi(2)).sqrt();

    // Scale
    let scale = if var_s > 1e-8 { fro * s_sign / var_s } else { 1.0 };

    // Rotation from cov (use atan2 of dominant direction)
    let angle = cov[1][0].atan2(cov[0][0]);
    let (sin_a, cos_a) = angle.sin_cos();

    // Translation: dst_centroid - scale * R * src_centroid
    let tx = mu_dx - scale * (cos_a * mu_sx - sin_a * mu_sy);
    let ty = mu_dy - scale * (sin_a * mu_sx + cos_a * mu_sy);

    [
        [scale * cos_a, -scale * sin_a, tx],
        [scale * sin_a,  scale * cos_a, ty],
    ]
}

fn warp_affine(img: &DynamicImage, m: [[f32; 3]; 2], out_w: u32, out_h: u32) -> RgbImage {
    let rgb = img.to_rgb8();
    let mut out = RgbImage::new(out_w, out_h);

    // Invert M (2x3 affine -> solve for src given dst)
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    let inv = if det.abs() > 1e-8 {
        let inv_det = 1.0 / det;
        [
            [ m[1][1] * inv_det, -m[0][1] * inv_det, (m[0][1]*m[1][2] - m[1][1]*m[0][2]) * inv_det],
            [-m[1][0] * inv_det,  m[0][0] * inv_det, (m[1][0]*m[0][2] - m[0][0]*m[1][2]) * inv_det],
        ]
    } else {
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
    };

    for dy in 0..out_h {
        for dx in 0..out_w {
            let sx = inv[0][0] * dx as f32 + inv[0][1] * dy as f32 + inv[0][2];
            let sy = inv[1][0] * dx as f32 + inv[1][1] * dy as f32 + inv[1][2];
            *out.get_pixel_mut(dx, dy) = bilinear(&rgb, sx, sy);
        }
    }
    out
}

fn bilinear(img: &RgbImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = img.dimensions();
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let get = |xi: i32, yi: i32| -> [f32; 3] {
        let xi = xi.clamp(0, w as i32 - 1) as u32;
        let yi = yi.clamp(0, h as i32 - 1) as u32;
        let p = img.get_pixel(xi, yi);
        [p[0] as f32, p[1] as f32, p[2] as f32]
    };

    let p00 = get(x0, y0);
    let p10 = get(x0 + 1, y0);
    let p01 = get(x0, y0 + 1);
    let p11 = get(x0 + 1, y0 + 1);

    let r = |i: usize| -> u8 {
        let v = p00[i]*(1.0-fx)*(1.0-fy) + p10[i]*fx*(1.0-fy)
              + p01[i]*(1.0-fx)*fy       + p11[i]*fx*fy;
        v.round().clamp(0.0, 255.0) as u8
    };
    Rgb([r(0), r(1), r(2)])
}
```

- [ ] **Step 4: Run tests and export**

Add `pub mod face_align;` to `crates/dupe-ml/src/lib.rs`.

```bash
cargo test -p dupe-ml face_align 2>&1 | tail -8
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-ml/src/face_align.rs crates/dupe-ml/src/lib.rs
git commit -m "feat: 5-point landmark alignment and bilinear warp"
```

---

## Task 8: ArcFace embedding (dupe-ml)

**Files:**
- Create: `crates/dupe-ml/src/face_embed.rs`

ArcFace w600k_r50 takes `[N, 3, 112, 112]` float32 normalized with mean 0.5 / std 0.5 (i.e. `(pixel/255 - 0.5) / 0.5`). Outputs `[N, 512]` embeddings. L2-normalize before storage.

- [ ] **Step 1: Write tests**

Create `crates/dupe-ml/src/face_embed.rs`:

```rust
use anyhow::{Context, Result};
use image::RgbImage;
use ndarray::Array4;
use ort::{Session, inputs};
use std::path::Path;

pub struct FaceEmbedder {
    session: Session,
}

impl FaceEmbedder {
    pub fn new(model_path: &Path) -> Result<Self> {
        let session = Session::builder()
            .context("ort session builder")?
            .commit_from_file(model_path)
            .context("load ArcFace model")?;
        Ok(Self { session })
    }

    /// Embed a batch of 112x112 aligned face crops. Returns L2-normalized 512-dim f32 vecs.
    pub fn embed_batch(&self, faces: &[RgbImage]) -> Result<Vec<Vec<f32>>> {
        todo!()
    }
}

/// Normalize 512-dim vector to unit length in-place.
pub fn l2_normalize(v: &mut Vec<f32>) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-8 { v.iter_mut().for_each(|x| *x /= norm); }
}

/// Pack RgbImage batch into NCHW float tensor, normalize to [-1, 1].
pub fn preprocess_batch(faces: &[RgbImage]) -> Array4<f32> {
    let n = faces.len();
    let mut tensor = Array4::<f32>::zeros([n, 3, 112, 112]);
    for (i, img) in faces.iter().enumerate() {
        for (x, y, pix) in img.enumerate_pixels() {
            tensor[[i, 0, y as usize, x as usize]] = (pix[0] as f32 / 255.0 - 0.5) / 0.5;
            tensor[[i, 1, y as usize, x as usize]] = (pix[1] as f32 / 255.0 - 0.5) / 0.5;
            tensor[[i, 2, y as usize, x as usize]] = (pix[2] as f32 / 255.0 - 0.5) / 0.5;
        }
    }
    tensor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_vector_unchanged() {
        let mut v = vec![1.0f32, 0.0, 0.0];
        l2_normalize(&mut v);
        assert!((v[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_produces_unit_length() {
        let mut v = vec![3.0f32, 4.0, 0.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_safe() {
        let mut v = vec![0.0f32; 512];
        l2_normalize(&mut v); // must not panic or divide by zero
    }

    #[test]
    fn preprocess_batch_shape() {
        let imgs = vec![RgbImage::new(112, 112), RgbImage::new(112, 112)];
        let t = preprocess_batch(&imgs);
        assert_eq!(t.shape(), &[2, 3, 112, 112]);
    }

    #[test]
    fn preprocess_batch_range() {
        let mut img = RgbImage::new(112, 112);
        // Fill with all-255 pixel
        for p in img.pixels_mut() { *p = image::Rgb([255, 255, 255]); }
        let t = preprocess_batch(&[img]);
        // (255/255 - 0.5) / 0.5 = 1.0
        assert!((t[[0, 0, 0, 0]] - 1.0).abs() < 1e-5);
    }
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p dupe-ml face_embed 2>&1 | tail -8
```

Expected: 5 tests pass (no model needed for these).

- [ ] **Step 3: Implement embed_batch**

Replace the `todo!()`:

```rust
pub fn embed_batch(&self, faces: &[RgbImage]) -> Result<Vec<Vec<f32>>> {
    if faces.is_empty() { return Ok(Vec::new()); }
    let tensor = preprocess_batch(faces);
    let outputs = self.session.run(inputs!["input.1" => tensor.view()]?)?;
    let raw = outputs[0].try_extract_tensor::<f32>()?;
    let view = raw.view();
    let n = faces.len();
    let mut result = Vec::with_capacity(n);
    for i in 0..n {
        let mut emb: Vec<f32> = (0..512).map(|j| view[[i, j]]).collect();
        l2_normalize(&mut emb);
        result.push(emb);
    }
    Ok(result)
}
```

- [ ] **Step 4: Export from lib.rs**

Add `pub mod face_embed;` to `crates/dupe-ml/src/lib.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/dupe-ml/src/face_embed.rs crates/dupe-ml/src/lib.rs
git commit -m "feat: ArcFace embedding wrapper with L2 normalization"
```

---

## Task 9: dupe-faces pipeline binary (dupe-ml)

**Files:**
- Create: `crates/dupe-ml/src/bin/dupe-faces.rs`
- Modify: `crates/dupe-ml/Cargo.toml`

- [ ] **Step 1: Register the binary in Cargo.toml**

Add to `crates/dupe-ml/Cargo.toml`:

```toml
[[bin]]
name = "dupe-faces"
path = "src/bin/dupe-faces.rs"
```

- [ ] **Step 2: Write an integration test**

Create `crates/dupe-ml/tests/faces_pipeline.rs`:

```rust
use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); p.pop();
    p.push("dupe-faces");
    p
}

fn make_db(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL,
         width INTEGER, height INTEGER);"
    ).unwrap();
    db
}

#[test]
fn exits_zero_on_empty_db() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    let status = Command::new(bin())
        .arg(&db).arg("--silent")
        .status().expect("failed to run dupe-faces");
    assert!(status.success());
}

#[test]
fn creates_faces_table() {
    let dir = tempdir().unwrap();
    let db = make_db(dir.path());
    Command::new(bin()).arg(&db).arg("--silent").status().unwrap();
    let conn = Connection::open(&db).unwrap();
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='faces'", [], |r| r.get(0)
    ).unwrap();
    assert_eq!(n, 1);
}
```

- [ ] **Step 3: Run test to confirm it fails**

```bash
cargo test -p dupe-ml --test faces_pipeline exits_zero 2>&1 | grep -E "error|FAILED" | head -5
```

Expected: compile error (file doesn't exist yet).

- [ ] **Step 4: Write the binary**

Create `crates/dupe-ml/src/bin/dupe-faces.rs`:

```rust
use anyhow::Result;
use clap::Parser;
use dupe_core::{face_cluster, face_db};
use dupe_ml::{face_align, face_detect, face_embed, face_models};
use half::f16;
use image::DynamicImage;
use rusqlite::Connection;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dupe-faces", about = "Detect, embed, and cluster faces in a dupe SQLite database.")]
struct Args {
    db: PathBuf,
    #[arg(long)] reprocess: bool,
    #[arg(long, default_value = "8")] batch: usize,
    #[arg(long)] dry_run: bool,
    #[arg(long)] silent: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if !args.db.exists() {
        anyhow::bail!("{:?} does not exist", args.db);
    }
    let conn = Connection::open(&args.db)?;
    face_db::create_faces_table(&conn)?;

    // 1. Determine which hashes to process
    let all_paths: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT path, hash FROM file_hashes WHERE ext IN ('jpg','jpeg','png','gif','webp','bmp','tiff','heic')"
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect()
    };

    let skip_hashes: std::collections::HashSet<String> = if args.reprocess {
        std::collections::HashSet::new()
    } else {
        face_db::hashes_with_faces(&conn)?.into_iter().collect()
    };

    let to_process: Vec<(String, String)> = all_paths.into_iter()
        .filter(|(_, hash)| !skip_hashes.contains(hash))
        .collect();

    if to_process.is_empty() {
        if !args.silent { eprintln!("All hashes already processed."); }
        return Ok(());
    }

    if !args.silent { eprintln!("Processing {} images...", to_process.len()); }

    // 2. Download models
    let (det_path, rec_path) = face_models::buffalo_l_paths()?;
    let mut detector = face_detect::FaceDetector::new(&det_path)?;
    let embedder = face_embed::FaceEmbedder::new(&rec_path)?;

    let mut total_faces = 0usize;

    // 3. Detect + align + embed per hash (one representative path per hash)
    for chunk in to_process.chunks(args.batch) {
        for (path, hash) in chunk {
            let img = match load_image(path) {
                Some(i) => i,
                None => continue,
            };
            let detections = match detector.detect(&img) {
                Ok(d) => d,
                Err(e) => { eprintln!("detect failed {path}: {e}"); continue; }
            };
            if detections.is_empty() { continue; }

            let crops: Vec<image::RgbImage> = detections.iter()
                .map(|d| face_align::align_face(&img, &d.landmarks))
                .collect();

            let embeddings = match embedder.embed_batch(&crops) {
                Ok(e) => e,
                Err(e) => { eprintln!("embed failed {path}: {e}"); continue; }
            };

            let rows: Vec<face_db::FaceRow> = detections.iter().zip(embeddings.iter()).map(|(det, emb)| {
                let [x1, y1, x2, y2] = det.bbox;
                let bbox = format!("{},{},{},{}", x1 as i32, y1 as i32, (x2 - x1) as i32, (y2 - y1) as i32);
                let lm_str: String = det.landmarks.iter()
                    .flat_map(|[x, y]| [x.to_string(), y.to_string()])
                    .collect::<Vec<_>>().join(",");
                let embedding: Vec<u8> = emb.iter()
                    .flat_map(|&v| f16::from_f32(v).to_le_bytes())
                    .collect();
                face_db::FaceRow { hash: hash.clone(), bbox, landmark: Some(lm_str), embedding, cluster_id: None, person_label: None, confirmed: 0 }
            }).collect();

            if !args.silent { println!("[faces] {path}: {} face(s)", rows.len()); }
            total_faces += rows.len();
            if !args.dry_run {
                face_db::replace_faces_for_hash(&conn, hash, &rows)?;
            }
        }
    }

    // 4. Re-cluster all embeddings in DB
    if !args.dry_run && total_faces > 0 {
        let all_embs = face_db::load_face_embeddings(&conn)?;
        let assignments = face_cluster::dbscan_cosine(&all_embs, 0.4, 2);
        face_db::update_cluster_assignments(&conn, &assignments)?;
        if !args.silent { eprintln!("Clustering complete: {} faces in DB.", all_embs.len()); }
    }

    if !args.silent { eprintln!("Done: {} new face(s) detected.", total_faces); }
    Ok(())
}

fn load_image(path: &str) -> Option<DynamicImage> {
    if path.to_lowercase().ends_with(".heic") {
        // HEIC: convert to JPEG via sips (macOS), then decode
        #[cfg(target_os = "macos")]
        {
            let tmp = std::env::temp_dir().join("dupe_faces_heic.jpg");
            let ok = std::process::Command::new("sips")
                .args(["-s", "format", "jpeg", path, "--out", tmp.to_str()?])
                .status().ok()?.success();
            if ok { return image::open(&tmp).ok(); }
        }
        return None;
    }
    image::open(path).ok()
}
```

- [ ] **Step 5: Build and run integration tests**

```bash
cargo build -p dupe-ml --bin dupe-faces 2>&1 | tail -5
cargo test -p dupe-ml --test faces_pipeline 2>&1 | tail -8
```

Expected: 2 integration tests pass (no model downloaded; empty DB).

- [ ] **Step 6: Commit**

```bash
git add crates/dupe-ml/src/bin/dupe-faces.rs crates/dupe-ml/tests/faces_pipeline.rs crates/dupe-ml/Cargo.toml crates/dupe-ml/src/lib.rs
git commit -m "feat: dupe-faces pipeline binary"
```

---

## Task 10: axum labeling server for dupe-report --faces

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`

This task adds the `--faces` flag. When set, instead of generating an HTML file it starts an axum server on port 7878. The actual HTML page content is added in Task 11.

- [ ] **Step 1: Add --faces to Args**

Open `crates/dupe/src/bin/dupe_report.rs`. Find the `Args` struct and add:

```rust
/// Start an interactive face labeling server at localhost:7878
#[arg(long)]
faces: bool,
```

- [ ] **Step 2: Add the tokio runtime and server dispatch in main**

At the end of `fn main()`, after the existing output file logic, add:

```rust
if args.faces {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.block_on(serve_faces(args.db.clone())).expect("faces server error");
    return;
}
```

- [ ] **Step 3: Add the server module**

Add at the bottom of `dupe_report.rs` (before the closing brace / after existing functions):

```rust
use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

#[derive(Clone)]
struct FacesState {
    db_path: std::path::PathBuf,
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

async fn serve_faces(db_path: std::path::PathBuf) -> anyhow::Result<()> {
    let (tx, rx) = oneshot::channel::<()>();
    let state = FacesState {
        db_path,
        shutdown: Arc::new(Mutex::new(Some(tx))),
    };
    let app = Router::new()
        .route("/", get(handle_root))
        .route("/api/faces", get(api_get_faces))
        .route("/api/assign", post(api_assign))
        .route("/api/new-person", post(api_new_person))
        .route("/api/remove-face", post(api_remove_face))
        .route("/api/set-primary", post(api_set_primary))
        .route("/api/search/person", get(api_search_person))
        .route("/api/quit", post(api_quit))
        .with_state(state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 7878));
    eprintln!("Faces labeling server: http://localhost:7878");
    // Auto-open browser
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("http://localhost:7878").spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg("http://localhost:7878").spawn();

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async { rx.await.ok(); })
        .await?;
    Ok(())
}

async fn handle_root(State(state): State<FacesState>) -> impl IntoResponse {
    Html(FACES_HTML)  // defined in Task 11
}

#[derive(Serialize)]
struct FacesData {
    people: Vec<PersonJson>,
    clusters: Vec<ClusterJson>,
    singletons: Vec<FaceJson>,
}

#[derive(Serialize)]
struct PersonJson {
    label: String,
    primary_face_id: Option<i64>,
    faces: Vec<FaceJson>,
}

#[derive(Serialize)]
struct ClusterJson {
    cluster_id: i64,
    faces: Vec<FaceJson>,
}

#[derive(Serialize)]
struct FaceJson {
    id: i64,
    hash: String,
    bbox: String,
    person_label: Option<String>,
    confirmed: i64,
}

async fn api_get_faces(State(state): State<FacesState>) -> impl IntoResponse {
    let conn = match rusqlite::Connection::open(&state.db_path) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match query_faces_data(&conn) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn query_faces_data(conn: &rusqlite::Connection) -> rusqlite::Result<FacesData> {
    let mut stmt = conn.prepare(
        "SELECT id, hash, bbox, person_label, confirmed, cluster_id FROM faces ORDER BY id"
    )?;
    let rows: Vec<_> = stmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?, r.get::<_, i64>(4)?, r.get::<_, Option<i64>>(5)?))
    })?.filter_map(|r| r.ok()).collect();

    let mut people: std::collections::HashMap<String, Vec<FaceJson>> = std::collections::HashMap::new();
    let mut clusters: std::collections::HashMap<i64, Vec<FaceJson>> = std::collections::HashMap::new();
    let mut singletons: Vec<FaceJson> = Vec::new();

    for (id, hash, bbox, person_label, confirmed, cluster_id) in rows {
        let face = FaceJson { id, hash, bbox, person_label: person_label.clone(), confirmed };
        if let Some(label) = person_label {
            if confirmed == 1 {
                people.entry(label).or_default().push(face);
                continue;
            }
        }
        match cluster_id {
            Some(cid) => clusters.entry(cid).or_default().push(face),
            None => singletons.push(face),
        }
    }

    let people_vec: Vec<PersonJson> = people.into_iter().map(|(label, faces)| {
        let primary_face_id = faces.first().map(|f| f.id);
        PersonJson { label, primary_face_id, faces }
    }).collect();

    let clusters_vec: Vec<ClusterJson> = clusters.into_iter()
        .map(|(cluster_id, faces)| ClusterJson { cluster_id, faces })
        .collect();

    Ok(FacesData { people: people_vec, clusters: clusters_vec, singletons })
}

#[derive(Deserialize)]
struct AssignBody { face_ids: Vec<i64>, person_label: String }

async fn api_assign(State(state): State<FacesState>, Json(body): Json<AssignBody>) -> impl IntoResponse {
    let conn = rusqlite::Connection::open(&state.db_path).unwrap();
    for id in &body.face_ids {
        conn.execute(
            "UPDATE faces SET person_label=?1, confirmed=1 WHERE id=?2",
            rusqlite::params![body.person_label, id],
        ).unwrap();
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct NewPersonBody { face_ids: Vec<i64>, label: String }

async fn api_new_person(State(state): State<FacesState>, Json(body): Json<NewPersonBody>) -> impl IntoResponse {
    let conn = rusqlite::Connection::open(&state.db_path).unwrap();
    for id in &body.face_ids {
        conn.execute(
            "UPDATE faces SET person_label=?1, confirmed=1 WHERE id=?2",
            rusqlite::params![body.label, id],
        ).unwrap();
    }
    StatusCode::OK
}

#[derive(Deserialize)]
struct RemoveFaceBody { face_id: i64 }

async fn api_remove_face(State(state): State<FacesState>, Json(body): Json<RemoveFaceBody>) -> impl IntoResponse {
    let conn = rusqlite::Connection::open(&state.db_path).unwrap();
    conn.execute("UPDATE faces SET cluster_id=NULL WHERE id=?1", [body.face_id]).unwrap();
    StatusCode::OK
}

#[derive(Deserialize)]
struct SetPrimaryBody { person_label: String, face_id: i64 }

async fn api_set_primary(State(state): State<FacesState>, Json(body): Json<SetPrimaryBody>) -> impl IntoResponse {
    // For now: mark the chosen face as confirmed=1; future: add primary_face_id column
    let conn = rusqlite::Connection::open(&state.db_path).unwrap();
    conn.execute(
        "UPDATE faces SET confirmed=1 WHERE id=?1 AND person_label=?2",
        rusqlite::params![body.face_id, body.person_label],
    ).unwrap();
    StatusCode::OK
}

#[derive(Deserialize)]
struct SearchQuery { name: String }

async fn api_search_person(State(state): State<FacesState>, Query(q): Query<SearchQuery>) -> impl IntoResponse {
    let conn = rusqlite::Connection::open(&state.db_path).unwrap();
    match dupe_core::person_search::search_by_person(&conn, &q.name, None) {
        Ok(paths) => Json(paths).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn api_quit(State(state): State<FacesState>) -> impl IntoResponse {
    if let Some(tx) = state.shutdown.lock().unwrap().take() {
        let _ = tx.send(());
    }
    StatusCode::OK
}

const FACES_HTML: &str = "";  // replaced in Task 11
```

- [ ] **Step 4: Build**

```bash
cargo build -p dupe --bin dupe-report 2>&1 | tail -8
```

Expected: compiles. The server will serve an empty page until Task 11.

- [ ] **Step 5: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/Cargo.toml
git commit -m "feat: axum labeling server behind dupe-report --faces"
```

---

## Task 11: Faces labeling HTML page

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs` (replace FACES_HTML)

This replaces the `const FACES_HTML: &str = "";` placeholder with the full self-contained labeling page. The page fetches `/api/faces` on load and re-fetches after every action.

- [ ] **Step 1: Replace FACES_HTML**

In `dupe_report.rs`, replace `const FACES_HTML: &str = "";` with:

```rust
const FACES_HTML: &str = r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="UTF-8">
<title>Faces - dupe</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
:root{--bg:#f3f4f6;--surface:#fff;--border:#e5e7eb;--text:#111827;--muted:#6b7280;
--accent:#2563eb;--green:#16a34a;--green-bg:#dcfce7;--green-border:#86efac;
--toolbar:#1e293b;--face-bg:#dbeafe;--remove:#ef4444}
@media(prefers-color-scheme:dark){:root{--bg:#111827;--surface:#1f2937;--border:#374151;
--text:#f9fafb;--muted:#9ca3af;--face-bg:#1e3a5f;--toolbar:#0f172a;
--green-bg:#14532d;--green-border:#166534;--green:#4ade80}}
body{background:var(--bg);color:var(--text);font:13px/1.5 system-ui,sans-serif}
.toolbar{background:var(--toolbar);color:#f1f5f9;display:flex;align-items:center;
gap:12px;padding:0 20px;height:44px}
.toolbar strong{font-size:14px}.stats{font-size:11px;color:#94a3b8}
.tbtn{background:#334155;color:#e2e8f0;border:none;border-radius:5px;padding:4px 10px;
font-size:11px;cursor:pointer;margin-left:auto}
.tbtn.primary{background:var(--accent);color:#fff;margin-left:6px}
main{max-width:1080px;margin:0 auto;padding:20px}
.sec-head{display:flex;align-items:center;gap:8px;margin:24px 0 12px;
border-bottom:1.5px solid var(--border);padding-bottom:8px}
.sec-head h2{font-size:13px;font-weight:700}
.pill{font-size:10px;font-weight:700;padding:2px 8px;border-radius:10px;
background:var(--border);color:var(--muted)}
.pill.green{background:var(--green-bg);color:var(--green)}
.sec-note{font-size:10px;color:var(--muted);margin-left:auto}
.people-grid{display:flex;flex-wrap:wrap;gap:12px}
.person-card{background:var(--surface);border:1.5px solid var(--green-border);
border-radius:10px;width:110px;cursor:pointer;transition:box-shadow .15s}
.person-card:hover,.person-card.drop-over{box-shadow:0 0 0 3px var(--accent);border-color:var(--accent)}
.person-avatar{width:100%;aspect-ratio:1;background:var(--face-bg);border-radius:8px 8px 0 0;
display:flex;align-items:center;justify-content:center;font-size:11px;font-weight:600;
color:var(--muted);overflow:hidden;position:relative}
.person-avatar img{width:100%;height:100%;object-fit:cover}
.person-foot{padding:6px 8px}
.person-name{font-size:12px;font-weight:700;color:var(--green)}
.person-count{font-size:10px;color:var(--muted)}
.drop-zone{width:110px;height:110px;border:2px dashed var(--border);border-radius:10px;
display:flex;align-items:center;justify-content:center;color:var(--muted);
font-size:10px;text-align:center;padding:8px}
.card-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(210px,1fr));gap:12px}
.face-card{background:var(--surface);border:1.5px solid var(--border);border-radius:10px;
overflow:hidden;cursor:grab}
.face-card:active{cursor:grabbing}
.card-header{display:flex;align-items:center;gap:6px;padding:8px 10px 4px}
.drag-handle{color:var(--muted);font-size:14px}
.card-label{font-size:10px;font-weight:700;color:var(--muted)}
.count-badge{margin-left:auto;background:var(--border);border-radius:10px;
padding:1px 7px;font-size:10px;font-weight:700;color:var(--muted)}
.face-grid{display:flex;flex-wrap:wrap;gap:3px;padding:4px 8px}
.face-thumb{width:52px;height:52px;border-radius:5px;background:var(--face-bg);
display:flex;align-items:center;justify-content:center;font-size:10px;color:var(--muted);
position:relative;overflow:hidden}
.face-thumb img{width:100%;height:100%;object-fit:cover}
.face-thumb .rm{position:absolute;top:-5px;right:-5px;width:16px;height:16px;
border-radius:50%;background:var(--remove);color:#fff;font-size:11px;font-weight:700;
display:none;align-items:center;justify-content:center;cursor:pointer;
border:1.5px solid var(--surface)}
.face-thumb:hover .rm{display:flex}
.face-more{width:52px;height:52px;border-radius:5px;background:var(--border);
display:flex;align-items:center;justify-content:center;font-size:11px;font-weight:700;color:var(--muted)}
.singleton-thumb{width:100%;aspect-ratio:1;background:var(--face-bg);display:flex;
align-items:center;justify-content:center;font-size:11px;color:var(--muted);
position:relative;overflow:hidden}
.singleton-thumb img{width:100%;height:100%;object-fit:cover}
.singleton-thumb .rm{top:6px;right:6px;width:20px;height:20px;font-size:13px}
.card-footer{padding:8px 10px 10px}
.np-btn{width:100%;background:var(--surface);border:1.5px dashed var(--border);
border-radius:6px;color:var(--muted);font-size:11px;font-weight:600;padding:5px;cursor:pointer}
.np-btn:hover{border-color:var(--accent);color:var(--accent)}
.np-form{display:none;gap:5px}
.np-form.show{display:flex}
.np-input{flex:1;border:1.5px solid var(--accent);border-radius:6px;padding:5px 8px;
font-size:11px;background:var(--bg);color:var(--text)}
.np-input:focus{outline:none}
.np-submit{background:var(--accent);color:#fff;border:none;border-radius:6px;
padding:5px 10px;font-size:11px;font-weight:700;cursor:pointer}
.drag-tip{font-size:10px;color:var(--muted);padding:0 10px 6px}
.modal-overlay{position:fixed;inset:0;background:rgba(0,0,0,.6);display:flex;
align-items:center;justify-content:center;z-index:100}
.modal{background:var(--surface);border-radius:12px;width:540px;max-height:80vh;
overflow:auto;padding:20px}
.modal-head{display:flex;align-items:center;gap:10px;margin-bottom:14px}
.modal-name{font-size:16px;font-weight:700}
.modal-sub{font-size:11px;color:var(--muted)}
.modal-close{margin-left:auto;font-size:18px;cursor:pointer;color:var(--muted)}
.modal-photos{display:flex;flex-wrap:wrap;gap:6px}
.modal-photo{width:80px;height:80px;border-radius:6px;background:var(--face-bg);
display:flex;align-items:center;justify-content:center;font-size:11px;color:var(--muted);
position:relative;overflow:hidden;cursor:pointer}
.modal-photo img{width:100%;height:100%;object-fit:cover}
.modal-photo .primary-badge{position:absolute;top:-5px;left:-5px;background:var(--green);
color:#fff;font-size:8px;font-weight:700;padding:1px 5px;border-radius:8px}
.modal-photo:hover::after{content:'Set primary';position:absolute;bottom:0;left:0;right:0;
background:rgba(0,0,0,.55);color:#fff;font-size:9px;text-align:center;padding:2px}
</style></head><body>
<div class="toolbar">
  <strong>Faces</strong>
  <span class="stats" id="stats">Loading...</span>
  <button class="tbtn" onclick="exportLabels()">Export labels</button>
  <button class="tbtn primary" onclick="saveAndClose()">Save &amp; close</button>
</div>
<main id="app"><p style="padding:20px;color:var(--muted)">Loading...</p></main>
<div class="modal-overlay" id="modal" style="display:none" onclick="if(event.target===this)closeModal()">
  <div class="modal">
    <div class="modal-head">
      <div><div class="modal-name" id="modal-name"></div><div class="modal-sub" id="modal-sub"></div></div>
      <span class="modal-close" onclick="closeModal()">&#x2715;</span>
    </div>
    <div class="modal-photos" id="modal-photos"></div>
  </div>
</div>
<script>
let DATA = null;
let dragData = null; // {face_ids: [], label: null}

async function load() {
  const r = await fetch('/api/faces');
  DATA = await r.json();
  render();
}

function render() {
  const app = document.getElementById('app');
  const totalPeople = DATA.people.length;
  const totalClusters = DATA.clusters.length;
  const totalSingletons = DATA.singletons.length;
  document.getElementById('stats').textContent =
    totalPeople + ' people · ' + totalClusters + ' clusters · ' + totalSingletons + ' singletons';
  app.innerHTML = renderSection1() + renderSection2() + renderSection3();
  initDragDrop();
}

function faceThumb(face, small) {
  const sz = small ? 52 : 80;
  // In production: replace with <img src="/api/thumb/{{face.id}}">
  return `<div class="face-thumb" data-id="${face.id}" style="width:${sz}px;height:${sz}px">
    <span style="font-size:9px;text-align:center;color:var(--muted)">#${face.id}</span>
    <div class="rm" onclick="removeFace(${face.id});event.stopPropagation()">&#x2212;</div>
  </div>`;
}

function renderSection1() {
  let cards = DATA.people.map(p => `
    <div class="person-card" draggable="false"
         data-person="${esc(p.label)}"
         ondragover="event.preventDefault();this.classList.add('drop-over')"
         ondragleave="this.classList.remove('drop-over')"
         ondrop="dropOnPerson('${esc(p.label)}',this)"
         onclick="openModal('${esc(p.label)}')">
      <div class="person-avatar">#${p.primary_face_id || '?'}</div>
      <div class="person-foot">
        <div class="person-name">${esc(p.label)}</div>
        <div class="person-count">${p.faces.length} photo(s)</div>
      </div>
    </div>`).join('');
  cards += `<div class="drop-zone"
    ondragover="event.preventDefault()"
    ondrop="dropToNew()">Drop here to create new person</div>`;
  return `<div class="sec-head"><h2>People</h2>
    <span class="pill green">${DATA.people.length} identified</span>
    <span class="sec-note">Click to view all · drag clusters here to assign</span></div>
    <div class="people-grid" id="people-grid">${cards}</div>`;
}

function clusterCard(cluster, isCluster) {
  const label = isCluster ? 'Cluster ' + cluster.cluster_id : 'Face #' + cluster.faces[0].id;
  const badge = cluster.faces.length > 1 ? `<span class="count-badge">+${cluster.faces.length} faces</span>` : '';
  const thumbs = isCluster
    ? cluster.faces.slice(0,4).map(f => faceThumb(f, true)).join('') +
      (cluster.faces.length > 4 ? `<div class="face-more">+${cluster.faces.length - 4}</div>` : '')
    : `<div class="singleton-thumb" data-id="${cluster.faces[0].id}">
         <span style="font-size:9px;color:var(--muted)">#${cluster.faces[0].id}</span>
         <div class="rm" onclick="removeFace(${cluster.faces[0].id})">&#x2212;</div>
       </div>`;
  const ids = cluster.faces.map(f => f.id).join(',');
  return `<div class="face-card" draggable="true"
    data-ids="${ids}"
    ondragstart="startDrag(this)">
    <div class="card-header">
      <span class="drag-handle">&#x283F;</span>
      <span class="card-label">${label}</span>${badge}
    </div>
    <div class="${isCluster ? 'face-grid' : ''}">${thumbs}</div>
    <div class="card-footer">
      <button class="np-btn" onclick="showNpForm(this)">+ New Person</button>
      <div class="np-form">
        <input class="np-input" placeholder="Enter name…"
          onkeydown="if(event.key==='Enter')submitNewPerson(this,'${ids}')"/>
        <button class="np-submit" onclick="submitNewPerson(this.previousElementSibling,'${ids}')">Add</button>
      </div>
    </div>
    <div class="drag-tip">&#x283F; drag to assign to a person above</div>
  </div>`;
}

function renderSection2() {
  const cards = DATA.clusters.map(c => clusterCard(c, true)).join('');
  return `<div class="sec-head"><h2>Unassigned clusters</h2>
    <span class="pill">${DATA.clusters.length} clusters</span>
    <span class="sec-note">Drag to a person · or create new person below</span></div>
    <div class="card-grid">${cards}</div>`;
}

function renderSection3() {
  const cards = DATA.singletons.map(f => clusterCard({faces:[f], cluster_id: null}, false)).join('');
  return `<div class="sec-head"><h2>Unassigned singletons</h2>
    <span class="pill">${DATA.singletons.length} faces</span>
    <span class="sec-note">Drag to a person · or create new person on hover</span></div>
    <div class="card-grid">${cards}</div>`;
}

function esc(s) { return s.replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])); }

function initDragDrop() {}

function startDrag(el) {
  dragData = { ids: el.dataset.ids.split(',').map(Number) };
  el.style.opacity = '0.5';
  setTimeout(() => el.style.opacity = '', 0);
}

async function dropOnPerson(label, el) {
  el.classList.remove('drop-over');
  if (!dragData) return;
  await fetch('/api/assign', { method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({ face_ids: dragData.ids, person_label: label }) });
  dragData = null;
  load();
}

async function dropToNew() {
  if (!dragData) return;
  const name = prompt('New person name:');
  if (!name) return;
  await fetch('/api/new-person', { method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({ face_ids: dragData.ids, label: name }) });
  dragData = null;
  load();
}

function showNpForm(btn) {
  btn.style.display = 'none';
  const form = btn.nextElementSibling;
  form.classList.add('show');
  form.querySelector('.np-input').focus();
}

async function submitNewPerson(input, ids) {
  const name = input.value.trim();
  if (!name) return;
  const face_ids = ids.split(',').map(Number);
  await fetch('/api/new-person', { method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({ face_ids, label: name }) });
  load();
}

async function removeFace(id) {
  await fetch('/api/remove-face', { method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({ face_id: id }) });
  load();
}

function openModal(label) {
  const person = DATA.people.find(p => p.label === label);
  if (!person) return;
  document.getElementById('modal-name').textContent = label;
  document.getElementById('modal-sub').textContent = person.faces.length + ' photos · click to set primary';
  document.getElementById('modal-photos').innerHTML = person.faces.map(f => `
    <div class="modal-photo" onclick="setPrimary('${esc(label)}',${f.id})">
      <span style="font-size:9px;color:var(--muted)">#${f.id}</span>
      ${f.id === person.primary_face_id ? '<div class="primary-badge">Primary</div>' : ''}
    </div>`).join('');
  document.getElementById('modal').style.display = 'flex';
}

async function setPrimary(label, faceId) {
  await fetch('/api/set-primary', { method:'POST', headers:{'Content-Type':'application/json'},
    body: JSON.stringify({ person_label: label, face_id: faceId }) });
  closeModal(); load();
}

function closeModal() { document.getElementById('modal').style.display = 'none'; }

function exportLabels() {
  const data = DATA.people.map(p => ({ label: p.label, face_ids: p.faces.map(f => f.id) }));
  const blob = new Blob([JSON.stringify(data, null, 2)], {type:'application/json'});
  const a = document.createElement('a'); a.href = URL.createObjectURL(blob);
  a.download = 'face-labels.json'; a.click();
}

async function saveAndClose() {
  await fetch('/api/quit', { method:'POST' });
  document.body.innerHTML = '<p style="padding:40px;font-family:system-ui">Saved. You can close this tab.</p>';
}

load();
</script>
</body></html>"#;
```

- [ ] **Step 2: Build and smoke-test**

```bash
cargo build -p dupe --bin dupe-report 2>&1 | tail -5
```

Expected: builds without errors.

Optionally start the server against a real DB and open `http://localhost:7878` in a browser to verify the three sections render and the "Save & close" button shuts the server down:

```bash
./target/debug/dupe-report ~/photos.db --faces
```

- [ ] **Step 3: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs
git commit -m "feat: faces labeling HTML/JS page served at localhost:7878"
```

---

## Task 12: dupe-search --person flag

**Files:**
- Modify: `crates/dupe-ml/src/bin/dupe-search.rs`

- [ ] **Step 1: Read the existing Args struct in dupe-search.rs to find the right insertion point**

Open `crates/dupe-ml/src/bin/dupe-search.rs` and locate the `Args` struct.

- [ ] **Step 2: Add --person to Args**

```rust
/// Find all photos containing this named person (requires dupe-report --faces labeling)
#[arg(long, conflicts_with = "query", conflicts_with = "image")]
person: Option<String>,
```

- [ ] **Step 3: Handle --person before the existing query/image dispatch**

At the start of `main()` (or wherever the existing query dispatch happens), add:

```rust
if let Some(name) = &args.person {
    let conn = rusqlite::Connection::open(&args.db)?;
    let paths = dupe_core::person_search::search_by_person(&conn, name, Some(args.k))?;
    if paths.is_empty() {
        eprintln!("No confirmed photos found for person: {name}");
    }
    for path in paths {
        println!("{path}");
    }
    return Ok(());
}
```

- [ ] **Step 4: Add dupe-core dependency to dupe-ml if not already present**

Check `crates/dupe-ml/Cargo.toml` - `dupe-core` should already be listed. If not, add:

```toml
dupe-core = { path = "../dupe-core" }
```

- [ ] **Step 5: Write an integration test**

Create `crates/dupe-ml/tests/person_search_cli.rs`:

```rust
use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn search_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); p.pop();
    p.push("dupe-search");
    p
}

fn make_db_with_person(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("t.db");
    let f = dir.join("alice.jpg");
    std::fs::write(&f, b"x").unwrap();
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE file_hashes (path TEXT PRIMARY KEY, hash TEXT NOT NULL,
         size_bytes INTEGER, created_at TEXT, modified_at TEXT, ext TEXT,
         phash INTEGER, exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
         CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL, bbox TEXT NOT NULL,
         landmark TEXT, embedding BLOB NOT NULL, cluster_id INTEGER, person_label TEXT, confirmed INTEGER DEFAULT 0);
         INSERT INTO file_hashes VALUES ('{}','habc',0,NULL,NULL,'jpg',NULL,NULL,NULL,NULL,NULL,NULL);
         INSERT INTO faces VALUES (1,'habc','0,0,50,50',NULL,X'0000',0,'Alice',1);",
        f.to_str().unwrap()
    )).unwrap();
    db
}

#[test]
fn person_flag_prints_path() {
    let dir = tempdir().unwrap();
    let db = make_db_with_person(dir.path());
    let out = Command::new(search_bin())
        .arg(&db).arg("--person").arg("Alice")
        .output().expect("run dupe-search");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("alice.jpg"), "expected path in output, got: {stdout}");
}

#[test]
fn person_flag_empty_for_unknown_name() {
    let dir = tempdir().unwrap();
    let db = make_db_with_person(dir.path());
    let out = Command::new(search_bin())
        .arg(&db).arg("--person").arg("Nobody")
        .output().expect("run dupe-search");
    assert!(out.stdout.is_empty());
}
```

- [ ] **Step 6: Build and run tests**

```bash
cargo build -p dupe-ml --bin dupe-search 2>&1 | tail -5
cargo test -p dupe-ml --test person_search_cli 2>&1 | tail -8
```

Expected: 2 tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/dupe-ml/src/bin/dupe-search.rs crates/dupe-ml/tests/person_search_cli.rs
git commit -m "feat: dupe-search --person flag for finding photos by name"
```

---

## Task 13: Docs and final verification

**Files:**
- Modify: `README.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add dupe-faces to README binaries table**

In the `## Binaries` table, add:

```markdown
| `dupe-faces` | Detect faces, embed with ArcFace, cluster by identity |
```

- [ ] **Step 2: Add dupe-faces quickstart step to README**

After the `dupe-embed` step in the quickstart, add:

```markdown
# 9. Detect and cluster faces
dupe-faces ~/photos.db

# 10. Label people in your browser
dupe-report ~/photos.db --faces

# 11. Find photos of a person
dupe-search ~/photos.db --person "Alice"
```

- [ ] **Step 3: Add dupe-faces section to README**

After the `## dupe-embed and dupe-search` section, add:

```markdown
## dupe-faces

Detects all faces in the scanned collection, embeds them with ArcFace (InsightFace buffalo_l), and clusters by identity using DBSCAN. Run after scanning.

\`\`\`bash
dupe-faces <db>               # process new images only
dupe-faces <db> --reprocess   # re-detect all images
dupe-faces <db> --dry-run     # detect without writing to db
dupe-faces <db> --batch 16    # detection batch size (default: 8)
\`\`\`

First run downloads ~200 MB of model weights (SCRFD + ArcFace) from Hugging Face into `~/.cache/huggingface/`. HEIC files require `sips` (macOS only). `.mov`, `.mp4`, and `.dng` are skipped.

After running, use `dupe-report <db> --faces` to open an interactive labeling server at `localhost:7878`. Assign clusters and singletons to named people. Labels with `confirmed=1` are used by `dupe-search --person`.
```

- [ ] **Step 4: Update dupe-search section in README**

Add `--person` to the dupe-search examples:

```bash
dupe-search ~/photos.db --person "Alice"           # find all photos of Alice
```

- [ ] **Step 5: Update CLAUDE.md**

In the `## Build & run` section add:

```bash
./target/release/dupe-faces ~/photos.db             # detect and cluster faces
./target/release/dupe-report ~/photos.db --faces    # interactive face labeling server
./target/release/dupe-search ~/photos.db --person "Alice"  # find photos by person
```

In the project structure `crates/dupe-ml/src/` add:
```
src/{face_detect.rs,face_align.rs,face_embed.rs,face_models.rs,bin/dupe-faces.rs}
```

In the `## SQLite schema` section add the `faces` table DDL.

- [ ] **Step 6: Build release binaries**

```bash
cargo build --release 2>&1 | tail -5
```

Expected: all binaries build with no errors.

- [ ] **Step 7: Run full test suite**

```bash
cargo test 2>&1 | tail -15
```

Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add README.md CLAUDE.md
git commit -m "docs: document dupe-faces, --faces server, and --person search"
```
