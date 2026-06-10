# EXIF Extraction Feature — Design Spec

**Date:** 2026-06-10  
**Feature:** `--exif` CLI flag for extracting EXIF metadata during image hashing

---

## Background

`dupe` is the ingestion phase of a photo deduplication pipeline. The user has a large library where the same image exists in multiple locations with potentially corrupted filesystem timestamps (from repeated copying). The most reliable date signal for identifying the true original is `DateTimeOriginal` — the shoot date written by the camera into the EXIF header, which survives file copy operations unchanged.

GPS coordinates and image dimensions are also captured since they're low-cost to extract and useful for downstream analysis.

---

## Goal

Add an `--exif` flag that extracts EXIF metadata from supported image files during the hashing pass and includes it in the JSONL output. Performance impact must be negligible.

---

## Approach

Extract EXIF inside `hash_file` (Approach A). After BLAKE3 finishes streaming the file, re-open and parse EXIF from the first few KB. The file is already in the OS page cache from the hash read, so re-opening costs almost nothing. The extraction runs inside the existing rayon parallel tasks — no extra pass, no sequential bottleneck.

---

## Supported Formats

EXIF extraction is attempted for: `.jpg`, `.jpeg`, `.tiff`, `.heic`

Skipped silently for all other extensions (`.png`, `.webp`, `.gif`, `.bmp`, `.mov`). These formats rarely carry meaningful EXIF and the `kamadak-exif` crate handles parse failures gracefully.

---

## New CLI Flag

```
--exif    Extract EXIF metadata (DateTimeOriginal, GPS, dimensions)
```

Added to `Args` in `main.rs` alongside `--similar` and `--silent`.

---

## New JSONL Fields

All fields use `#[serde(skip_serializing_if = "Option::is_none")]` — absent from output when `--exif` is not passed.

| Field | Type | Source EXIF tag | Notes |
|---|---|---|---|
| `exif_date` | `Option<String>` | `DateTimeOriginal` | ISO 8601 string. Falls back to `DateTime` if `DateTimeOriginal` absent. |
| `gps_lat` | `Option<f64>` | `GPSLatitude` + `GPSLatitudeRef` | Decimal degrees. Negative = South. |
| `gps_lon` | `Option<f64>` | `GPSLongitude` + `GPSLongitudeRef` | Decimal degrees. Negative = West. |
| `width` | `Option<u32>` | `PixelXDimension` | |
| `height` | `Option<u32>` | `PixelYDimension` | |

---

## GPS Conversion

EXIF stores GPS as three rational numbers (degrees, minutes, seconds) plus a reference character (N/S or E/W). Conversion to decimal degrees:

```
decimal = degrees + minutes/60 + seconds/3600
if ref == 'S' || ref == 'W' { decimal = -decimal }
```

A helper `rational_to_f64(r: exif::Rational) -> f64` converts a single rational value.

---

## Crate

Add `kamadak-exif = "0.5"` to `[dependencies]` in `Cargo.toml`.

- Pure Rust, no C dependencies
- Supports JPEG, TIFF, HEIC/HEIF
- Returns `None`-safe: parse errors are silently ignored (no panic)

---

## Code Changes

### `Cargo.toml`
Add `kamadak-exif = "0.5"`.

### `src/types.rs`
Add five optional fields to `FileRecord`:

```rust
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
```

### `src/hasher.rs`

Add:
- `const EXIF_EXTENSIONS: &[&str]` — `["jpg", "jpeg", "tiff", "heic"]`
- `fn extract_exif(path: &Path) -> ExifData` — opens file, runs `kamadak-exif` reader, extracts all five fields, returns a plain struct
- `fn rational_to_f64(r: exif::Rational) -> f64` — helper for GPS conversion
- Update `hash_file` signature to `hash_file(path: &Path, exif: bool) -> io::Result<FileRecord>`
- After BLAKE3 loop, if `exif && EXIF_EXTENSIONS.contains(ext)`, call `extract_exif` and populate the new fields

A small internal `ExifData` struct carries the extracted values from `extract_exif` back to `hash_file`:

```rust
struct ExifData {
    exif_date: Option<String>,
    gps_lat: Option<f64>,
    gps_lon: Option<f64>,
    width: Option<u32>,
    height: Option<u32>,
}
```

### `src/main.rs`

- Add `--exif` bool field to `Args`
- Pass `args.exif` into `hasher::hash_file(path, args.exif)` inside the `par_iter` closure

---

## Error Handling

EXIF extraction failures (corrupt headers, unsupported format variant, missing tags) are silently ignored — the field is left `None`. The file record is always written. No warnings emitted.

---

## Testing

- Unit test in `hasher.rs`: create a real JPEG with known EXIF using the `img_hash` / `image` crate or a fixture file, assert `exif_date`, `width`, `height` are populated
- Unit test: file without EXIF returns all-`None` EXIF fields
- Unit test: GPS rational-to-decimal conversion
- Integration test in `tests/integration.rs`: run binary with `--exif` flag, verify JSONL output contains `exif_date` field for a fixture JPEG

---

## Out of Scope

- `Make` / `Model` camera fields (not requested)
- Writing EXIF to modify files
- Displaying EXIF data in the console duplicate report
- EXIF-based grouping logic (belongs in downstream pipeline)
