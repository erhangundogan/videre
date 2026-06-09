# dupe — Design Spec
Date: 2026-06-09

## Purpose

`dupe` is a fast CLI tool for scanning large image collections (tens of thousands of files) to find exact and visually similar duplicates. It is the **ingestion phase** of a data pipeline: it produces a JSONL file that can be queried with `jq` today and loaded into PostgreSQL or Redis later for visual grouping and date-based analysis.

The core problem: the same image may exist in many folders with different filenames and dates (due to repeated copying). `dupe` hashes every file, groups by hash, and surfaces duplicate groups ranked by file date — so the user can identify which copy is the original.

---

## CLI Interface

```
dupe [OPTIONS] <directory>

Arguments:
  <directory>          Directory to scan recursively

Options:
  --output <path>      JSONL output file [default: /tmp/hashes]
  --similar            Also find visually similar images via perceptual hash
  --silent             Suppress console output
  -h, --help
  -V, --version
```

---

## Supported File Types

`.jpg`, `.jpeg`, `.png`, `.gif`, `.webp`, `.bmp`, `.tiff`, `.mov`, `.heic`

---

## Data Flow

```
walkdir (recursive)
  → filter by extension
  → rayon par_iter() — parallel BLAKE3 hash + metadata per file
  → Vec<FileRecord>
  → append each record as one JSONL line to --output file
  → group by hash → HashMap<hash, Vec<FileRecord>>
  → [if --similar] compute pHash per image, cluster by Hamming distance ≤ 10
  → [unless --silent] print duplicate groups to console, oldest file first
```

---

## JSONL Output Format

One JSON object per line, appended to the output file:

```json
{
  "path": "/photos/vacation/IMG_001.jpg",
  "hash": "abc123def456...",
  "size_bytes": 2048576,
  "created_at": "2023-01-15T10:30:00Z",
  "modified_at": "2024-06-01T14:22:00Z",
  "ext": "jpg"
}
```

The file is append-only. Re-running adds new records; downstream tools are responsible for deduplication by path if needed.

---

## Console Output (default, suppressed with --silent)

After scanning, duplicate groups are printed:

```
Found 3 duplicate group(s):

Group 1 (hash: abc123...):
  [ORIGINAL?] /photos/2020/IMG_001.jpg  modified: 2020-03-01
              /photos/backup/IMG_001.jpg modified: 2022-07-15
              /photos/copy2/photo.jpg    modified: 2023-11-02

Group 2 ...
```

Files within each group are sorted by `modified_at` ascending — the oldest is flagged as the likely original.

---

## Module Structure

```
src/
  main.rs       CLI entry point, argument parsing (clap), pipeline orchestration
  scanner.rs    walkdir traversal, extension filtering → Vec<PathBuf>
  hasher.rs     BLAKE3 exact hash + pHash computation, metadata extraction
  output.rs     JSONL serialization + append, console grouping report
  types.rs      FileRecord, DuplicateGroup structs (serde Serialize/Deserialize)
```

---

## Crates

| Crate | Purpose |
|-------|---------|
| `clap` (4.x) | CLI argument parsing |
| `blake3` | Fast cryptographic hashing |
| `rayon` | Parallel file processing |
| `walkdir` | Recursive directory traversal |
| `serde` + `serde_json` | JSONL serialization |
| `chrono` | Date/time formatting |
| `image` | Image decoding (for pHash) |
| `img_hash` | Perceptual hashing |

---

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Unreadable file (permissions, corruption) | Log to stderr, skip file, continue scan |
| `--similar` on `.mov` file | Skip pHash (video), exact hash only |
| Output file not writable | Fatal error with clear message, exit 1 |
| Directory does not exist | Fatal error with clear message, exit 1 |

---

## Future Extensions (out of scope now)

- PostgreSQL loader: `cat /tmp/hashes | dupe-load --pg postgresql://...`
- Redis ingestion for visual grouping UI
- `--threshold <n>` to tune pHash Hamming distance
- `--ext` flag for custom file type filtering
