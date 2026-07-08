# Image Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Local semantic image search: `dupe-embed` writes SigLIP embeddings for every unique image hash into SQLite; `dupe-search` answers text and image queries by brute-force vector scan.

**Architecture:** Cargo workspace with three crates. `dupe-core` holds the embeddings schema and vector serialization shared by tools. `dupe` keeps the existing binaries untouched. `dupe-ml` is a lib + two binaries wrapping candle (Metal on macOS) running SigLIP so400m/14-384. Embeddings are keyed by BLAKE3 content hash, L2-normalized, stored as f16 little-endian BLOBs. Search is an exact dot-product scan with top-k.

**Tech Stack:** Rust, candle-core/candle-nn/candle-transformers (SigLIP), tokenizers, hf-hub, half, rusqlite (bundled), rayon, image, clap.

**Spec:** `docs/superpowers/specs/2026-07-08-image-search-design.md`

---

## Reference: final workspace layout

```
dupe/
  Cargo.toml                      # workspace root (no package)
  crates/
    dupe/                         # existing crate, moved verbatim
      Cargo.toml
      src/{main.rs,scanner.rs,hasher.rs,output.rs,sqlite_output.rs,types.rs,bin/}
      tests/integration.rs
    dupe-core/
      Cargo.toml
      src/lib.rs                  # re-exports
      src/vectors.rs              # l2_normalize, f16 BLOB round-trip
      src/embeddings.rs           # schema, pending query, insert, load
    dupe-ml/
      Cargo.toml
      src/lib.rs                  # module declarations
      src/device.rs               # Metal-or-CPU device selection
      src/model.rs                # weights download, SigLIP load, embed_images/embed_text
      src/preprocess.rs           # decode + resize + normalize to tensor, HEIC via sips
      src/search.rs               # score + top_k (pure, unit-tested)
      src/bin/dupe-embed.rs
      src/bin/dupe-search.rs
```

Notes for the implementer:
- The em dash character is banned in this codebase (docs, code, comments, commits). Use `-` or `:`.
- No `Co-Authored-By` trailer on commits.
- All commits push with: `git push git@github.com:erhangundogan/dupe.git main`  (push once at the end of each task, not each step).
- candle API details below follow the official candle SigLIP example (`candle-examples/examples/siglip`). If a symbol does not exist under your candle version, open that example and match it; the shapes of the calls are stable, names occasionally move.
- Deviation from the spec: model weights live in the standard hf-hub cache (`~/.cache/huggingface/`) rather than `~/.cache/dupe/`. The standard location costs zero code and plays well with other tools; the spec's path is superseded.

---

### Task 1: Convert to Cargo workspace

Existing behavior must not change. Pure file moves plus manifest surgery.

**Files:**
- Create: `Cargo.toml` (new workspace root)
- Move: `Cargo.toml` -> `crates/dupe/Cargo.toml`, `src/` -> `crates/dupe/src/`, `tests/` -> `crates/dupe/tests/`

- [x] **Step 1: Move the crate**

```bash
mkdir -p crates/dupe
git mv Cargo.toml crates/dupe/Cargo.toml
git mv src crates/dupe/src
git mv tests crates/dupe/tests
```

- [x] **Step 2: Write the workspace root manifest**

Create `Cargo.toml` at repo root:

```toml
[workspace]
resolver = "2"
members = ["crates/dupe"]
```

- [x] **Step 3: Fix path-relative assumptions**

Check `crates/dupe/tests/integration.rs`. If it locates binaries via `env!("CARGO_BIN_EXE_dupe")` (or `_dupe-report`, `_dupe-fix-dates`) nothing changes. If it hardcodes `target/release/...` or `target/debug/...`, replace each with the `env!("CARGO_BIN_EXE_<name>")` macro, which cargo resolves correctly inside workspaces. Test fixture paths are relative to the crate directory and keep working after the move.

- [x] **Step 4: Verify everything still builds and passes**

Run: `cargo test 2>&1 | tail -20`
Expected: same pass count as before the move, zero failures.

Run: `cargo build --release && ls target/release/dupe target/release/dupe-report target/release/dupe-fix-dates`
Expected: all three binaries exist (workspace target dir stays at repo root).

- [x] **Step 5: Commit and push**

```bash
git add -A
git commit -m "refactor: convert to Cargo workspace, move crate to crates/dupe"
git push git@github.com:erhangundogan/dupe.git main
```

---

### Task 2: dupe-core crate: vector serialization

**Files:**
- Create: `crates/dupe-core/Cargo.toml`
- Create: `crates/dupe-core/src/lib.rs`
- Create: `crates/dupe-core/src/vectors.rs`
- Modify: `Cargo.toml` (root: add member)

- [x] **Step 1: Create the crate**

`crates/dupe-core/Cargo.toml`:

```toml
[package]
name = "dupe-core"
version = "0.1.0"
edition = "2021"

[dependencies]
half = "2"
rusqlite = { version = "0.32", features = ["bundled"] }
```

`crates/dupe-core/src/lib.rs`:

```rust
pub mod vectors;
```

Root `Cargo.toml` members becomes:

```toml
members = ["crates/dupe", "crates/dupe-core"]
```

- [x] **Step 2: Write failing tests for vector round-trip and normalization**

`crates/dupe-core/src/vectors.rs`:

```rust
//! f32 <-> f16 BLOB conversion and L2 normalization for stored embeddings.
//! Storage format: little-endian f16, 2 bytes per dimension.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_round_trip_preserves_values_within_tolerance() {
        let v = vec![0.1f32, -0.5, 0.999, 0.0];
        let bytes = to_f16_bytes(&v);
        assert_eq!(bytes.len(), 8);
        let back = from_f16_bytes(&bytes);
        for (a, b) in v.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn l2_normalize_produces_unit_vector() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6);
        assert!((v[0] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_vector_stays_zero() {
        let mut v = vec![0.0f32; 4];
        l2_normalize(&mut v);
        assert!(v.iter().all(|x| *x == 0.0));
    }
}
```

- [x] **Step 3: Run tests to verify they fail**

Run: `cargo test -p dupe-core 2>&1 | tail -5`
Expected: compile error, `to_f16_bytes` not found.

- [x] **Step 4: Implement**

Prepend to `crates/dupe-core/src/vectors.rs` (above the tests module):

```rust
use half::f16;

pub fn to_f16_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 2);
    for &x in v {
        out.extend_from_slice(&f16::from_f32(x).to_le_bytes());
    }
    out
}

pub fn from_f16_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dupe-core 2>&1 | tail -5`
Expected: `3 passed`.

- [x] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: add dupe-core crate with f16 vector serialization"
```

---

### Task 3: dupe-core: embeddings table schema and queries

**Files:**
- Create: `crates/dupe-core/src/embeddings.rs`
- Modify: `crates/dupe-core/src/lib.rs`

- [x] **Step 1: Write failing tests**

`crates/dupe-core/src/embeddings.rs` (tests first; the `file_hashes` DDL below mirrors `crates/dupe/src/sqlite_output.rs` exactly):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE file_hashes (
                path        TEXT PRIMARY KEY,
                hash        TEXT NOT NULL,
                size_bytes  INTEGER,
                created_at  TEXT,
                modified_at TEXT,
                ext         TEXT,
                phash       INTEGER,
                exif_date   TEXT,
                gps_lat     REAL,
                gps_lon     REAL,
                width       INTEGER,
                height      INTEGER
            );",
        )
        .unwrap();
        ensure_embeddings_table(&conn).unwrap();
        conn
    }

    fn insert_file(conn: &Connection, path: &str, hash: &str, ext: &str) {
        conn.execute(
            "INSERT INTO file_hashes (path, hash, ext) VALUES (?1, ?2, ?3)",
            rusqlite::params![path, hash, ext],
        )
        .unwrap();
    }

    #[test]
    fn pending_images_dedupes_by_hash_and_filters_ext() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/b/1-copy.jpg", "h1", "jpg"); // same hash, second path
        insert_file(&conn, "/a/2.png", "h2", "png");
        insert_file(&conn, "/a/clip.mp4", "h3", "mp4");   // unsupported for embedding

        let pending = pending_images(&conn).unwrap();
        assert_eq!(pending.len(), 2); // h1 once, h2 once, h3 excluded
        assert!(pending.iter().any(|p| p.hash == "h1"));
        assert!(pending.iter().any(|p| p.hash == "h2"));
    }

    #[test]
    fn pending_images_excludes_already_embedded() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/a/2.jpg", "h2", "jpg");
        insert_embeddings(&conn, "test-model", &[("h1".to_string(), vec![0u8; 4])]).unwrap();

        let pending = pending_images(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].hash, "h2");
    }

    #[test]
    fn insert_and_load_round_trip() {
        let conn = test_db();
        insert_embeddings(
            &conn,
            "test-model",
            &[("h1".to_string(), vec![1u8, 2, 3, 4]), ("h2".to_string(), vec![5u8, 6])],
        )
        .unwrap();

        let rows = load_embeddings(&conn, "test-model").unwrap();
        assert_eq!(rows.len(), 2);
        let h1 = rows.iter().find(|(h, _)| h == "h1").unwrap();
        assert_eq!(h1.1, vec![1u8, 2, 3, 4]);

        // different model_id loads nothing
        assert!(load_embeddings(&conn, "other").unwrap().is_empty());
    }

    #[test]
    fn paths_for_hash_returns_all_duplicates() {
        let conn = test_db();
        insert_file(&conn, "/a/1.jpg", "h1", "jpg");
        insert_file(&conn, "/b/1-copy.jpg", "h1", "jpg");
        let paths = paths_for_hash(&conn, "h1").unwrap();
        assert_eq!(paths.len(), 2);
    }
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dupe-core 2>&1 | tail -5`
Expected: compile error, `ensure_embeddings_table` not found.

- [x] **Step 3: Implement**

Prepend to `crates/dupe-core/src/embeddings.rs`:

```rust
//! Embeddings table: one row per unique content hash, keyed to file_hashes.hash.

use rusqlite::{Connection, Result, params};

/// Extensions the embedding pipeline can decode. Video is out of scope in v1.
pub const EMBEDDABLE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "heic", "dng",
];

pub struct PendingImage {
    pub hash: String,
    pub path: String,
}

pub fn ensure_embeddings_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS embeddings (
            hash        TEXT PRIMARY KEY,
            model_id    TEXT NOT NULL,
            embedding   BLOB NOT NULL,
            embedded_at TEXT NOT NULL
        );",
    )
}

/// Unique hashes that are embeddable but not yet embedded; one representative
/// path per hash (MIN(path) keeps it deterministic).
pub fn pending_images(conn: &Connection) -> Result<Vec<PendingImage>> {
    let placeholders = EMBEDDABLE_EXTS
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT hash, MIN(path) FROM file_hashes
         WHERE lower(ext) IN ({placeholders})
           AND hash NOT IN (SELECT hash FROM embeddings)
         GROUP BY hash
         ORDER BY hash"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(EMBEDDABLE_EXTS.iter()),
        |row| {
            Ok(PendingImage {
                hash: row.get(0)?,
                path: row.get(1)?,
            })
        },
    )?;
    rows.collect()
}

/// Upsert a batch of (hash, f16 blob) rows inside one transaction.
pub fn insert_embeddings(
    conn: &Connection,
    model_id: &str,
    items: &[(String, Vec<u8>)],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO embeddings (hash, model_id, embedding, embedded_at)
             VALUES (?1, ?2, ?3, datetime('now'))",
        )?;
        for (hash, blob) in items {
            stmt.execute(params![hash, model_id, blob])?;
        }
    }
    tx.commit()
}

pub fn load_embeddings(conn: &Connection, model_id: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut stmt =
        conn.prepare("SELECT hash, embedding FROM embeddings WHERE model_id = ?1")?;
    let rows = stmt.query_map(params![model_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

pub fn paths_for_hash(conn: &Connection, hash: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare("SELECT path FROM file_hashes WHERE hash = ?1 ORDER BY path")?;
    let rows = stmt.query_map(params![hash], |row| row.get(0))?;
    rows.collect()
}
```

Update `crates/dupe-core/src/lib.rs`:

```rust
pub mod embeddings;
pub mod vectors;
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p dupe-core 2>&1 | tail -5`
Expected: `7 passed` (3 from vectors, 4 from embeddings).

- [x] **Step 5: Commit and push**

```bash
git add -A
git commit -m "feat: embeddings schema, pending-hash query, and blob round-trip in dupe-core"
git push git@github.com:erhangundogan/dupe.git main
```

---

### Task 4: dupe-ml crate skeleton, device selection, search math

Pure-logic parts first (unit-testable without model weights).

**Files:**
- Create: `crates/dupe-ml/Cargo.toml`
- Create: `crates/dupe-ml/src/lib.rs`
- Create: `crates/dupe-ml/src/device.rs`
- Create: `crates/dupe-ml/src/search.rs`
- Modify: `Cargo.toml` (root: add member)

- [x] **Step 1: Create the crate**

`crates/dupe-ml/Cargo.toml` (check crates.io for the current candle version and use it; 0.9 is the floor):

```toml
[package]
name = "dupe-ml"
version = "0.1.0"
edition = "2021"

[dependencies]
dupe-core = { path = "../dupe-core" }
clap = { version = "4", features = ["derive"] }
rusqlite = { version = "0.32", features = ["bundled"] }
rayon = "1"
image = { version = "0.25", default-features = false, features = ["jpeg", "png", "gif", "webp", "bmp", "tiff"] }
half = "2"
anyhow = "1"
candle-core = "0.9"
candle-nn = "0.9"
candle-transformers = "0.9"
tokenizers = "0.20"
hf-hub = "0.3"

[target.'cfg(target_os = "macos")'.dependencies]
candle-core = { version = "0.9", features = ["metal"] }
candle-nn = { version = "0.9", features = ["metal"] }
candle-transformers = { version = "0.9", features = ["metal"] }

[[bin]]
name = "dupe-embed"
path = "src/bin/dupe-embed.rs"

[[bin]]
name = "dupe-search"
path = "src/bin/dupe-search.rs"
```

`crates/dupe-ml/src/lib.rs`:

```rust
pub mod device;
pub mod search;
```

Root `Cargo.toml` members becomes:

```toml
members = ["crates/dupe", "crates/dupe-core", "crates/dupe-ml"]
```

`crates/dupe-ml/src/device.rs`:

```rust
use candle_core::Device;

/// Metal on macOS when available, CPU otherwise. Never fails.
pub fn best_device() -> Device {
    #[cfg(target_os = "macos")]
    {
        if let Ok(d) = Device::new_metal(0) {
            return d;
        }
    }
    Device::Cpu
}
```

- [x] **Step 2: Write failing tests for top-k scoring**

`crates/dupe-ml/src/search.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_k_orders_by_score_descending() {
        let corpus = vec![
            ("a".to_string(), vec![1.0f32, 0.0]),
            ("b".to_string(), vec![0.0f32, 1.0]),
            ("c".to_string(), vec![0.7f32, 0.7]),
        ];
        let query = vec![1.0f32, 0.0];
        let hits = top_k(&query, &corpus, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, "a");
        assert!((hits[0].1 - 1.0).abs() < 1e-6);
        assert_eq!(hits[1].0, "c");
    }

    #[test]
    fn top_k_handles_k_larger_than_corpus() {
        let corpus = vec![("a".to_string(), vec![1.0f32])];
        let hits = top_k(&[1.0], &corpus, 10);
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn top_k_skips_dimension_mismatch() {
        let corpus = vec![
            ("bad".to_string(), vec![1.0f32]),          // wrong dims
            ("good".to_string(), vec![1.0f32, 0.0]),
        ];
        let hits = top_k(&[1.0, 0.0], &corpus, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "good");
    }
}
```

- [x] **Step 3: Run tests to verify they fail**

Run: `cargo test -p dupe-ml 2>&1 | tail -5`
Expected: compile error, `top_k` not found. (First build compiles candle; several minutes is normal.)

- [x] **Step 4: Implement top_k**

Prepend to `crates/dupe-ml/src/search.rs`:

```rust
//! Brute-force scoring. Inputs are L2-normalized so dot product = cosine.

pub fn top_k(query: &[f32], corpus: &[(String, Vec<f32>)], k: usize) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = corpus
        .iter()
        .filter(|(_, v)| v.len() == query.len())
        .map(|(hash, v)| {
            let dot: f32 = query.iter().zip(v.iter()).map(|(a, b)| a * b).sum();
            (hash.clone(), dot)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    scored
}
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dupe-ml 2>&1 | tail -5`
Expected: `3 passed`.

- [x] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: dupe-ml crate skeleton with device selection and top-k search"
```

---

### Task 5: Preprocessing: image file to model tensor

**Files:**
- Create: `crates/dupe-ml/src/preprocess.rs`
- Modify: `crates/dupe-ml/src/lib.rs`
- Create: `crates/dupe-ml/tests/fixtures/red_2x2.png` (generated in Step 1)

- [x] **Step 1: Create a tiny fixture image**

```bash
mkdir -p crates/dupe-ml/tests/fixtures
python3 -c "
from struct import pack
import zlib
def chunk(t, d):
    c = t + d
    return pack('>I', len(d)) + c + pack('>I', zlib.crc32(c))
ihdr = pack('>IIBBBBB', 2, 2, 8, 2, 0, 0, 0)
raw = b'\x00' + b'\xff\x00\x00' * 2 + b'\x00' + b'\xff\x00\x00' * 2
png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')
open('crates/dupe-ml/tests/fixtures/red_2x2.png', 'wb').write(png)
"
```

Verify: `file crates/dupe-ml/tests/fixtures/red_2x2.png` prints `PNG image data, 2 x 2`.

- [x] **Step 2: Write failing tests**

`crates/dupe-ml/src/preprocess.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::Device;

    #[test]
    fn preprocess_produces_correct_shape_and_range() {
        let t = image_to_tensor(
            std::path::Path::new("tests/fixtures/red_2x2.png"),
            384,
            &Device::Cpu,
        )
        .unwrap();
        assert_eq!(t.dims(), &[3, 384, 384]);
        // SigLIP normalization maps [0,1] to [-1,1]; red pixel -> R channel ~ 1.0
        let flat: Vec<f32> = t.flatten_all().unwrap().to_vec1().unwrap();
        assert!(flat.iter().all(|v| *v >= -1.001 && *v <= 1.001));
        assert!((flat[0] - 1.0).abs() < 0.02); // first value is R channel of red image
    }

    #[test]
    fn preprocess_missing_file_is_err_not_panic() {
        let r = image_to_tensor(std::path::Path::new("/nonexistent.jpg"), 384, &Device::Cpu);
        assert!(r.is_err());
    }
}
```

- [x] **Step 3: Run tests to verify they fail**

Run: `cargo test -p dupe-ml 2>&1 | tail -5`
Expected: compile error, `image_to_tensor` not found.

- [x] **Step 4: Implement**

Prepend to `crates/dupe-ml/src/preprocess.rs`:

```rust
//! Decode an image file into a SigLIP input tensor: resize to NxN,
//! scale to [0,1], normalize with mean 0.5 / std 0.5 per channel -> [-1,1].
//! HEIC is converted to a temp JPEG via sips (macOS), matching dupe-report.

use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use std::path::Path;

pub fn image_to_tensor(path: &Path, size: usize, device: &Device) -> Result<Tensor> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    let img = if ext == "heic" {
        decode_heic(path, size)?
    } else {
        image::open(path).with_context(|| format!("decode {}", path.display()))?
    };

    let img = img
        .resize_exact(size as u32, size as u32, image::imageops::FilterType::Triangle)
        .to_rgb8();

    let data: Vec<f32> = img.into_raw().iter().map(|&b| b as f32 / 255.0).collect();
    // HWC -> CHW, then (x - 0.5) / 0.5
    let t = Tensor::from_vec(data, (size, size, 3), device)?
        .permute((2, 0, 1))?
        .to_dtype(DType::F32)?;
    let t = ((t - 0.5)? / 0.5)?;
    Ok(t)
}

fn decode_heic(path: &Path, size: usize) -> Result<image::DynamicImage> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let tmp = std::env::temp_dir().join(format!("dupe_embed_{:016x}.jpg", h.finish()));
    let status = std::process::Command::new("sips")
        .args(["-s", "format", "jpeg", "--resampleHeightWidthMax"])
        .arg((size * 2).to_string())
        .arg(path)
        .args(["--out".as_ref() as &std::ffi::OsStr, tmp.as_os_str()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("run sips (HEIC decode requires macOS)")?;
    anyhow::ensure!(status.success(), "sips failed for {}", path.display());
    let img = image::open(&tmp).with_context(|| format!("decode sips output for {}", path.display()));
    let _ = std::fs::remove_file(&tmp);
    img
}
```

Update `crates/dupe-ml/src/lib.rs`:

```rust
pub mod device;
pub mod preprocess;
pub mod search;
```

- [x] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dupe-ml 2>&1 | tail -5`
Expected: `5 passed` (3 search + 2 preprocess).

- [x] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: image preprocessing to SigLIP input tensor with HEIC support"
```

---

### Task 6: Model wrapper: load SigLIP, embed images and text

No cheap unit test exists for this (weights are ~1.5GB). Tests are feature-gated behind `real-model` and excluded from default runs. Before implementing, open the candle SigLIP example for your installed version (`https://github.com/huggingface/candle/tree/main/candle-examples/examples/siglip`) and reconcile names; the code below follows it.

**Files:**
- Create: `crates/dupe-ml/src/model.rs`
- Modify: `crates/dupe-ml/src/lib.rs`
- Modify: `crates/dupe-ml/Cargo.toml` (add feature)

- [x] **Step 1: Add the feature gate**

Append to `crates/dupe-ml/Cargo.toml`:

```toml
[features]
real-model = []
```

- [x] **Step 2: Implement the model wrapper**

`crates/dupe-ml/src/model.rs`:

```rust
//! SigLIP so400m/14-384 via candle. Weights auto-download from Hugging Face
//! to the hf-hub cache on first use. Embeddings are L2-normalized f32.

use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::siglip;
use std::path::Path;
use tokenizers::Tokenizer;

pub const MODEL_ID: &str = "siglip-so400m-384";
const HF_REPO: &str = "google/siglip-so400m-patch14-384";
pub const IMAGE_SIZE: usize = 384;

pub struct Embedder {
    model: siglip::Model,
    tokenizer: Tokenizer,
    config: siglip::Config,
    device: Device,
}

impl Embedder {
    /// Downloads weights on first call (prints a note to stderr), then loads.
    pub fn load(device: Device) -> Result<Self> {
        let api = hf_hub::api::sync::Api::new().context("init hf-hub api")?;
        let repo = api.model(HF_REPO.to_string());
        eprintln!("Loading model {HF_REPO} (downloads to hf-hub cache on first run)...");
        let weights = repo.get("model.safetensors").context(
            "download model weights; check network or pre-populate the hf-hub cache",
        )?;
        let tokenizer_file = repo.get("tokenizer.json").context("download tokenizer")?;
        let config_file = repo.get("config.json").context("download config")?;

        let config: siglip::Config =
            serde_json::from_str(&std::fs::read_to_string(&config_file)?)
                .context("parse siglip config.json")?;
        let tokenizer = Tokenizer::from_file(&tokenizer_file)
            .map_err(|e| anyhow::anyhow!("load tokenizer: {e}"))?;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device)?
        };
        let model = siglip::Model::new(&config, vb).context("build siglip model")?;
        Ok(Self { model, tokenizer, config, device })
    }

    /// Embed a batch of preprocessed image tensors (each [3, 384, 384]).
    /// Returns one L2-normalized vector per input.
    pub fn embed_images(&self, images: &[Tensor]) -> Result<Vec<Vec<f32>>> {
        let batch = Tensor::stack(images, 0)?;
        let features = self.model.get_image_features(&batch)?;
        tensor_to_normalized_rows(&features)
    }

    /// Embed a text query. Returns one L2-normalized vector.
    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        // SigLIP expects lowercase input padded to the text tower's max length.
        let max_len = self.config.text_config.max_position_embeddings;
        let pad_id = self
            .tokenizer
            .token_to_id("</s>")
            .unwrap_or(1);
        let enc = self
            .tokenizer
            .encode(text.to_lowercase(), true)
            .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;
        let mut ids: Vec<u32> = enc.get_ids().to_vec();
        ids.truncate(max_len);
        while ids.len() < max_len {
            ids.push(pad_id);
        }
        let input = Tensor::new(ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let features = self.model.get_text_features(&input)?;
        let rows = tensor_to_normalized_rows(&features)?;
        Ok(rows.into_iter().next().unwrap())
    }
}

fn tensor_to_normalized_rows(t: &Tensor) -> Result<Vec<Vec<f32>>> {
    let rows: Vec<Vec<f32>> = t.to_dtype(DType::F32)?.to_vec2()?;
    Ok(rows
        .into_iter()
        .map(|mut v| {
            dupe_core::vectors::l2_normalize(&mut v);
            v
        })
        .collect())
}

/// Convenience: preprocess a file and embed it (used by dupe-search --image).
pub fn embed_image_file(embedder: &Embedder, path: &Path) -> Result<Vec<f32>> {
    let t = crate::preprocess::image_to_tensor(path, IMAGE_SIZE, &embedder.device)?;
    let rows = embedder.embed_images(&[t])?;
    Ok(rows.into_iter().next().unwrap())
}

#[cfg(all(test, feature = "real-model"))]
mod tests {
    use super::*;

    #[test]
    fn text_and_image_towers_agree_on_semantics() {
        let e = Embedder::load(crate::device::best_device()).unwrap();
        let red = embed_image_file(&e, std::path::Path::new("tests/fixtures/red_2x2.png")).unwrap();
        let q_red = e.embed_text("a solid red square").unwrap();
        let q_dog = e.embed_text("a photo of a dog").unwrap();
        let dot = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        assert!(dot(&red, &q_red) > dot(&red, &q_dog));
        assert_eq!(red.len(), q_red.len());
    }
}
```

Note for the implementer: `serde_json` must be added to `crates/dupe-ml/Cargo.toml` dependencies (`serde_json = "1"`). If `siglip::Config` in your candle version does not implement `Deserialize`, use the closest provided constructor (for so400m it is `siglip::Config::so400m_patch14_384()` in recent versions) and delete the config.json download. If `text_config.max_position_embeddings` is not public, hardcode `64` with a comment naming the config value it mirrors.

Update `crates/dupe-ml/src/lib.rs`:

```rust
pub mod device;
pub mod model;
pub mod preprocess;
pub mod search;
```

- [x] **Step 3: Verify it compiles without the feature**

Run: `cargo test -p dupe-ml 2>&1 | tail -5`
Expected: `5 passed` (real-model test not compiled).

- [x] **Step 4: Run the real-model test once locally**

Run: `cargo test -p dupe-ml --features real-model --release 2>&1 | tail -5`
Expected: downloads weights on first run (several minutes), then `6 passed`. If the tokenizer or config file names differ in the HF repo, adjust to what `huggingface.co/google/siglip-so400m-patch14-384/tree/main` actually lists and re-run.

- [x] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: SigLIP model wrapper with image and text embedding"
```

---

### Task 7: dupe-embed binary

**Files:**
- Create: `crates/dupe-ml/src/bin/dupe-embed.rs`

- [x] **Step 1: Implement the binary**

`crates/dupe-ml/src/bin/dupe-embed.rs`:

```rust
use anyhow::{Context, Result};
use clap::Parser;
use dupe_core::{embeddings, vectors};
use dupe_ml::{device, model, preprocess};
use rayon::prelude::*;
use rusqlite::Connection;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dupe-embed", about = "Generate SigLIP embeddings for images in a dupe SQLite database")]
struct Args {
    /// SQLite database produced by: dupe --output-sqlite <db>
    db: PathBuf,

    /// Inference batch size
    #[arg(long, default_value_t = 32)]
    batch: usize,

    /// Rows written per transaction (resume granularity)
    #[arg(long, default_value_t = 500)]
    chunk: usize,

    /// Suppress progress output on stderr (errors always shown)
    #[arg(long)]
    silent: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let conn = Connection::open(&args.db)
        .with_context(|| format!("open {}", args.db.display()))?;
    embeddings::ensure_embeddings_table(&conn)?;

    let pending = embeddings::pending_images(&conn)?;
    if pending.is_empty() {
        if !args.silent {
            eprintln!("Nothing to embed: all hashes already have embeddings.");
        }
        return Ok(());
    }
    if !args.silent {
        eprintln!("{} image(s) to embed", pending.len());
    }

    let dev = device::best_device();
    let embedder = model::Embedder::load(dev.clone())?;

    let mut done = 0usize;
    let mut failed = 0usize;
    for chunk in pending.chunks(args.chunk) {
        // Decode in parallel; None = unreadable, logged and skipped.
        let decoded: Vec<Option<(String, candle_core::Tensor)>> = chunk
            .par_iter()
            .map(|p| {
                match preprocess::image_to_tensor(
                    std::path::Path::new(&p.path),
                    model::IMAGE_SIZE,
                    &candle_core::Device::Cpu, // decode on CPU, move in batch
                ) {
                    Ok(t) => Some((p.hash.clone(), t)),
                    Err(e) => {
                        eprintln!("skip {}: {e:#}", p.path);
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

        embeddings::insert_embeddings(&conn, model::MODEL_ID, &rows)?;
        done += rows.len();
        if !args.silent {
            eprintln!("embedded {done}/{} ({failed} skipped)", pending.len());
        }
    }

    if !args.silent {
        eprintln!("Done: {done} embedded, {failed} skipped.");
    }
    Ok(())
}
```

- [x] **Step 2: Verify it builds and help text works**

Run: `cargo build -p dupe-ml --release && ./target/release/dupe-embed --help`
Expected: usage text with `--batch`, `--chunk`, `--silent`.

- [x] **Step 3: End-to-end smoke test against a real tiny DB**

```bash
mkdir -p /tmp/dupe-embed-smoke && cp crates/dupe-ml/tests/fixtures/red_2x2.png /tmp/dupe-embed-smoke/
./target/release/dupe --output-sqlite /tmp/dupe-embed-smoke.db /tmp/dupe-embed-smoke
./target/release/dupe-embed /tmp/dupe-embed-smoke.db
sqlite3 /tmp/dupe-embed-smoke.db "SELECT hash, model_id, length(embedding) FROM embeddings;"
```

Expected: one row, model_id `siglip-so400m-384`, blob length = 2 x embedding dims (so400m emits 1152 dims -> 2304 bytes).

Rerun `./target/release/dupe-embed /tmp/dupe-embed-smoke.db` and expect: `Nothing to embed`. That proves resumability.

- [x] **Step 4: Commit**

```bash
git add -A
git commit -m "feat: dupe-embed binary with chunked, resumable embedding pipeline"
```

---

### Task 8: dupe-search binary

**Files:**
- Create: `crates/dupe-ml/src/bin/dupe-search.rs`

- [x] **Step 1: Implement the binary**

`crates/dupe-ml/src/bin/dupe-search.rs`:

```rust
use anyhow::{Context, Result};
use clap::Parser;
use dupe_core::{embeddings, vectors};
use dupe_ml::{device, model, search};
use rusqlite::Connection;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dupe-search", about = "Semantic image search over a dupe SQLite database")]
struct Args {
    /// SQLite database with embeddings (run dupe-embed first)
    db: PathBuf,

    /// Text query, e.g. "sunset on beach" (omit when using --image)
    query: Option<String>,

    /// Search by example image instead of text
    #[arg(long, conflicts_with = "query")]
    image: Option<PathBuf>,

    /// Number of results
    #[arg(short = 'k', long, default_value_t = 20)]
    top_k: usize,

    /// Prepend the cosine score to each output line
    #[arg(long)]
    scores: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let conn = Connection::open(&args.db)
        .with_context(|| format!("open {}", args.db.display()))?;

    let corpus_raw = embeddings::load_embeddings(&conn, model::MODEL_ID)?;
    anyhow::ensure!(
        !corpus_raw.is_empty(),
        "no embeddings found in {} for model {}; run dupe-embed first",
        args.db.display(),
        model::MODEL_ID
    );
    let corpus: Vec<(String, Vec<f32>)> = corpus_raw
        .into_iter()
        .map(|(hash, blob)| (hash, vectors::from_f16_bytes(&blob)))
        .collect();

    let embedder = model::Embedder::load(device::best_device())?;
    let query_vec = match (&args.query, &args.image) {
        (Some(text), None) => embedder.embed_text(text)?,
        (None, Some(img)) => model::embed_image_file(&embedder, img)?,
        _ => anyhow::bail!("provide either a text query or --image <path>"),
    };

    let hits = search::top_k(&query_vec, &corpus, args.top_k);
    for (hash, score) in hits {
        for path in embeddings::paths_for_hash(&conn, &hash)? {
            if args.scores {
                println!("{score:.4}\t{path}");
            } else {
                println!("{path}");
            }
        }
    }
    Ok(())
}
```

- [x] **Step 2: Verify build and CLI contract**

Run: `cargo build -p dupe-ml --release && ./target/release/dupe-search --help`
Expected: usage showing positional query, `--image`, `-k`, `--scores`.

Run: `./target/release/dupe-search /tmp/nonexistent.db "cat" 2>&1; echo "exit=$?"`
Expected: clear error, nonzero exit, no panic backtrace.

- [x] **Step 3: End-to-end smoke test (both modes)**

Using the database from Task 7 Step 3:

```bash
./target/release/dupe-search /tmp/dupe-embed-smoke.db "a solid red square" --scores
./target/release/dupe-search /tmp/dupe-embed-smoke.db --image crates/dupe-ml/tests/fixtures/red_2x2.png --scores
```

Expected: both print the fixture path; image mode scores near 1.0 (self-similarity).

- [x] **Step 4: Commit and push**

```bash
git add -A
git commit -m "feat: dupe-search binary with text and image query modes"
git push git@github.com:erhangundogan/dupe.git main
```

---

### Task 9: Docs and final verification

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`

- [x] **Step 1: Update CLAUDE.md**

In the Build & run section add:

```bash
./target/release/dupe-embed ~/photos.db                         # embed all images (resumable)
./target/release/dupe-search ~/photos.db "sunset on beach"      # text search
./target/release/dupe-search ~/photos.db --image query.jpg      # find similar images
```

Replace the Project structure block with the workspace layout (crates/dupe, crates/dupe-core, crates/dupe-ml as in this plan's reference layout). Add to Key crates: `candle` (SigLIP inference, Metal on macOS), `tokenizers`, `hf-hub`, `half`. Add a new section:

```markdown
## dupe-embed / dupe-search

`dupe-embed <db>` embeds every unique image hash (SigLIP so400m/14-384, 1152-dim,
L2-normalized f16 BLOB) into an `embeddings` table keyed by content hash. Resumable:
re-running processes only missing hashes. `--batch` (default 32), `--chunk` (rows per
transaction, default 500), `--silent`. HEIC via sips; mov/mp4 skipped.

`dupe-search <db> "query"` or `dupe-search <db> --image photo.jpg` prints matching
paths to stdout (all duplicate paths per matched hash). `-k` top-k (default 20),
`--scores` prepends cosine score. Brute-force exact scan; no ANN index at this scale.

Model weights auto-download from Hugging Face (google/siglip-so400m-patch14-384) on
first run. Embeddings schema:

    CREATE TABLE embeddings (
        hash        TEXT PRIMARY KEY,
        model_id    TEXT NOT NULL,
        embedding   BLOB NOT NULL,
        embedded_at TEXT NOT NULL
    );
```

- [x] **Step 2: Update README.md**

Mirror the same content in README style: add the two binaries to the Installation note, a "Semantic search" section with the usage examples above, and the embeddings table DDL next to the existing schema block. Mention the recommended workflow gains a step: `dupe-embed` after scanning, `dupe-search` anytime after.

- [x] **Step 3: Full workspace verification**

Run: `cargo test 2>&1 | tail -10`
Expected: all crates pass (dupe integration tests, dupe-core 7, dupe-ml 5).

Run: `cargo build --release 2>&1 | tail -3 && ls target/release/{dupe,dupe-report,dupe-fix-dates,dupe-embed,dupe-search}`
Expected: five binaries.

- [x] **Step 4: Commit and push**

```bash
git add -A
git commit -m "docs: document dupe-embed and dupe-search, workspace layout"
git push git@github.com:erhangundogan/dupe.git main
```
