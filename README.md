# dupe

A fast CLI tool for finding duplicate images across large file collections.

Scans directories recursively, hashes every image with BLAKE3, and reports duplicate groups ranked by file date — so you can identify which copy is the original. Designed as the ingestion phase of a data pipeline: results are written as JSONL for downstream loading into PostgreSQL or Redis.

## Features

- **Exact duplicates** — BLAKE3 hash, byte-for-byte identical files
- **Visual duplicates** — dHash perceptual hashing via `--similar` flag (finds re-saves, resized copies)
- **EXIF metadata** — `--exif` flag extracts shoot date, GPS coordinates, and dimensions from JPEG/HEIC/TIFF
- **Parallel processing** — rayon saturates all CPU cores; handles tens of thousands of files
- **JSONL output** — one JSON object per file, append-mode, ready for `jq` or database ingestion
- **Date-aware reporting** — duplicate groups sorted oldest-first to surface likely originals

## Supported File Types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic`

## Installation

```bash
git clone git@github.com:erhangundogan/dupe.git
cd dupe
cargo build --release
```

Binary is at `./target/release/dupe`.

## Usage

```bash
dupe [OPTIONS] <directory>
```

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `--output <path>` | JSONL output file (appended on each run) | `/tmp/hashes` |
| `--similar` | Also find visually similar images via perceptual hash | off |
| `--exif` | Extract EXIF metadata (DateTimeOriginal, GPS coordinates, image dimensions) | off |
| `--silent` | Suppress all console output | off |

### Examples

```bash
# Find exact duplicates in a photo library
dupe ~/Photos

# Also find visually similar images (resized, re-compressed copies)
dupe --similar ~/Photos

# Write results to a custom file
dupe --output ~/dupes.jsonl ~/Photos

# Silent mode — JSONL output only, no console output
dupe --silent --output ~/dupes.jsonl ~/Photos

# Extract EXIF metadata (shoot date, GPS, dimensions) alongside hashes
dupe --exif --output ~/dupes.jsonl ~/Photos

# Combine flags
dupe --exif --similar --output ~/dupes.jsonl ~/Photos
```

## Output

### Console (default)

```
Scanning "/Users/erhan/Photos"...
Found 12483 file(s) to process
Wrote 12483 record(s) to "/tmp/hashes"

Found 3 duplicate group(s):

Group 1 (hash: a3f2c1d8...):
  [ORIGINAL?] /Photos/2019/vacation/IMG_001.jpg  modified: 2019-08-12T14:22:00+00:00
              /Photos/backup/IMG_001.jpg          modified: 2022-03-01T09:15:00+00:00
              /Photos/copy2/photo.jpg             modified: 2023-11-10T18:44:00+00:00
```

Files within each group are sorted by modification date ascending. The oldest file is flagged as `[ORIGINAL?]`.

### JSONL file

One JSON object per line, appended on every run:

Without `--exif`:
```json
{"path":"/Photos/2019/vacation/IMG_001.jpg","hash":"a3f2c1d8...","size_bytes":3145728,"created_at":"2019-08-12T14:22:00+00:00","modified_at":"2019-08-12T14:22:00+00:00","ext":"jpg"}
```

With `--exif`:
```json
{"path":"/Photos/2019/vacation/IMG_001.jpg","hash":"a3f2c1d8...","size_bytes":3145728,"created_at":"2019-08-12T14:22:00+00:00","modified_at":"2019-08-12T14:22:00+00:00","ext":"jpg","exif_date":"2019-08-12T14:22:00","gps_lat":41.015,"gps_lon":28.979,"width":4032,"height":3024}
```

#### Fields

| Field | Type | Description |
|-------|------|-------------|
| `path` | string | Absolute file path |
| `hash` | string | BLAKE3 hex hash (exact duplicate key) |
| `size_bytes` | number | File size in bytes |
| `created_at` | string \| null | ISO 8601 creation time (null on Linux) |
| `modified_at` | string \| null | ISO 8601 modification time |
| `ext` | string | Lowercase file extension |
| `phash` | number | dHash value (only present with `--similar`) |
| `exif_date` | string \| null | Camera-local shoot date from EXIF `DateTimeOriginal`, no timezone (only with `--exif`) |
| `gps_lat` | number \| null | GPS latitude in decimal degrees, negative = South (only with `--exif`) |
| `gps_lon` | number \| null | GPS longitude in decimal degrees, negative = West (only with `--exif`) |
| `width` | number \| null | Image width in pixels from EXIF (only with `--exif`) |
| `height` | number \| null | Image height in pixels from EXIF (only with `--exif`) |

## Pipeline Usage

The JSONL output is designed for downstream processing.

### Query with jq

```bash
# Find all files with a given hash
cat /tmp/hashes | jq -r 'select(.hash == "a3f2c1d8...") | .path'

# List all duplicate hashes (appearing more than once)
cat /tmp/hashes | jq -r '.hash' | sort | uniq -d

# Find largest duplicate files
cat /tmp/hashes | jq -s 'group_by(.hash) | map(select(length > 1)) | flatten | sort_by(-.size_bytes)'
```

### Load into PostgreSQL

```sql
CREATE TABLE file_hashes (
    path        TEXT PRIMARY KEY,
    hash        TEXT NOT NULL,
    size_bytes  BIGINT,
    created_at  TIMESTAMPTZ,
    modified_at TIMESTAMPTZ,
    ext         TEXT,
    phash       BIGINT,
    exif_date   TIMESTAMP,
    gps_lat     DOUBLE PRECISION,
    gps_lon     DOUBLE PRECISION,
    width       INTEGER,
    height      INTEGER
);
```

```bash
cat /tmp/hashes | \
  jq -r '[.path, .hash, .size_bytes, .created_at, .modified_at, .ext, .phash, .exif_date, .gps_lat, .gps_lon, .width, .height] | @tsv' | \
  psql -c "COPY file_hashes FROM STDIN"
```

## How It Works

### Exact duplicates (default)

Files are hashed with [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) using a 64 KB streaming buffer. Files with identical hashes are exact byte-for-byte copies regardless of filename or location.

### Visual duplicates (`--similar`)

Uses [dHash](http://www.hackerfactor.com/blog/index.php?/archives/529-Kind-of-Like-That.html) (difference hash):

1. Resize image to 9×8 pixels (grayscale)
2. For each row, compare 8 adjacent pixel pairs → 1 bit per pair
3. Result: 64-bit fingerprint

Two images are considered similar when their Hamming distance is ≤ 10 (out of 64 bits). This finds resized copies, re-compressed JPEGs, and minor edits. `.mov` and `.heic` files are excluded from perceptual hashing (exact hash still runs).

## Project Structure

```
src/
  main.rs      CLI entry point, argument parsing, pipeline orchestration
  scanner.rs   Recursive file discovery, extension filtering
  hasher.rs    BLAKE3 hashing, dHash perceptual hashing, EXIF extraction
  output.rs    JSONL append writer, duplicate grouping, console report
  types.rs     FileRecord, DuplicateGroup structs
tests/
  integration.rs  End-to-end tests against the real binary
```

## License

MIT
