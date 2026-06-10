# dupe

A fast Rust CLI tool for finding duplicate images across large file collections.

## What it does

Scans a directory recursively, hashes every image file (BLAKE3), and outputs a JSONL file with full metadata per file. Groups duplicates and prints them to console ranked by date (oldest = likely original). Designed as the ingestion phase of a pipeline — output can be loaded into PostgreSQL or Redis for visual analysis.

## Usage

```
dupe [OPTIONS] <directory>

Options:
  --output <path>   JSONL output file [default: /tmp/hashes]
  --similar         Also find visually similar images (perceptual hash)
  --exif            Extract EXIF metadata (DateTimeOriginal, GPS, dimensions) for jpg/jpeg/tiff/heic
  --silent          Suppress console output
```

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic`

## Build & run

```bash
cargo build --release
./target/release/dupe /path/to/photos
./target/release/dupe --similar --output ~/dupes.jsonl /path/to/photos
./target/release/dupe --exif --output ~/dupes.jsonl /path/to/photos
```

## Project structure

```
src/
  main.rs     CLI entry, pipeline orchestration
  scanner.rs  Recursive file discovery, extension filter
  hasher.rs   BLAKE3 + perceptual hash + EXIF extraction, metadata
  output.rs   JSONL append, console duplicate report
  types.rs    FileRecord, DuplicateGroup structs
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

## Design spec

`docs/superpowers/specs/2026-06-09-dupe-design.md`
