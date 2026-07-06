# dupe

A fast Rust CLI tool for finding duplicate images across large file collections.

## What it does

Scans a directory recursively, hashes every image file (BLAKE3), and writes REMOVE candidates to stdout one per line: ready to pipe into `trash` or `rm`. Results are also saved to JSONL or SQLite for downstream analysis. A companion `dupe-report` binary reads the SQLite database and generates an HTML review page.

## Usage

```
dupe [OPTIONS] <directory>

Options:
  --output <path>          JSONL output file [default: /tmp/hashes]; mutually exclusive with --output-sqlite
  --output-sqlite <path>   SQLite output file; upserts by path; mutually exclusive with --output
  --similar                Also find visually similar images (perceptual hash)
  --silent                 Suppress progress output on stderr (stdout paths are always written)
```

`--output` and `--output-sqlite` cannot be used together: passing both is an error.

## Output behavior

- **stdout**: REMOVE candidate paths, one per line (pipe-ready)
- **stderr**: scan progress and summary (suppressed by `--silent`)

KEEP candidate within each group = oldest `exif_date`; falls back to `min(created_at, modified_at)` if absent.

## Build & run

```bash
cargo build --release
./target/release/dupe ~/Photos                                  # preview removals
./target/release/dupe ~/Photos | xargs trash                    # delete duplicates
./target/release/dupe --output-sqlite ~/photos.db ~/Photos      # scan to SQLite
./target/release/dupe-report ~/photos.db                        # generate HTML report
./target/release/dupe-fix-dates ~/photos.db --dry-run           # preview date fixes
./target/release/dupe-fix-dates ~/photos.db                     # apply date fixes
```

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic`

## Project structure

```
src/
  main.rs          CLI entry, pipeline orchestration
  scanner.rs       Recursive file discovery, extension filter
  hasher.rs        BLAKE3 + dHash perceptual hash + EXIF extraction
  output.rs        JSONL append, duplicate grouping, loser path output
  sqlite_output.rs SQLite upsert writer
  types.rs         FileRecord, DuplicateGroup structs
  bin/
    dupe_report.rs   HTML report generator (reads SQLite db)
    dupe_fix_dates.rs Set mtime = exif_date for files in SQLite db
tests/
  integration.rs   End-to-end tests against both binaries
```

## Key crates

- `clap`: CLI parsing
- `blake3`: fast exact hashing
- `rayon`: parallel hashing across CPU cores
- `walkdir`: recursive traversal
- `serde_json`: JSONL output
- `chrono`: date formatting
- `image` + `img_hash`: perceptual hashing for `--similar`
- `kamadak-exif`: EXIF metadata extraction (always on for jpg/jpeg/tiff/heic)
- `rusqlite` (bundled): SQLite output for `--output-sqlite` and `dupe-report`
- `filetime`: set file `mtime` portably for `dupe-fix-dates`

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

EXIF extraction runs automatically for `jpg`, `jpeg`, `tiff`, and `heic` files. Fields are `null`/absent when the file has no EXIF data.

| Field | Type | Notes |
|-------|------|-------|
| `exif_date` | string | `DateTimeOriginal` formatted as `YYYY-MM-DDTHH:MM:SS`, camera-local time, no timezone |
| `gps_lat` | float | Decimal degrees, negative = South |
| `gps_lon` | float | Decimal degrees, negative = West |
| `width` | integer | From `PixelXDimension` |
| `height` | integer | From `PixelYDimension` |

## dupe-report

Reads `file_hashes` from a SQLite database and writes a self-contained HTML file.

```bash
dupe-report <db>                    # output: <db>_report.html
dupe-report <db> -o <out>           # explicit output path
dupe-report <db> --heic             # embed HEIC thumbnails as base64 JPEG (macOS/sips)
dupe-report <db> --heic-original    # embed HEIC thumbnails + 1200px lightbox version
```

Report includes:
- Stats header (files, groups, wasted space)
- Toolbar: Expand all / Collapse all / Sort dropdown (wasted space, date kept oldest-first, date kept newest-first)
- Duplicate groups sorted by wasted space by default; sorting is instant DOM reorder
- Per-file: thumbnail preview, KEEP/REMOVE badge, filename, path + copy button, size, modified, EXIF date, GPS link, dimensions
- Image thumbnails via `file://` URL (lazy-loaded, force-loaded on group expand)
- `.mov` files shown as `<video>` thumbnail; click opens lightbox with playback controls
- `.heic` files: "HEIC" text by default; `--heic` embeds 240px JPEG thumbnail; `--heic-original` also embeds 1200px lightbox version (macOS only, requires `sips`)
- Lightbox overlay for full-size image/video viewing; Escape or backdrop click closes

## dupe-fix-dates

Reads `file_hashes` from a SQLite database and sets `modified_at` on each file to its `exif_date`. Only files with `exif_date` present are touched. Operates on all such files (KEEP and REMOVE alike: REMOVE files will be deleted afterward anyway).

```bash
dupe-fix-dates <db>            # apply: set mtime = exif_date for all files with EXIF
dupe-fix-dates <db> --dry-run  # preview without modifying anything
dupe-fix-dates <db> --silent   # suppress per-file output (errors always shown)
```

- `exif_date` is camera-local time with no timezone; treated as local system time when computing the UNIX timestamp
- Only `modified_at` is set (`created_at` / birth time requires a macOS-only syscall and is not supported)
- Exits with code 1 if any file could not be updated

## Design spec

`docs/superpowers/specs/2026-06-09-dupe-design.md`
