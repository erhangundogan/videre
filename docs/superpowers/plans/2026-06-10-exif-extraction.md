# EXIF Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--exif` CLI flag that extracts `DateTimeOriginal`, GPS coordinates, and image dimensions from JPEG/HEIC/TIFF files during the hashing pass, appending them to the JSONL output.

**Architecture:** EXIF extraction runs inside `hash_file` after BLAKE3 finishes — the file is in the OS page cache so re-opening is nearly free. A new `extract_exif(path)` function in `hasher.rs` returns an `ExifData` struct. Five new optional fields are added to `FileRecord`. The `--exif` flag threads through `main.rs → hash_file(path, exif: bool)`.

**Tech Stack:** `kamadak-exif = "0.5"` (pure Rust, JPEG/TIFF/HEIC support). Python + Pillow + piexif for one-time test fixture creation.

---

## File Map

| File | Change |
|------|--------|
| `Cargo.toml` | Add `kamadak-exif = "0.5"` to `[dependencies]` |
| `src/types.rs` | Add 5 optional fields to `FileRecord` |
| `src/hasher.rs` | Add `ExifData`, `rational_to_f64`, `extract_exif`, `extract_gps`; update `hash_file` signature |
| `src/main.rs` | Add `--exif` to `Args`; pass `args.exif` to `hash_file` |
| `tests/fixtures/sample_with_exif.jpg` | New binary fixture (created via Python, committed) |
| `tests/integration.rs` | Add `--exif` integration test |

---

## Task 1: Add dependency and create test fixture

**Files:**
- Modify: `Cargo.toml`
- Create: `tests/fixtures/sample_with_exif.jpg`

- [ ] **Step 1: Add kamadak-exif to Cargo.toml**

In `Cargo.toml`, add to `[dependencies]`:
```toml
kamadak-exif = "0.5"
```

- [ ] **Step 2: Verify it fetches**

```bash
cargo check
```
Expected: compiles without errors. A new `exif` crate appears in the dependency tree.

- [ ] **Step 3: Create the test fixture**

This JPEG has known EXIF values that unit tests will assert against:
- `DateTimeOriginal`: `2023:08:15 14:30:00`
- GPS: 48°51'N, 2°21'E (Paris area)
- `PixelXDimension`: 100, `PixelYDimension`: 80

```bash
pip install Pillow piexif
python3 -c "
from PIL import Image
import piexif

exif_dict = {
    'Exif': {
        piexif.ExifIFD.DateTimeOriginal: b'2023:08:15 14:30:00',
        piexif.ExifIFD.PixelXDimension: 100,
        piexif.ExifIFD.PixelYDimension: 80,
    },
    'GPS': {
        piexif.GPSIFD.GPSLatitudeRef: b'N',
        piexif.GPSIFD.GPSLatitude: ((48, 1), (51, 1), (0, 1)),
        piexif.GPSIFD.GPSLongitudeRef: b'E',
        piexif.GPSIFD.GPSLongitude: ((2, 1), (21, 1), (0, 1)),
    },
}
exif_bytes = piexif.dump(exif_dict)
img = Image.new('RGB', (100, 80), color=(128, 64, 32))
img.save('tests/fixtures/sample_with_exif.jpg', exif=exif_bytes)
print('Created tests/fixtures/sample_with_exif.jpg')
"
```

Expected values after parsing:
- `exif_date`: `"2023-08-15T14:30:00"`
- `gps_lat`: `48.85` (48 + 51/60)
- `gps_lon`: `2.35` (2 + 21/60)
- `width`: `100`
- `height`: `80`

- [ ] **Step 4: Verify the fixture is readable**

```bash
ls -lh tests/fixtures/sample_with_exif.jpg
```
Expected: file exists, non-zero size.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock tests/fixtures/sample_with_exif.jpg
git commit -m "feat: add kamadak-exif dependency and EXIF test fixture"
```

---

## Task 2: Extend FileRecord with EXIF fields

**Files:**
- Modify: `src/types.rs`
- Modify: `src/output.rs` (update `make_record` test helper)

- [ ] **Step 1: Write the failing test**

In `src/types.rs`, inside the `#[cfg(test)] mod tests` block, add:

```rust
#[test]
fn file_record_exif_fields_serialize_when_present() {
    let record = FileRecord {
        path: "/photos/img.jpg".to_string(),
        hash: "abc123".to_string(),
        size_bytes: 1024,
        created_at: None,
        modified_at: None,
        ext: "jpg".to_string(),
        phash: None,
        exif_date: Some("2023-08-15T14:30:00".to_string()),
        gps_lat: Some(48.85),
        gps_lon: Some(2.35),
        width: Some(100),
        height: Some(80),
    };
    let json = serde_json::to_string(&record).unwrap();
    assert!(json.contains("\"exif_date\":\"2023-08-15T14:30:00\""));
    assert!(json.contains("\"gps_lat\":48.85"));
    assert!(json.contains("\"gps_lon\":2.35"));
    assert!(json.contains("\"width\":100"));
    assert!(json.contains("\"height\":80"));
}

#[test]
fn file_record_exif_fields_absent_when_none() {
    let record = FileRecord {
        path: "/a.jpg".to_string(),
        hash: "x".to_string(),
        size_bytes: 0,
        created_at: None,
        modified_at: None,
        ext: "jpg".to_string(),
        phash: None,
        exif_date: None,
        gps_lat: None,
        gps_lon: None,
        width: None,
        height: None,
    };
    let json = serde_json::to_string(&record).unwrap();
    assert!(!json.contains("exif_date"));
    assert!(!json.contains("gps_lat"));
    assert!(!json.contains("gps_lon"));
    assert!(!json.contains("width"));
    assert!(!json.contains("height"));
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p dupe types 2>&1 | head -20
```
Expected: compile error — `FileRecord` has no field `exif_date`.

- [ ] **Step 3: Add the five fields to FileRecord**

In `src/types.rs`, update `FileRecord` to:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub path: String,
    pub hash: String,
    pub size_bytes: u64,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub ext: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phash: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exif_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps_lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gps_lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}
```

- [ ] **Step 4: Update make_record in output.rs tests**

In `src/output.rs`, find the `make_record` helper in `#[cfg(test)] mod tests` and update it:

```rust
fn make_record(path: &str, hash: &str) -> FileRecord {
    FileRecord {
        path: path.to_string(),
        hash: hash.to_string(),
        size_bytes: 100,
        created_at: None,
        modified_at: Some("2023-01-01T00:00:00+00:00".to_string()),
        ext: "jpg".to_string(),
        phash: None,
        exif_date: None,
        gps_lat: None,
        gps_lon: None,
        width: None,
        height: None,
    }
}
```

- [ ] **Step 5: Run tests and verify they pass**

```bash
cargo test 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/types.rs src/output.rs
git commit -m "feat: add EXIF fields to FileRecord"
```

---

## Task 3: Implement extract_exif in hasher.rs

**Files:**
- Modify: `src/hasher.rs`

- [ ] **Step 1: Write failing tests**

In `src/hasher.rs`, inside `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn rational_to_f64_converts_correctly() {
    let r = exif::Rational { num: 51, denom: 1 };
    assert!((rational_to_f64(&r) - 51.0).abs() < f64::EPSILON);
}

#[test]
fn rational_to_f64_zero_denom_returns_zero() {
    let r = exif::Rational { num: 5, denom: 0 };
    assert_eq!(rational_to_f64(&r), 0.0);
}

#[test]
fn extract_exif_returns_none_for_non_jpeg() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("file.txt");
    fs::write(&path, b"not an image").unwrap();
    let data = extract_exif(&path);
    assert!(data.exif_date.is_none());
    assert!(data.gps_lat.is_none());
    assert!(data.gps_lon.is_none());
    assert!(data.width.is_none());
    assert!(data.height.is_none());
}

#[test]
fn extract_exif_reads_fields_from_fixture() {
    // fixture created in Task 1: tests/fixtures/sample_with_exif.jpg
    // DateTimeOriginal: 2023:08:15 14:30:00
    // GPS: 48°51'N, 2°21'E → lat=48.85, lon=2.35
    // PixelXDimension: 100, PixelYDimension: 80
    let path = std::path::Path::new("tests/fixtures/sample_with_exif.jpg");
    let data = extract_exif(path);
    assert_eq!(data.exif_date.as_deref(), Some("2023-08-15T14:30:00"));
    assert!((data.gps_lat.unwrap() - 48.85).abs() < 0.01);
    assert!((data.gps_lon.unwrap() - 2.35).abs() < 0.01);
    assert_eq!(data.width, Some(100));
    assert_eq!(data.height, Some(80));
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p dupe hasher 2>&1 | head -20
```
Expected: compile error — `rational_to_f64` and `extract_exif` not defined.

- [ ] **Step 3: Add imports and ExifData struct**

At the top of `src/hasher.rs`, add to the existing imports:

```rust
use exif::{In, Reader, Tag, Value};
```

After the existing imports, add:

```rust
struct ExifData {
    exif_date: Option<String>,
    gps_lat: Option<f64>,
    gps_lon: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
}
```

- [ ] **Step 4: Implement rational_to_f64**

Add after `ExifData`:

```rust
fn rational_to_f64(r: &exif::Rational) -> f64 {
    if r.denom == 0 { 0.0 } else { r.num as f64 / r.denom as f64 }
}
```

- [ ] **Step 5: Implement extract_gps helper**

```rust
fn extract_gps(
    exif: &exif::Exif,
    coord_tag: Tag,
    ref_tag: Tag,
    negative_ref: u8,
) -> Option<f64> {
    let coord_field = exif.get_field(coord_tag, In::PRIMARY)?;
    let ref_field = exif.get_field(ref_tag, In::PRIMARY)?;
    if let (Value::Rational(rationals), Value::Ascii(refs)) =
        (&coord_field.value, &ref_field.value)
    {
        if rationals.len() < 3 {
            return None;
        }
        let d = rational_to_f64(&rationals[0]);
        let m = rational_to_f64(&rationals[1]);
        let s = rational_to_f64(&rationals[2]);
        let mut decimal = d + m / 60.0 + s / 3600.0;
        if refs.first().and_then(|r| r.first()).copied() == Some(negative_ref) {
            decimal = -decimal;
        }
        Some(decimal)
    } else {
        None
    }
}
```

- [ ] **Step 6: Implement extract_exif**

```rust
fn extract_exif(path: &Path) -> ExifData {
    let mut result = ExifData {
        exif_date: None,
        gps_lat: None,
        gps_lon: None,
        width: None,
        height: None,
    };

    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return result,
    };
    let exif = match Reader::new().read_from_container(&mut BufReader::new(file)) {
        Ok(e) => e,
        Err(_) => return result,
    };

    // DateTimeOriginal: "YYYY:MM:DD HH:MM:SS" → "YYYY-MM-DDTHH:MM:SS"
    if let Some(field) = exif.get_field(Tag::DateTimeOriginal, In::PRIMARY) {
        if let Value::Ascii(ref vec) = field.value {
            if let Some(bytes) = vec.first() {
                let s = String::from_utf8_lossy(bytes);
                if s.len() >= 19 {
                    result.exif_date = Some(format!(
                        "{}-{}-{}T{}",
                        &s[0..4],
                        &s[5..7],
                        &s[8..10],
                        &s[11..19]
                    ));
                }
            }
        }
    }

    // PixelXDimension / PixelYDimension
    if let Some(field) = exif.get_field(Tag::PixelXDimension, In::PRIMARY) {
        result.width = match &field.value {
            Value::Long(v) => v.first().copied(),
            Value::Short(v) => v.first().map(|&x| x as u32),
            _ => None,
        };
    }
    if let Some(field) = exif.get_field(Tag::PixelYDimension, In::PRIMARY) {
        result.height = match &field.value {
            Value::Long(v) => v.first().copied(),
            Value::Short(v) => v.first().map(|&x| x as u32),
            _ => None,
        };
    }

    // GPS
    result.gps_lat = extract_gps(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S');
    result.gps_lon = extract_gps(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W');

    result
}
```

- [ ] **Step 7: Run tests and verify they pass**

```bash
cargo test -p dupe hasher 2>&1 | tail -20
```
Expected: all `hasher` tests pass, including the 4 new ones.

- [ ] **Step 8: Commit**

```bash
git add src/hasher.rs
git commit -m "feat: implement extract_exif with GPS and dimension support"
```

---

## Task 4: Update hash_file to accept exif flag

**Files:**
- Modify: `src/hasher.rs`
- Modify: `src/main.rs` (temporary: pass `false` until Task 5)

- [ ] **Step 1: Write the failing test**

In `src/hasher.rs`, inside `#[cfg(test)] mod tests`, add:

```rust
#[test]
fn hash_file_with_exif_true_populates_exif_fields_for_jpeg() {
    let path = std::path::Path::new("tests/fixtures/sample_with_exif.jpg");
    let record = hash_file(path, true).unwrap();
    assert_eq!(record.exif_date.as_deref(), Some("2023-08-15T14:30:00"));
    assert!(record.gps_lat.is_some());
    assert!(record.gps_lon.is_some());
    assert_eq!(record.width, Some(100));
    assert_eq!(record.height, Some(80));
}

#[test]
fn hash_file_with_exif_false_leaves_exif_fields_empty() {
    let path = std::path::Path::new("tests/fixtures/sample_with_exif.jpg");
    let record = hash_file(path, false).unwrap();
    assert!(record.exif_date.is_none());
    assert!(record.gps_lat.is_none());
    assert!(record.gps_lon.is_none());
    assert!(record.width.is_none());
    assert!(record.height.is_none());
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p dupe hasher::tests::hash_file_with_exif 2>&1 | head -20
```
Expected: compile error — `hash_file` takes 1 argument, not 2.

- [ ] **Step 3: Add EXIF_EXTENSIONS and update hash_file**

In `src/hasher.rs`, add the constant after `PHASH_EXTENSIONS`:

```rust
const EXIF_EXTENSIONS: &[&str] = &["jpg", "jpeg", "tiff", "heic"];
```

Replace the existing `hash_file` function with:

```rust
pub fn hash_file(path: &Path, exif: bool) -> io::Result<FileRecord> {
    let metadata = fs::metadata(path)?;
    let size_bytes = metadata.len();
    let created_at = metadata.created().ok().map(system_time_to_iso);
    let modified_at = metadata.modified().ok().map(system_time_to_iso);

    let mut hasher = blake3::Hasher::new();
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut buffer = [0u8; 65536];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 { break; }
        hasher.update(&buffer[..n]);
    }
    let hash = hasher.finalize().to_hex().to_string();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let (exif_date, gps_lat, gps_lon, width, height) =
        if exif && EXIF_EXTENSIONS.contains(&ext.as_str()) {
            let d = extract_exif(path);
            (d.exif_date, d.gps_lat, d.gps_lon, d.width, d.height)
        } else {
            (None, None, None, None, None)
        };

    Ok(FileRecord {
        path: path.to_string_lossy().to_string(),
        hash,
        size_bytes,
        created_at,
        modified_at,
        ext,
        phash: None,
        exif_date,
        gps_lat,
        gps_lon,
        width,
        height,
    })
}
```

- [ ] **Step 4: Fix existing unit tests in hasher.rs that call hash_file**

In `src/hasher.rs` tests, update every `hash_file(&path)` call to `hash_file(&path, false)`:

- `hash_file_returns_correct_record`: change to `hash_file(&path, false).unwrap()`
- `same_content_same_hash`: change both calls to `hash_file(&a, false)` and `hash_file(&b, false)`
- `different_content_different_hash`: same, both calls

- [ ] **Step 5: Fix main.rs call site (pass false temporarily)**

In `src/main.rs`, find:
```rust
hasher::hash_file(path)
```
Replace with:
```rust
hasher::hash_file(path, false)
```

- [ ] **Step 6: Run all tests**

```bash
cargo test 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/hasher.rs src/main.rs
git commit -m "feat: update hash_file to accept exif flag and extract EXIF conditionally"
```

---

## Task 5: Add --exif CLI flag and integration test

**Files:**
- Modify: `src/main.rs`
- Modify: `tests/integration.rs`

- [ ] **Step 1: Write the failing integration test**

In `tests/integration.rs`, add:

```rust
#[test]
fn exif_flag_populates_exif_fields_in_output() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    // Copy the fixture JPEG into the scan directory
    fs::copy(
        "tests/fixtures/sample_with_exif.jpg",
        scan_dir.path().join("photo.jpg"),
    )
    .unwrap();

    let status = Command::new(dupe_bin())
        .arg("--silent")
        .arg("--exif")
        .arg("--output")
        .arg(&output)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run dupe");

    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();

    assert_eq!(record["exif_date"], "2023-08-15T14:30:00");
    assert!(record["gps_lat"].as_f64().is_some());
    assert!(record["gps_lon"].as_f64().is_some());
    assert_eq!(record["width"], 100);
    assert_eq!(record["height"], 80);
}

#[test]
fn without_exif_flag_exif_fields_absent_from_output() {
    let scan_dir = tempdir().unwrap();
    let out_dir = tempdir().unwrap();
    let output = out_dir.path().join("hashes");

    fs::copy(
        "tests/fixtures/sample_with_exif.jpg",
        scan_dir.path().join("photo.jpg"),
    )
    .unwrap();

    let status = Command::new(dupe_bin())
        .arg("--silent")
        .arg("--output")
        .arg(&output)
        .arg(scan_dir.path())
        .status()
        .expect("failed to run dupe");

    assert!(status.success());

    let content = fs::read_to_string(&output).unwrap();
    let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();

    assert!(record.get("exif_date").is_none());
    assert!(record.get("gps_lat").is_none());
    assert!(record.get("gps_lon").is_none());
    assert!(record.get("width").is_none());
    assert!(record.get("height").is_none());
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test --test integration exif 2>&1 | head -30
```
Expected: compile error or test failure — `--exif` flag not recognized.

- [ ] **Step 3: Add --exif to Args in main.rs**

In `src/main.rs`, in the `Args` struct, add after `similar`:

```rust
/// Extract EXIF metadata (DateTimeOriginal, GPS, dimensions)
#[arg(long)]
exif: bool,
```

- [ ] **Step 4: Pass args.exif to hash_file**

In `src/main.rs`, find:
```rust
hasher::hash_file(path, false)
```
Replace with:
```rust
hasher::hash_file(path, args.exif)
```

- [ ] **Step 5: Build and run**

```bash
cargo build --release 2>&1 | tail -5
```
Expected: builds successfully.

```bash
cargo run -- --exif --silent --output /tmp/test_exif tests/fixtures/ 2>&1
cat /tmp/test_exif
```
Expected: one JSON line with `exif_date`, `gps_lat`, `gps_lon`, `width`, `height` fields present.

- [ ] **Step 6: Run all tests**

```bash
cargo test 2>&1 | tail -20
```
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs tests/integration.rs
git commit -m "feat: add --exif CLI flag for EXIF metadata extraction"
```

---

## Self-Review Checklist

- [x] **Spec coverage:** `--exif` flag ✓, `DateTimeOriginal` ✓, GPS ✓, `PixelXDimension`/`PixelYDimension` ✓, `kamadak-exif` crate ✓, extraction inside `hash_file` ✓, `Option::is_none` skip_serializing ✓, error handling silent ✓, EXIF_EXTENSIONS list ✓, `ExifData` struct ✓, `rational_to_f64` ✓
- [x] **Placeholder scan:** No TBDs or TODOs — all steps contain actual code
- [x] **Type consistency:** `ExifData` struct defined in Task 3 used in Task 4; `hash_file(path, false)` fixed in Task 4 Step 5 before Task 5 changes it to `args.exif`; `FileRecord` new fields defined in Task 2, used in Tasks 3 and 4
- [x] **Function name consistency:** `extract_exif`, `extract_gps`, `rational_to_f64` consistent across all tasks
