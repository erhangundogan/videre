# dupe-report All-Files Gallery and Similarity Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `--all` flag to `dupe-report` that renders every file (singular and duplicate) in the HTML page with client-side "find similar" image search over the existing SigLIP embeddings.

**Architecture:** The report stays a single static HTML file. Rust reads the `embeddings` table, concatenates the raw f16 blobs, and inlines them as one base64 constant plus a parallel hash array. Page JS decodes f16 to a flat Float32Array once at load and answers "Similar" clicks with a brute-force dot product (vectors are L2-normalized, so dot = cosine). Results render in a dedicated panel at the top; the gallery below lists all files without KEEP/REMOVE badges on singular files.

**Tech Stack:** Rust (rusqlite, existing report generator), vanilla JS embedded in the generated HTML. No new external crates. The `dupe` crate gains a path dependency on `dupe-core` so the report can name the model id without pulling in candle.

**Spec:** `docs/superpowers/specs/2026-07-09-report-search-design.md`

Read the spec before starting. Key constraints:
- Without `--all` the output must be unchanged (no vectors, no gallery, no new markup).
- Never use the em dash character in code, comments, docs, or commit messages.
- No Co-Authored-By trailer on commits.

---

## File map

- Modify: `crates/dupe-core/src/embeddings.rs` (add `DEFAULT_MODEL_ID` constant)
- Modify: `crates/dupe-ml/src/model.rs` (reuse the constant, single source of truth)
- Modify: `crates/dupe/Cargo.toml` (add `dupe-core` dependency)
- Modify: `crates/dupe/src/bin/dupe_report.rs` (flag, queries, vector block, gallery, JS)
- Create: `crates/dupe/tests/report.rs` (integration tests running the built binary)
- Modify: `CLAUDE.md`, `README.md` (document `--all`)

`dupe_report.rs` is a standalone binary target; its helper functions get unit tests in a `#[cfg(test)]` module at the bottom of the same file (`cargo test -p dupe --bin dupe-report`).

---

### Task 1: Move the model id constant to dupe-core

The report needs the model id to query `embeddings`, but `crates/dupe` must not depend on `dupe-ml` (that would pull candle/tokenizers into the core tool). Move the constant down to `dupe-core` and have `dupe-ml` re-export it.

**Files:**
- Modify: `crates/dupe-core/src/embeddings.rs`
- Modify: `crates/dupe-ml/src/model.rs:13`

- [ ] **Step 1: Add the constant with a test in dupe-core**

In `crates/dupe-core/src/embeddings.rs`, directly below the `EMBEDDABLE_EXTS` constant, add:

```rust
/// Model id used by dupe-embed / dupe-search / dupe-report. Single source of
/// truth so the report binary can query embeddings without depending on dupe-ml.
pub const DEFAULT_MODEL_ID: &str = "google/siglip-so400m-patch14-384";
```

In the `tests` module of the same file, add:

```rust
#[test]
fn default_model_id_is_the_siglip_checkpoint() {
    assert_eq!(DEFAULT_MODEL_ID, "google/siglip-so400m-patch14-384");
}
```

- [ ] **Step 2: Point dupe-ml at the constant**

In `crates/dupe-ml/src/model.rs` replace line 13:

```rust
pub const MODEL_ID: &str = "google/siglip-so400m-patch14-384";
```

with:

```rust
pub const MODEL_ID: &str = dupe_core::embeddings::DEFAULT_MODEL_ID;
```

- [ ] **Step 3: Build and test the workspace**

Run: `cargo test -p dupe-core && cargo build -p dupe-ml`
Expected: dupe-core tests pass including `default_model_id_is_the_siglip_checkpoint`; dupe-ml compiles (its binaries still see the same `model::MODEL_ID` value).

- [ ] **Step 4: Commit**

```bash
git add crates/dupe-core/src/embeddings.rs crates/dupe-ml/src/model.rs
git commit -m "refactor: move SigLIP model id constant to dupe-core"
```

---

### Task 2: dupe-core dependency and full hash in the per-file JSON

Every image card needs the full content hash so JS can link cards to vectors. Today `file_to_json` omits the hash (groups only carry an 8-char prefix).

**Files:**
- Modify: `crates/dupe/Cargo.toml`
- Modify: `crates/dupe/src/bin/dupe_report.rs` (function `file_to_json`, new test module)

- [ ] **Step 1: Add the dependency**

In `crates/dupe/Cargo.toml` under `[dependencies]` add:

```toml
dupe-core = { path = "../dupe-core" }
```

- [ ] **Step 2: Write the failing unit test**

At the bottom of `crates/dupe/src/bin/dupe_report.rs` add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, hash: &str, ext: &str) -> FileRow {
        FileRow {
            path: path.to_string(),
            hash: hash.to_string(),
            size_bytes: 100,
            ext: ext.to_string(),
            created_at: None,
            modified_at: None,
            exif_date: None,
            gps_lat: None,
            gps_lon: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn file_json_includes_full_hash() {
        let f = row("/a/x.jpg", "deadbeefcafe", "jpg");
        let json = file_to_json(&f, false, false);
        assert!(json.contains("\"hash\":\"deadbeefcafe\""), "{json}");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p dupe --bin dupe-report file_json_includes_full_hash`
Expected: FAIL (assertion, no `"hash"` key in the JSON)

- [ ] **Step 4: Add the hash field**

In `file_to_json` (around `crates/dupe/src/bin/dupe_report.rs:161`), change the format string to emit the hash first:

```rust
    format!(
        "{{\"hash\":{hash},\"path\":{path},\"ext\":{ext},\"size\":{size},\
         \"cr\":{cr},\"mo\":{mo},\"ex\":{ex},\
         \"lat\":{lat},\"lon\":{lon},\"w\":{w},\"h\":{h},\
         \"tb\":{tb},\"fb\":{fb}}}",
        hash = json_str(&f.hash),
        path = json_str(&f.path),
        ext  = json_str(&f.ext),
        size = f.size_bytes,
        cr = cr, mo = mo, ex = ex,
        lat = lat, lon = lon, w = w, h = h,
        tb = tb, fb = fb,
    )
```

Note: adding a key to this JSON is additive; the existing JS reads named fields and ignores extras. This does change the byte content of reports even without `--all`, which is acceptable: the spec's regression requirement is about structure and behavior (no vectors, no gallery), and the integration test in Task 4 pins that.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p dupe --bin dupe-report`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/dupe/Cargo.toml crates/dupe/src/bin/dupe_report.rs Cargo.lock
git commit -m "feat: emit full content hash in report per-file JSON"
```

---

### Task 3: Vector block query and script emission

Rust side of the search data flow: read all embeddings for the model, concatenate blobs in hash order, base64 once.

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`

- [ ] **Step 1: Write the failing unit tests**

Add to the `tests` module in `dupe_report.rs`:

```rust
    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
                exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER
            );",
        )
        .unwrap();
        conn
    }

    fn add_embeddings_table(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE embeddings (
                hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
                embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
            );",
        )
        .unwrap();
    }

    #[test]
    fn query_vectors_returns_none_without_table() {
        let conn = mem_db();
        assert!(query_vectors(&conn).is_none());
    }

    #[test]
    fn query_vectors_returns_none_when_empty() {
        let conn = mem_db();
        add_embeddings_table(&conn);
        assert!(query_vectors(&conn).is_none());
    }

    #[test]
    fn query_vectors_orders_by_hash_and_encodes_f16() {
        let conn = mem_db();
        add_embeddings_table(&conn);
        // f16 1.0 = 0x3C00 little-endian = [0x00, 0x3C]
        let one = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let two = dupe_core::vectors::to_f16_bytes(&[0.0, 1.0]);
        // Insert out of order to prove ORDER BY hash
        conn.execute(
            "INSERT INTO embeddings VALUES ('bbb', ?1, ?2, 'now')",
            rusqlite::params![dupe_core::embeddings::DEFAULT_MODEL_ID, two],
        ).unwrap();
        conn.execute(
            "INSERT INTO embeddings VALUES ('aaa', ?1, ?2, 'now')",
            rusqlite::params![dupe_core::embeddings::DEFAULT_MODEL_ID, one.clone()],
        ).unwrap();
        // Wrong model id must be excluded
        conn.execute(
            "INSERT INTO embeddings VALUES ('ccc', 'other-model', ?1, 'now')",
            rusqlite::params![one],
        ).unwrap();

        let vb = query_vectors(&conn).unwrap();
        assert_eq!(vb.hashes, vec!["aaa".to_string(), "bbb".to_string()]);
        assert_eq!(vb.dim, 2);
        // blob = [00 3C 00 00] ++ [00 00 00 3C]
        let expected = base64_encode(&[0x00, 0x3C, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3C]);
        assert_eq!(vb.b64, expected);
    }

    #[test]
    fn query_vectors_skips_rows_with_wrong_dimension() {
        let conn = mem_db();
        add_embeddings_table(&conn);
        let good = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let bad = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0, 0.0]); // 3 dims
        conn.execute(
            "INSERT INTO embeddings VALUES ('aaa', ?1, ?2, 'now')",
            rusqlite::params![dupe_core::embeddings::DEFAULT_MODEL_ID, good],
        ).unwrap();
        conn.execute(
            "INSERT INTO embeddings VALUES ('bbb', ?1, ?2, 'now')",
            rusqlite::params![dupe_core::embeddings::DEFAULT_MODEL_ID, bad],
        ).unwrap();
        let vb = query_vectors(&conn).unwrap();
        assert_eq!(vb.hashes, vec!["aaa".to_string()]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dupe --bin dupe-report query_vectors`
Expected: FAIL to compile (`query_vectors` and `VectorBlock` not defined)

- [ ] **Step 3: Implement query_vectors**

Add to `dupe_report.rs` after the `Stats` struct:

```rust
struct VectorBlock {
    hashes: Vec<String>,
    b64: String,
    dim: usize,
}

/// Load all embeddings for the default model, ordered by hash, as one
/// base64-encoded f16 buffer. Returns None when the table is missing or empty.
/// Rows whose blob length disagrees with the first row's dimension are skipped
/// (mirrors search.rs semantics for corrupt rows).
fn query_vectors(conn: &Connection) -> Option<VectorBlock> {
    let table_exists = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='embeddings'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if !table_exists {
        return None;
    }
    let mut stmt = conn
        .prepare("SELECT hash, embedding FROM embeddings WHERE model_id = ?1 ORDER BY hash")
        .ok()?;
    let rows: Vec<(String, Vec<u8>)> = stmt
        .query_map([dupe_core::embeddings::DEFAULT_MODEL_ID], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    let first_len = rows.iter().map(|(_, b)| b.len()).find(|l| *l > 0 && l % 2 == 0)?;
    let dim = first_len / 2;
    let mut blob = Vec::with_capacity(rows.len() * first_len);
    let mut hashes = Vec::with_capacity(rows.len());
    for (hash, bytes) in rows {
        if bytes.len() != first_len {
            continue;
        }
        blob.extend_from_slice(&bytes);
        hashes.push(hash);
    }
    if hashes.is_empty() {
        return None;
    }
    Some(VectorBlock { hashes, b64: base64_encode(&blob), dim })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p dupe --bin dupe-report`
Expected: PASS (all four query_vectors tests plus the Task 2 test)

- [ ] **Step 5: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs
git commit -m "feat: query embeddings into a base64 f16 vector block for the report"
```

---

### Task 4: --all flag, all-files query, and script constants

Wire the CLI flag through `main` and `generate_html`. With `--all`: emit `ALLFILES` (every row in `file_hashes`) and, when embeddings exist, `VEC_B64` / `VEC_HASHES` / `VEC_DIM`. Without `--all`: emit none of them.

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`
- Create: `crates/dupe/tests/report.rs`

- [ ] **Step 1: Write the failing integration tests**

Create `crates/dupe/tests/report.rs`:

```rust
use rusqlite::Connection;
use std::process::Command;
use tempfile::tempdir;

fn report_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // deps/
    path.pop(); // debug/
    path.push("dupe-report");
    path
}

/// Fixture: two duplicates (hash hdup), one singular (hsing), one video (hvid).
/// Embeddings exist for hdup and hsing.
fn fixture_db(dir: &std::path::Path, with_embeddings: bool) -> std::path::PathBuf {
    let db = dir.join("test.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE file_hashes (
            path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
            created_at TEXT, modified_at TEXT, ext TEXT, phash INTEGER,
            exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER
        );",
    )
    .unwrap();
    for (path, hash, ext) in [
        ("/pics/a.jpg", "hdup", "jpg"),
        ("/pics/b.jpg", "hdup", "jpg"),
        ("/pics/c.jpg", "hsing", "jpg"),
        ("/pics/d.mov", "hvid", "mov"),
    ] {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, size_bytes, ext) VALUES (?1, ?2, 100, ?3)",
            rusqlite::params![path, hash, ext],
        )
        .unwrap();
    }
    if with_embeddings {
        conn.execute_batch(
            "CREATE TABLE embeddings (
                hash TEXT PRIMARY KEY, model_id TEXT NOT NULL,
                embedding BLOB NOT NULL, embedded_at TEXT NOT NULL
            );",
        )
        .unwrap();
        let v1 = dupe_core::vectors::to_f16_bytes(&[1.0, 0.0]);
        let v2 = dupe_core::vectors::to_f16_bytes(&[0.0, 1.0]);
        for (hash, v) in [("hdup", v1), ("hsing", v2)] {
            conn.execute(
                "INSERT INTO embeddings VALUES (?1, ?2, ?3, 'now')",
                rusqlite::params![hash, dupe_core::embeddings::DEFAULT_MODEL_ID, v],
            )
            .unwrap();
        }
    }
    db
}

fn run_report(db: &std::path::Path, all: bool) -> String {
    let out = db.with_extension("html");
    let mut cmd = Command::new(report_bin());
    cmd.arg(db).arg("-o").arg(&out);
    if all {
        cmd.arg("--all");
    }
    let status = cmd.status().expect("failed to run dupe-report");
    assert!(status.success());
    std::fs::read_to_string(&out).unwrap()
}

#[test]
fn without_all_flag_no_gallery_or_vectors() {
    let dir = tempdir().unwrap();
    let db = fixture_db(dir.path(), true);
    let html = run_report(&db, false);
    assert!(!html.contains("VEC_B64"));
    assert!(!html.contains("ALLFILES"));
    assert!(!html.contains("id=\"gallery\""));
}

#[test]
fn all_flag_emits_gallery_and_vectors() {
    let dir = tempdir().unwrap();
    let db = fixture_db(dir.path(), true);
    let html = run_report(&db, true);
    assert!(html.contains("var ALLFILES="));
    // All four files present, including the singular and the video
    assert!(html.contains("/pics/c.jpg"));
    assert!(html.contains("/pics/d.mov"));
    assert!(html.contains("var VEC_B64=\""));
    assert!(html.contains("var VEC_HASHES="));
    assert!(html.contains("\"hdup\""));
    assert!(html.contains("\"hsing\""));
    assert!(html.contains("var VEC_DIM=2;"));
    assert!(html.contains("id=\"gallery\""));
    assert!(html.contains("id=\"results\""));
}

#[test]
fn all_flag_without_embeddings_renders_gallery_only() {
    let dir = tempdir().unwrap();
    let db = fixture_db(dir.path(), false);
    let html = run_report(&db, true);
    assert!(html.contains("var ALLFILES="));
    assert!(html.contains("id=\"gallery\""));
    assert!(!html.contains("var VEC_B64"));
    // JS must guard on empty vectors: constants exist but empty
    assert!(html.contains("var VEC_HASHES=[];"));
}
```

Note: `dupe-core` must be reachable from this test. It is a regular dependency of the `dupe` crate after Task 2, so `use` from integration tests works via the crate graph only if re-exported. Simplest fix: add `dupe-core` to `[dev-dependencies]` too:

```toml
[dev-dependencies]
tempfile = "3"
rusqlite = { version = "0.32", features = ["bundled"] }
serde_json = "1"
dupe-core = { path = "../dupe-core" }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dupe --test report`
Expected: FAIL (`--all` flag unknown, exits nonzero; or assertions on missing markers)

- [ ] **Step 3: Implement flag, query, and constant emission**

In `dupe_report.rs`:

1. Add to `Args`:

```rust
    /// Include every file (singular and duplicate) in a searchable gallery
    #[arg(long)]
    all: bool,
```

2. Add an all-files query after `query_groups`:

```rust
fn query_all_files(conn: &Connection) -> Vec<FileRow> {
    let mut stmt = conn
        .prepare(
            "SELECT path, hash, size_bytes, COALESCE(ext,''), created_at, modified_at, exif_date, \
                    gps_lat, gps_lon, width, height \
             FROM file_hashes ORDER BY path",
        )
        .expect("failed to prepare query");
    stmt.query_map([], |r| {
        Ok(FileRow {
            path:       r.get(0)?,
            hash:       r.get(1)?,
            size_bytes: r.get(2)?,
            ext:        r.get(3)?,
            created_at: r.get(4)?,
            modified_at:r.get(5)?,
            exif_date:  r.get(6)?,
            gps_lat:    r.get(7)?,
            gps_lon:    r.get(8)?,
            width:      r.get(9)?,
            height:     r.get(10)?,
        })
    })
    .expect("failed to execute query")
    .filter_map(|r| r.ok())
    .collect()
}
```

3. Change `generate_html` signature to:

```rust
fn generate_html(
    db_path: &str,
    stats: &Stats,
    groups: &[Vec<FileRow>],
    all_files: Option<&[FileRow]>,   // Some(..) only with --all
    vectors: Option<&VectorBlock>,   // Some(..) only with --all and embeddings present
    heic: bool,
    heic_original: bool,
) -> String {
```

4. In `generate_html`, right after the `GROUPS` array is written (after the `out.push_str("\n];\n");` at line ~423), emit the new constants:

```rust
    // All-files gallery data and similarity vectors (--all only)
    if let Some(files) = all_files {
        out.push_str("var ALLFILES=[\n");
        for (i, f) in files.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push_str(&file_to_json(f, heic, heic_original));
        }
        out.push_str("\n];\n");
        match vectors {
            Some(vb) => {
                out.push_str(&format!("var VEC_DIM={};\n", vb.dim));
                out.push_str("var VEC_HASHES=[");
                for (i, h) in vb.hashes.iter().enumerate() {
                    if i > 0 { out.push(','); }
                    out.push_str(&json_str(h));
                }
                out.push_str("];\n");
                out.push_str("var VEC_B64=\"");
                out.push_str(&vb.b64);
                out.push_str("\";\n");
            }
            None => {
                out.push_str("var VEC_DIM=0;\nvar VEC_HASHES=[];\nvar VEC_B64=\"\";\n");
            }
        }
    } else {
        out.push_str("var ALLFILES=null;\nvar VEC_DIM=0;\nvar VEC_HASHES=[];\nvar VEC_B64=\"\";\n");
    }
```

Wait: the spec requires NO new constants at all without `--all` (`!html.contains("VEC_B64")` in the test above). So the `else` branch must emit nothing and the JS added in Task 5 must guard with `typeof ALLFILES!=='undefined'`. Use this instead:

```rust
    // All-files gallery data and similarity vectors (--all only).
    // Without --all nothing is emitted so the page is unchanged.
    if let Some(files) = all_files {
        out.push_str("var ALLFILES=[\n");
        for (i, f) in files.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push_str(&file_to_json(f, heic, heic_original));
        }
        out.push_str("\n];\n");
        match vectors {
            Some(vb) => {
                out.push_str(&format!("var VEC_DIM={};\n", vb.dim));
                out.push_str("var VEC_HASHES=[");
                for (i, h) in vb.hashes.iter().enumerate() {
                    if i > 0 { out.push(','); }
                    out.push_str(&json_str(h));
                }
                out.push_str("];\n");
                out.push_str("var VEC_B64=\"");
                out.push_str(&vb.b64);
                out.push_str("\";\n");
            }
            None => {
                out.push_str("var VEC_DIM=0;\nvar VEC_HASHES=[];\nvar VEC_B64=\"\";\n");
            }
        }
    }
```

5. Also inside `generate_html`, emit the results panel and gallery containers only with `--all`. After the `more-wrap` div (line ~414), add:

```rust
    if all_files.is_some() {
        let n = all_files.map(|f| f.len()).unwrap_or(0);
        out.push_str(&format!(
            "<div class=\"results-panel\" id=\"results\" style=\"display:none\"></div>\n\
             <div class=\"gallery-head\"><h2>All files</h2><span class=\"info\" id=\"gallery-info\">{n} files</span></div>\n\
             <div class=\"gallery\" id=\"gallery\"></div>\n\
             <div class=\"more-wrap\"><button id=\"gallery-more\" onclick=\"showMoreGallery()\"></button></div>\n"
        ));
    }
```

Placement note: the results panel `div#results` in the DOM sits here, but CSS in Task 5 pins it visually below the toolbar with `position:sticky` omitted; it is fine for v1 that results appear above the gallery and the page scrolls to it on search.

Correction for spec fidelity ("results panel at top"): place the results div directly after the toolbar instead. Concretely, emit it in the toolbar section: after the existing toolbar `push_str` (line ~406), add:

```rust
    if all_files.is_some() {
        out.push_str("<div class=\"results-panel\" id=\"results\" style=\"display:none\"></div>\n");
    }
```

and only the gallery head/gallery/more button after `more-wrap`. The `id="results"` emission happens exactly once (after the toolbar, not after more-wrap).

6. Stats header: add the embedded count stat. Replace the stats `format!` block's closing `</div></div>` handling by adding one more stat div when vectors are present. Simplest: build an extra string before the header `push_str`:

```rust
    let embedded_stat = match vectors {
        Some(vb) => format!(
            "<div class=\"stat\"><span class=\"num\">{}</span><span class=\"label\">Embedded</span></div>",
            vb.hashes.len()
        ),
        None => String::new(),
    };
```

and include `{embedded_stat}` inside the `.stats` div in the header format string (after the wasted-space stat div):

```rust
            <div class=\"stat warn\"><span class=\"num\">{wasted}</span><span class=\"label\">Wasted space</span></div>\
            {embedded_stat}\
```

(with `embedded_stat = embedded_stat,` added to the format arguments).

7. In `main`, wire it up (replace the current stats/groups/html block):

```rust
    let conn = Connection::open(&args.db).expect("failed to open database");
    let stats = query_stats(&conn);
    let groups = query_groups(&conn);
    let all_files = args.all.then(|| query_all_files(&conn));
    let vectors = if args.all {
        let v = query_vectors(&conn);
        if v.is_none() {
            eprintln!("no embeddings found; run dupe-embed for similarity search");
        }
        v
    } else {
        None
    };
    let html = generate_html(
        &args.db.to_string_lossy(),
        &stats,
        &groups,
        all_files.as_deref(),
        vectors.as_ref(),
        args.heic,
        args.heic_original,
    );
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p dupe`
Expected: `without_all_flag_no_gallery_or_vectors` PASSES. `all_flag_emits_gallery_and_vectors` PASSES (containers and constants exist). `all_flag_without_embeddings_renders_gallery_only` PASSES. Unit tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/tests/report.rs crates/dupe/Cargo.toml Cargo.lock
git commit -m "feat: add --all flag emitting gallery containers and vector constants"
```

---

### Task 5: Page CSS and JS for gallery rendering and similarity search

All client-side behavior: f16 decode, gallery cards, Similar buttons, results panel, Clear. Everything is appended to the existing CSS and JS string blocks in `generate_html`; every addition is inert when `ALLFILES` is undefined (report without `--all` is behaviorally unchanged).

**Files:**
- Modify: `crates/dupe/src/bin/dupe_report.rs`

- [ ] **Step 1: Extend the integration test with JS/CSS markers**

Add to `crates/dupe/tests/report.rs`:

```rust
#[test]
fn all_flag_page_contains_similarity_js() {
    let dir = tempdir().unwrap();
    let db = fixture_db(dir.path(), true);
    let html = run_report(&db, true);
    for marker in [
        "function decodeVecs(",
        "function findSimilar(",
        "function renderResults(",
        "function clearResults(",
        "function buildCard(",
        "function showMoreGallery(",
        "data-similar=",
        ".results-panel",
        ".gallery{",
    ] {
        assert!(html.contains(marker), "missing marker: {marker}");
    }
}

#[test]
fn without_all_flag_no_similarity_js_side_effects() {
    let dir = tempdir().unwrap();
    let db = fixture_db(dir.path(), true);
    let html = run_report(&db, false);
    // The shared JS block may define functions, but nothing may reference
    // gallery containers unconditionally. The cheap proof: no gallery ids.
    assert!(!html.contains("id=\"gallery\""));
    assert!(!html.contains("id=\"results\""));
}
```

- [ ] **Step 2: Run tests to verify the new one fails**

Run: `cargo test -p dupe --test report all_flag_page_contains_similarity_js`
Expected: FAIL (markers missing)

- [ ] **Step 3: Add CSS**

In the `concat!` style block of `generate_html` (before `"</style>\n</head>..."`), append these lines:

```rust
        ".results-panel{margin:16px 32px;padding:14px 16px;background:#fff;",
        "border:1px solid #e4e4e7;border-radius:10px}\n",
        ".results-head{display:flex;align-items:center;gap:10px;margin-bottom:10px}\n",
        ".results-head h2{font-size:14px}\n",
        ".results-strip{display:flex;gap:10px;overflow-x:auto;padding-bottom:6px}\n",
        ".rcard{flex:0 0 auto;width:132px;text-align:center;position:relative}\n",
        ".rcard .thumb{max-width:120px;max-height:120px}\n",
        ".rcard.query{border-right:2px solid #e4e4e7;padding-right:10px;margin-right:4px}\n",
        ".score{position:absolute;top:4px;left:8px;background:rgba(24,24,27,.75);color:#fff;",
        "font-size:10px;padding:1px 5px;border-radius:4px}\n",
        ".copies{position:absolute;top:4px;right:8px;background:#fbbf24;color:#18181b;",
        "font-size:10px;font-weight:700;padding:1px 5px;border-radius:4px}\n",
        ".rname{font-size:11px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;",
        "color:#52525b;margin-top:2px}\n",
        ".gallery-head{padding:20px 32px 4px;display:flex;align-items:baseline;gap:12px}\n",
        ".gallery-head h2{font-size:16px}\n",
        ".gallery{padding:12px 32px;display:grid;",
        "grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:10px}\n",
        ".card{background:#fff;border:1px solid #e4e4e7;border-radius:10px;padding:8px;",
        "text-align:center;position:relative}\n",
        ".card .thumb{max-width:100%;max-height:130px}\n",
        ".card-meta{font-size:11px;color:#71717a;margin-top:4px;white-space:nowrap;",
        "overflow:hidden;text-overflow:ellipsis}\n",
        ".similar-btn{margin-top:6px;padding:2px 10px;font-size:11px}\n",
```

- [ ] **Step 4: Add JS**

Append a second raw-string JS block right before `out.push_str("</script>\n</body>\n</html>");`:

```rust
    out.push_str(r#"
// ---- All-files gallery and similarity search (active only with --all) ----
var GPAGE=200,gShown=0,HASH_FILES={},VECS=null,VEC_INDEX={};
function decodeVecs(b64,n,dim){
  var bin=atob(b64);
  var out=new Float32Array(n*dim);
  for(var i=0;i<n*dim;i++){
    var lo=bin.charCodeAt(i*2),hi=bin.charCodeAt(i*2+1);
    var h=(hi<<8)|lo;
    var s=(h&0x8000)?-1:1,e=(h>>10)&0x1f,f=h&0x3ff;
    if(e===0)out[i]=s*f*Math.pow(2,-24);
    else if(e===31)out[i]=f?NaN:s*Infinity;
    else out[i]=s*(1+f/1024)*Math.pow(2,e-15);
  }
  return out;
}
function bestDateJs(f){
  if(f.ex&&f.ex.indexOf('0000')!==0)return f.ex;
  if(f.cr&&f.mo)return f.cr<f.mo?f.cr:f.mo;
  return f.cr||f.mo||'';
}
function similarBtn(hash){
  if(!VECS||VEC_INDEX[hash]==null)return '';
  return '<button class="similar-btn" data-similar="'+escA(hash)+'">Similar</button>';
}
function buildCard(f){
  var fname=f.path.split('/').pop()||f.path;
  var copies=HASH_FILES[f.hash]&&HASH_FILES[f.hash].length>1?
    '<span class="copies">x'+HASH_FILES[f.hash].length+'</span>':'';
  return '<div class="card" data-hash="'+escA(f.hash)+'">'+copies+
    buildPreview(f)+
    '<div class="card-meta" title="'+escA(f.path)+'">'+escH(fname)+'</div>'+
    '<div class="card-meta">'+fmtB(f.size)+(bestDateJs(f)?' &middot; '+escH(bestDateJs(f)):'')+'</div>'+
    similarBtn(f.hash)+
    '</div>';
}
function renderGallery(){
  if(typeof ALLFILES==='undefined')return;
  var g=document.getElementById('gallery');
  var end=Math.min(gShown+GPAGE,ALLFILES.length);
  var html='';
  for(var i=gShown;i<end;i++)html+=buildCard(ALLFILES[i]);
  var tmp=document.createElement('div');
  tmp.innerHTML=html;
  while(tmp.firstChild)g.appendChild(tmp.firstChild);
  gShown=end;
  var btn=document.getElementById('gallery-more');
  var rem=ALLFILES.length-gShown;
  if(rem>0){btn.style.display='inline-block';btn.textContent='Show more ('+rem+' remaining)';}
  else btn.style.display='none';
}
function showMoreGallery(){renderGallery();}
function findSimilar(hash){
  var qi=VEC_INDEX[hash];
  if(qi==null||!VECS)return;
  var q=VECS.subarray(qi*VEC_DIM,(qi+1)*VEC_DIM);
  var scores=[];
  for(var i=0;i<VEC_HASHES.length;i++){
    if(i===qi)continue;
    var v=VECS.subarray(i*VEC_DIM,(i+1)*VEC_DIM);
    var dot=0;
    for(var d=0;d<VEC_DIM;d++)dot+=q[d]*v[d];
    if(isFinite(dot))scores.push([i,dot]);
  }
  scores.sort(function(a,b){return b[1]-a[1];});
  renderResults(hash,scores.slice(0,24));
}
function resultCard(hash,score,isQuery){
  var files=HASH_FILES[hash];
  if(!files||!files.length)return '';
  var f=files[0];
  var fname=f.path.split('/').pop()||f.path;
  var badge=isQuery?'':'<span class="score">'+score.toFixed(3)+'</span>';
  var copies=files.length>1?'<span class="copies">x'+files.length+'</span>':'';
  return '<div class="rcard'+(isQuery?' query':'')+'" data-hash="'+escA(hash)+'">'+
    badge+copies+buildPreview(f)+
    '<div class="rname" title="'+escA(f.path)+'">'+(isQuery?'query: ':'')+escH(fname)+'</div>'+
    '</div>';
}
function renderResults(qHash,scored){
  var panel=document.getElementById('results');
  var html='<div class="results-head"><h2>Similar images</h2>'+
    '<button onclick="clearResults()">Clear</button></div>'+
    '<div class="results-strip">'+resultCard(qHash,1,true);
  for(var i=0;i<scored.length;i++){
    html+=resultCard(VEC_HASHES[scored[i][0]],scored[i][1],false);
  }
  html+='</div>';
  panel.innerHTML=html;
  panel.style.display='block';
  panel.querySelectorAll('img').forEach(function(img){if(img.loading==='lazy')img.loading='eager';});
  panel.scrollIntoView({behavior:'smooth',block:'start'});
}
function clearResults(){
  var panel=document.getElementById('results');
  panel.style.display='none';
  panel.innerHTML='';
}
if(typeof ALLFILES!=='undefined'){
  ALLFILES.forEach(function(f){
    (HASH_FILES[f.hash]=HASH_FILES[f.hash]||[]).push(f);
  });
  if(VEC_HASHES.length>0){
    VECS=decodeVecs(VEC_B64,VEC_HASHES.length,VEC_DIM);
    for(var vi=0;vi<VEC_HASHES.length;vi++)VEC_INDEX[VEC_HASHES[vi]]=vi;
  }
  renderGallery();
}
document.addEventListener('click',function(e){
  var sb=e.target.closest('[data-similar]');
  if(sb){e.preventDefault();e.stopPropagation();findSimilar(sb.dataset.similar);}
});
"#);
```

- [ ] **Step 5: Add Similar buttons to duplicate-group rows**

In the existing `buildRow` JS function, the badge cell currently reads:

```js
    '<td class="badge"><span class="'+bc+'">'+bt+'</span></td>'+
```

Replace it with:

```js
    '<td class="badge"><span class="'+bc+'">'+bt+'</span>'+
    (typeof similarBtn==='function'?similarBtn(f.hash):'')+'</td>'+
```

Note: `buildRow` is defined in the first JS block and `similarBtn` in the second; by the time a click renders rows both exist, and the initial `render(true)` call at the end of the first block runs before `similarBtn` is defined. Move the initial `render(true);` line from the end of the first JS block to the very end of the second JS block (after the `document.addEventListener('click', ...)` for data-similar). This keeps one render entry point and guarantees `similarBtn` exists during row building.

The `similarBtn` helper itself returns an empty string when `VECS` is null, so duplicate rows show no button without `--all` or without embeddings.

- [ ] **Step 6: Run all tests**

Run: `cargo test -p dupe`
Expected: PASS, including `all_flag_page_contains_similarity_js` and `without_all_flag_no_similarity_js_side_effects`

- [ ] **Step 7: Manual browser check with fixture data**

```bash
cargo build --release
# Generate a report from the real demo database
./target/release/dupe-report ~/dupe-demo.db --all -o /tmp/report_all.html
open /tmp/report_all.html
```

(Use the actual demo db path from the scan session, e.g. the SQLite file used with `dupe --output-sqlite`.)

Verify in the browser:
- Gallery renders with thumbnails, filename, size, date; no KEEP/REMOVE badges on singular cards
- "Similar" buttons appear on embedded images only (not on mov/mp4/dng cards)
- Clicking Similar shows the results panel at the top with the query card, scored matches, and a working Clear button
- Duplicate groups unchanged; Similar buttons in group rows work
- Lightbox opens from gallery and results cards
- A report generated WITHOUT `--all` looks and behaves exactly as before

- [ ] **Step 8: Commit**

```bash
git add crates/dupe/src/bin/dupe_report.rs crates/dupe/tests/report.rs
git commit -m "feat: gallery rendering and in-page similarity search JS"
```

---

### Task 6: Documentation and final verification

**Files:**
- Modify: `CLAUDE.md` (dupe-report section)
- Modify: `README.md` (dupe-report section, semantic search section, recommended workflow)
- Modify: `docs/superpowers/specs/2026-07-09-report-search-design.md` (Status: Approved to Implemented)

- [ ] **Step 1: Update CLAUDE.md**

In the `## dupe-report` section, add `--all` to the usage block:

```
dupe-report <db> --all              # include all files in a searchable gallery
```

And append to the report feature list:

```
- `--all`: adds an "All files" gallery of every scanned file (no KEEP/REMOVE badges on singular files) with client-side "find similar" search using SigLIP embeddings from dupe-embed (vectors inlined as base64 f16; brute-force cosine in JS). Without embeddings the gallery renders and search is hidden.
```

- [ ] **Step 2: Update README.md**

In the `### HTML report (dupe-report)` section, add to the command list:

```bash
dupe-report ~/photos.db --all                   # all files + similarity search (needs dupe-embed)
```

And add a bullet to "The report shows:":

```
- With `--all`: an "All files" gallery of every scanned file and a "Similar" button on embedded images; results appear in a panel at the top ranked by cosine score. Run `dupe-embed` first to enable search.
```

In the "Recommended workflow", extend step 6 or add step 7 showing the visual flow:

```bash
# 7. Visual search in the browser (after dupe-embed)
dupe-report ~/photos.db --all
```

- [ ] **Step 3: Mark the spec implemented**

In `docs/superpowers/specs/2026-07-09-report-search-design.md` change `Status: Approved` to `Status: Implemented`.

- [ ] **Step 4: Full workspace verification**

Run: `cargo test --workspace && cargo build --release`
Expected: all tests pass, release build clean.

- [ ] **Step 5: Commit and push**

```bash
git add CLAUDE.md README.md docs/superpowers/specs/2026-07-09-report-search-design.md
git commit -m "docs: document dupe-report --all gallery and similarity search"
git push git@github.com:erhangundogan/dupe.git main
```

---

## Self-review notes

- Spec coverage: CLI flag (Task 4), gallery without badges (Task 5 buildCard has no badge markup), results panel top placement (Task 4 step 3.5 correction), per-hash results with copies badge (Task 5 resultCard), embedded stat (Task 4 step 3.6), stderr hint (Task 4 step 3.7), no-embeddings graceful path (Task 4 test 3 + similarBtn guard), non-finite skip (findSimilar isFinite), f16 byte-order pin (Task 3 test with 0x3C00), regression without --all (Task 4 test 1, Task 5 test 2). Text search, ANN, server mode: out of scope per spec.
- Type consistency: `VectorBlock { hashes, b64, dim }` used identically in Tasks 3 and 4; `file_to_json` signature unchanged (hash read from `FileRow` field added in Task 2); `generate_html` new signature used consistently in Task 4 step 3.7.
- Known accepted deviation: adding `"hash"` to per-file JSON changes report bytes even without `--all` (structure and behavior unchanged); noted in Task 2 step 4.
