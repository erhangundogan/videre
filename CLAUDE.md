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

KEEP candidate within each group = oldest `exif_date`; falls back to `min(created_at, modified_at)` if absent. `exif_date` values of `0000-00-00T00:00:00` (cameras with unset clocks) are treated as absent.

## Build & run

```bash
cargo build --release
./target/release/dupe ~/Photos                                  # preview removals
./target/release/dupe ~/Photos | xargs trash                    # delete duplicates
./target/release/dupe --output-sqlite ~/photos.db ~/Photos      # scan to SQLite
./target/release/dupe-report ~/photos.db                        # generate HTML report
./target/release/dupe-fix-dates ~/photos.db --dry-run           # preview date fixes
./target/release/dupe-fix-dates ~/photos.db                     # apply date fixes
./target/release/dupe-prune ~/photos.db --dry-run               # preview prune
./target/release/dupe-prune ~/photos.db                         # prune stale rows + sync metadata
./target/release/dupe-embed ~/photos.db                         # embed all images (resumable)
./target/release/dupe-search ~/photos.db "sunset on beach"      # text search
./target/release/dupe-search ~/photos.db --image query.jpg      # find similar images
```

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic` `.mp4` `.dng`

## Project structure

```
crates/
  dupe/
    Cargo.toml
    src/{main.rs,scanner.rs,hasher.rs,output.rs,sqlite_output.rs,types.rs,bin/}
    tests/integration.rs
  dupe-core/
    Cargo.toml
    src/lib.rs
    src/vectors.rs
    src/embeddings.rs
  dupe-ml/
    Cargo.toml
    src/lib.rs
    src/{device.rs,model.rs,preprocess.rs,search.rs}
    src/bin/{dupe-embed.rs,dupe-search.rs}
```

## Key crates

- `clap`: CLI parsing
- `blake3`: fast exact hashing
- `rayon`: parallel hashing across CPU cores
- `walkdir`: recursive traversal
- `serde_json`: JSONL output
- `chrono`: date formatting
- `image`: image decoding and dHash perceptual hashing for `--similar` (implemented inline, no img_hash crate)
- `kamadak-exif`: EXIF metadata extraction (always on for jpg/jpeg/tiff/heic/dng)
- `rusqlite` (bundled): SQLite output for `--output-sqlite` and `dupe-report`
- `filetime`: set file `mtime` portably for `dupe-fix-dates`
- `candle-core` / `candle-nn` / `candle-transformers`: SigLIP inference, Metal on macOS
- `tokenizers`: text tokenization for SigLIP
- `hf-hub`: Hugging Face model weight downloads
- `half`: f16 storage for embeddings

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

EXIF extraction runs automatically for `jpg`, `jpeg`, `tiff`, `heic`, and `dng` files. Fields are `null`/absent when the file has no EXIF data.

| Field | Type | Notes |
|-------|------|-------|
| `exif_date` | string | `DateTimeOriginal` formatted as `YYYY-MM-DDTHH:MM:SS`, camera-local time, no timezone; `0000-*` values from cameras with unset clocks are discarded (stored as null) |
| `gps_lat` | float | Decimal degrees, negative = South |
| `gps_lon` | float | Decimal degrees, negative = West |
| `width` | integer | From `PixelXDimension` |
| `height` | integer | From `PixelYDimension` |

## dupe-report

Reads `file_hashes` from a SQLite database and writes a self-contained HTML file. Two usage phases:

**Phase 1 (pre-deletion):** run without `--all` to review duplicate groups with KEEP/REMOVE badges before deleting anything.

**Phase 2 (post-deletion):** run with `--all` to browse the full cleaned collection with in-page similarity search. Files recorded in the database but no longer on disk are automatically excluded (checked at generation time; the database is not modified). `dupe-prune` is planned as a future command to remove stale rows permanently.

```bash
dupe-report <db>                    # output: <db>_report.html
dupe-report <db> -o <out>           # explicit output path
dupe-report <db> --heic             # embed HEIC thumbnails as base64 JPEG (macOS/sips)
dupe-report <db> --heic-original    # embed HEIC thumbnails + 1200px lightbox version
dupe-report <db> --all              # all-files gallery + in-page similarity search
```

Report includes:
- Stats header (files, groups, wasted space)
- Toolbar: Expand all / Collapse all / Sort dropdown (wasted space, date kept oldest-first, date kept newest-first)
- Duplicate groups sorted by wasted space by default; sorting is instant DOM reorder
- Per-file: thumbnail preview, KEEP/REMOVE badge, filename, path + copy button, size, created, modified, EXIF date, GPS link, dimensions
- Image thumbnails via `file://` URL (lazy-loaded, force-loaded on group expand)
- `.mov` and `.mp4` files shown as `<video>` thumbnail; click opens lightbox with playback controls
- `.heic` files: "HEIC" text by default; `--heic` embeds 240px JPEG thumbnail; `--heic-original` also embeds 1200px lightbox version (macOS only, requires `sips`)
- Lightbox overlay for full-size image/video viewing; Escape or backdrop click closes
- `--all`: gallery of files that exist on disk (200-card pages, lazy thumbnails) + "Similar" button per file; click opens a results panel with top-24 cosine matches using inline SigLIP f16 embeddings (requires prior `dupe-embed` run)

## dupe-fix-dates

Reads `file_hashes` from a SQLite database and sets `modified_at` on each file to its `exif_date`. Only files with `exif_date` present are touched. Operates on all such files (KEEP and REMOVE alike: REMOVE files will be deleted afterward anyway).

```bash
dupe-fix-dates <db>            # apply: set mtime = exif_date for all files with EXIF
dupe-fix-dates <db> --dry-run  # preview without modifying anything
dupe-fix-dates <db> --silent   # suppress per-file output (errors always shown)
```

- `exif_date` is camera-local time with no timezone; treated as local system time when computing the UNIX timestamp
- Only `modified_at` is set (`created_at` / birth time requires a macOS-only syscall and is not supported)
- Files that no longer exist on disk (e.g. trashed duplicates still in the DB) are silently skipped and reported in the summary as "no longer on disk (skipped)"
- Exits with code 1 if any file could not be updated (missing files are not counted as errors)

## dupe-prune

Syncs the SQLite database with the current filesystem state. Run after deleting duplicates and fixing dates.

```bash
dupe-prune <db>            # apply
dupe-prune <db> --dry-run  # preview without modifying the database
dupe-prune <db> --silent   # apply without per-file output
```

In a single pass:
- Deletes `file_hashes` rows for files no longer on disk
- Refreshes `modified_at` for surviving files from their current filesystem mtime
- Deletes `embeddings` rows whose hash has no remaining `file_hashes` entry (orphan cleanup)

Shared-hash safety: if two paths share the same hash and one file is deleted, the embedding is only removed if no `file_hashes` row for that hash survives. Dry-run orphan count is a lower bound (pre-existing orphans only; does not account for orphans created by the would-be deletions). Exits with code 1 if any row update fails.

## dupe-embed / dupe-search

`dupe-embed <db>` embeds every unique image hash (SigLIP so400m/14-384, 1152-dim,
L2-normalized f16 BLOB) into an `embeddings` table keyed by content hash. Resumable:
re-running processes only missing hashes. `--batch` (default 32), `--chunk` (rows per
transaction, default 500), `--silent`. HEIC via sips; DNG, mov, and mp4 skipped (the
`image` crate has no DNG decoder; EXIF metadata is still available from the scan).

`dupe-search <db> "query"` or `dupe-search <db> --image photo.jpg` prints matching
paths to stdout (all duplicate paths per matched hash). `-k` top-k (default 20),
`--scores` prepends cosine score. Brute-force exact scan; no ANN index at this scale.

Model weights auto-download from Hugging Face (google/siglip-so400m-patch14-384) on
first run.

Embeddings schema:

```sql
CREATE TABLE embeddings (
    hash        TEXT PRIMARY KEY,
    model_id    TEXT NOT NULL,
    embedding   BLOB NOT NULL,
    embedded_at TEXT NOT NULL
);
```

## Design specs

- `docs/superpowers/specs/2026-06-09-dupe-design.md` - core dedupe tool
- `docs/superpowers/specs/2026-07-08-image-search-design.md` - semantic image search (dupe-embed, dupe-search; SigLIP + candle/Metal; Cargo workspace restructure)
