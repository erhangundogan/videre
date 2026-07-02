# dupe

A fast Rust CLI tool for finding duplicate images across large file collections.

## What it does

Scans a directory recursively, hashes every image file (BLAKE3), and outputs results to JSONL or SQLite with full metadata per file. Groups duplicates and prints them to console ranked by date (oldest = likely original). Designed as the ingestion phase of a pipeline — output can be loaded into PostgreSQL or Redis for visual analysis.

## Usage

```
dupe [OPTIONS] <directory>

Options:
  --output <path>          JSONL output file [default: /tmp/hashes]; mutually exclusive with --output-sqlite
  --output-sqlite <path>   SQLite output file; upserts by path; mutually exclusive with --output
  --similar                Also find visually similar images (perceptual hash)
  --exif                   Extract EXIF metadata (DateTimeOriginal, GPS, dimensions) for jpg/jpeg/tiff/heic
  --silent                 Suppress console output
```

`--output` and `--output-sqlite` cannot be used together — passing both is an error.

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic`

## Build & run

```bash
cargo build --release
./target/release/dupe /path/to/photos
./target/release/dupe --similar --output ~/dupes.jsonl /path/to/photos
./target/release/dupe --exif --output ~/dupes.jsonl /path/to/photos
./target/release/dupe --exif --output-sqlite ~/photos.db /path/to/photos
```

## Project structure

```
src/
  main.rs          CLI entry, pipeline orchestration
  scanner.rs       Recursive file discovery, extension filter
  hasher.rs        BLAKE3 + perceptual hash + EXIF extraction, metadata
  output.rs        JSONL append, console duplicate report
  sqlite_output.rs SQLite upsert writer
  types.rs         FileRecord, DuplicateGroup structs
```

## Key crates

- `clap` — CLI parsing
- `blake3` — fast exact hashing
- `rayon` — parallel hashing across CPU cores
- `walkdir` — recursive traversal
- `serde_json` — JSONL output
- `chrono` — date formatting
- `image` + `img_hash` — perceptual hashing for `--similar`
- `kamadak-exif` — EXIF metadata extraction for `--exif`
- `rusqlite` (bundled) — SQLite output for `--output-sqlite`

## SQLite schema

```sql
CREATE TABLE file_hashes (
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
);
```

Re-scanning the same folder with the same SQLite file upserts (overwrites) existing rows via `INSERT OR REPLACE`. `phash` is stored as signed `INTEGER` (cast from `u64`).

## EXIF fields

| Field | Type | Notes |
|-------|------|-------|
| `exif_date` | string | `DateTimeOriginal` formatted as `YYYY-MM-DDTHH:MM:SS`, camera-local time, no timezone |
| `gps_lat` | float | Decimal degrees, negative = South |
| `gps_lon` | float | Decimal degrees, negative = West |
| `width` | integer | From `PixelXDimension` |
| `height` | integer | From `PixelYDimension` |

Only populated when `--exif` is passed and file extension is `jpg`, `jpeg`, `tiff`, or `heic`.

## Design spec

`docs/superpowers/specs/2026-06-09-dupe-design.md`
