# dupe

Find and remove duplicate images across large file collections.

Scans recursively, hashes every image with BLAKE3, and writes duplicate paths to stdout one per line - ready to pipe into `trash` or `rm`. Results persist in SQLite for review, date-fixing, and semantic search.

## Binaries

| Binary | Purpose |
|--------|---------|
| `dupe` | Scan a directory, print duplicate paths to stdout |
| `dupe-report` | Read the SQLite database, generate an HTML review page |
| `dupe-fix-dates` | Set each file's mtime to its EXIF shoot date |
| `dupe-prune` | Remove stale rows, sync metadata, clean orphan embeddings |
| `dupe-embed` | Compute SigLIP embeddings for every image in the database |
| `dupe-search` | Search images by text description or example image |
| `dupe-faces` | Detect, embed, and cluster faces; enables person search |

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic` `.mp4` `.dng`

## Install

```bash
git clone git@github.com:erhangundogan/dupe.git
cd dupe
cargo build --release
```

Binaries land in `./target/release/`.

## Quickstart

```bash
# 1. Scan - duplicates printed to stdout, everything written to SQLite
# If you don't wanna review duplicates visually then you can start from point 3
dupe --output-sqlite ~/photos.db ~/Photos

# 2. Review - open the HTML report in your browser
dupe-report ~/photos.db

# 3. Delete duplicates
dupe --output-sqlite ~/photos.db ~/Photos | xargs trash

# 4. Fix timestamps - set mtime = EXIF shoot date on remaining files
dupe-fix-dates ~/photos.db

# 5. Embed images for semantic search (downloads ~1.8 GB model on first run)
dupe-embed ~/photos.db

# 6. Search by text or example image
dupe-search ~/photos.db "golden gate bridge at sunset"
dupe-search ~/photos.db --image reference.jpg

# 7. Detect, embed, and cluster faces for person search
dupe-faces ~/photos.db

# 8. Label faces in the browser UI, then save and close
dupe-report ~/photos.db --faces

# 9. Find all photos of a named person
dupe-search ~/photos.db --person "Alice"

# 10. Prune the database: remove stale rows, sync metadata, clean orphan embeddings
dupe-prune ~/photos.db

# 11. Browse the full collection with in-page similarity search
dupe-report --all ~/photos.db

# 12. Browse a Year/Month/Day drill-down gallery (static HTML, same as --all)
dupe-report --by-date ~/photos.db

# 13. Live report with labeled-face and location metadata in the lightbox
dupe-report --show-faces ~/photos.db
```

---

## dupe

```
dupe [OPTIONS] <directory>
```

| Flag | Description |
|------|-------------|
| `--output-sqlite <path>` | Write results to SQLite (upserts by path on each run) |
| `--output <path>` | Write results to JSONL (appended on each run, default: `/tmp/hashes`) |
| `--similar` | Also find visually similar images via dHash perceptual hashing |
| `--silent` | Suppress progress on stderr (stdout paths are always written) |

`--output` and `--output-sqlite` are mutually exclusive.

**stdout** receives REMOVE candidate paths, one per line - pipe directly into any deletion tool. The KEEP candidate in each group is the file with the oldest `exif_date`; falls back to `min(created_at, modified_at)` when EXIF is absent. `0000-*` EXIF dates (cameras with unset clocks) are treated as absent.

**stderr** shows scan progress and a summary. Suppressed by `--silent`.

```bash
dupe ~/Photos                                        # preview removals
dupe ~/Photos | xargs trash                          # delete immediately
dupe --silent ~/Photos > to_delete.txt               # save list for later
dupe --similar --output-sqlite ~/photos.db ~/Photos  # include visual duplicates
```

Visual duplicates use [dHash](http://www.hackerfactor.com/blog/index.php?/archives/529-Kind-of-Like-That.html): images are resized to 9x8 grayscale, adjacent pixel pairs produce a 64-bit fingerprint, and pairs with Hamming distance <= 10 are grouped as similar. Visual groups are logged to stderr only - review with `dupe-report` before deleting.

---

## dupe-report

Reads the SQLite database and generates a self-contained HTML file. There are two distinct phases where the report is useful.

**Phase 1 - review before deleting.** Run immediately after `dupe` to visually inspect duplicate groups and confirm KEEP/REMOVE decisions before touching any files.

```bash
dupe-report <db>               # output: <db>_report.html
dupe-report <db> -o out.html   # explicit output path
dupe-report <db> --heic        # embed HEIC thumbnails as JPEG (macOS only, requires qlmanage)
dupe-report <db> --heic-original  # same + 1200px lightbox version
```

**Phase 2 - browse after cleaning.** Run with `--all` once duplicates have been deleted. The report becomes a full gallery of your cleaned collection with in-page semantic search.

```bash
dupe-report <db> --all
```

`--all` automatically skips files that were recorded in the database but no longer exist on disk, so the gallery always reflects the current state of your collection. Files are checked at report generation time; the database itself is not modified. Run `dupe-prune` to permanently clean up stale rows and sync metadata.

**Drill-down by date.** `--by-date` adds a static Year > Month > Day gallery over your KEEP files, generated the same way as `--all` (no server involved - it's plain HTML and can be combined with `--all`, `--heic`, and `--heic-original`).

```bash
dupe-report <db> --by-date
```

**Live report with face and location metadata.** `--show-faces` switches `dupe-report` into server mode: it starts the same local server `--faces` uses (`localhost:7878`), but serves the interactive report (not the labeling UI) at `/`. The lightbox for each photo shows its labeled faces - click one to jump to `/person/<name>` - and a reverse-geocoded location name looked up on demand via a `/api/location` call and cached into the database for next time.

```bash
dupe-report <db> --show-faces
```

Passing `--faces` and `--show-faces` together moves the report to `/` and the labeling UI to `/faces` (with `--faces` alone, `/` stays the labeling UI as before).

The report includes:

- Stats header: files scanned, duplicate groups, wasted space
- Toolbar: Expand all / Collapse all, sort by wasted space / date kept oldest-first / newest-first
- Duplicate groups with KEEP/REMOVE badges, image thumbnails, EXIF date, GPS map links, copy-path buttons
- Lightbox for full-size images and video playback (`.mov`, `.mp4`)
- `--all`: paginated gallery of every file on disk (200 per page) with a "Similar" button on each card that opens a results panel showing the top 24 cosine-similar images, computed client-side from SigLIP embeddings inlined in the page (requires a prior `dupe-embed` run)

HEIC files show a "HEIC" placeholder by default; `--heic` embeds a 240px JPEG thumbnail via `qlmanage` (QuickLook, macOS only) - not `sips`, which silently skips the rotation some iPhone HEIC files need (see Platform notes).

`--faces` starts a local web server on `localhost:7878` for interactive face labeling: color-coded People / Unassigned Clusters / Singletons sections, drag-and-drop assignment, a "New Person" form, per-cluster detail pages with a "Dissolve cluster" action for bad groupings, per-person detail pages, and click-to-view original photos. Labels are saved back to the `faces` table as `person_label`. Close the browser tab or press Ctrl-C to stop the server.

---

## dupe-faces

Detects faces in every image in the database, embeds each face with ArcFace, and clusters faces across images into identity groups. Run this after `dupe-embed` (or independently) to enable person search.

```bash
dupe-faces <db>                         # process new hashes only (resumable)
dupe-faces <db> --reprocess             # re-detect and re-embed all hashes
dupe-faces <db> --recluster             # skip detection; re-run clustering only
dupe-faces <db> --dry-run               # detect and embed but do not write to db
dupe-faces <db> --batch <n>             # images per ONNX batch (default: 8)
dupe-faces <db> --silent                # suppress per-image progress
dupe-faces <db> --eps <f32>             # DBSCAN cosine-distance radius (default: 0.6)
dupe-faces <db> --min-cluster-size <n>  # minimum faces per cluster (default: 3)
```

Face detection uses InsightFace buffalo_l (SCRFD-10GF detector + ArcFace w600k_r50 embedder) via ONNX Runtime. Model weights are downloaded automatically on first run and cached in `~/.cache/ort/`. HEIC images are converted via `qlmanage`, matching the rest of the pipeline (see Platform notes).

Faces below `--min-cluster-size` stay as unassigned singletons instead of forming a tiny cluster. Use `--recluster --eps <value>` to retune clustering tightness without re-running detection.

**Faces workflow:**

```bash
dupe --output-sqlite ~/photos.db ~/Photos    # scan images
dupe-faces ~/photos.db                       # detect + embed + cluster faces
dupe-report ~/photos.db --faces              # label in browser, save and close
dupe-search ~/photos.db --person "Alice"     # find all photos of Alice
```

---

## dupe-prune

Syncs the database with the current state of the filesystem. Run this after deleting duplicates and fixing dates to keep the database consistent.

```bash
dupe-prune <db>            # apply all cleanup
dupe-prune <db> --dry-run  # preview without modifying the database
dupe-prune <db> --silent   # apply without per-file output
```

What it does in a single pass:

- **Removes stale rows**: deletes `file_hashes` rows for files that no longer exist on disk (e.g. duplicates that were trashed)
- **Syncs modified_at**: refreshes the `modified_at` column for surviving files from the current filesystem mtime - picks up changes made by `dupe-fix-dates` or any other tool
- **Cleans orphan embeddings**: deletes rows from `embeddings` whose hash has no remaining `file_hashes` entry

In dry-run mode, the orphan embedding count is a lower bound: it reflects only pre-existing orphans, not ones that would be created by the would-be row removals.

---

## dupe-embed and dupe-search

`dupe-embed` computes SigLIP embeddings (google/siglip-so400m-patch14-384, 1152-dim f16) for every image in the database and stores them keyed by content hash. Re-running only processes images not yet embedded. `.mov`, `.mp4`, and `.dng` files are skipped.

```bash
dupe-embed <db>                   # embed all unprocessed images
dupe-embed <db> --batch 64        # larger batch size (default: 32)
dupe-embed <db> --silent          # suppress per-image output
```

**First run downloads ~1.8 GB of model weights from Hugging Face.** Weights are cached in `~/.cache/huggingface/` and reused on every subsequent run. If all images are already embedded, the binary exits immediately without loading the model.

```bash
dupe-search <db> "sunset on beach"          # text query
dupe-search <db> --image query.jpg          # find images similar to an example
dupe-search <db> "birthday cake" -k 10 --scores  # top 10 with cosine scores
dupe-search <db> --person "Alice"           # find all photos of Alice (requires dupe-faces)
```

On macOS, inference uses Metal (Apple Silicon GPU). On Linux, CPU only - embedding large collections will be significantly slower. CUDA support can be enabled by adding `features = ["cuda"]` to the candle dependencies in `crates/dupe-ml/Cargo.toml`.

---

## dupe-fix-dates

Sets each file's `modified_at` timestamp to its EXIF shoot date, so Finder, sort-by-date views, and backup tools see the correct original capture time.

```bash
dupe-fix-dates <db> --dry-run  # preview without changing anything
dupe-fix-dates <db>            # apply
dupe-fix-dates <db> --silent   # apply without per-file output
```

Only files with `exif_date` in the database are touched. EXIF time is treated as local system time. Only `mtime` is updated (`created_at` / birth time is not changed). Files that no longer exist on disk are silently skipped and reported in the summary.

---

## Platform notes

| | macOS | Linux |
|-|-------|-------|
| `dupe`, `dupe-report`, `dupe-fix-dates` | yes | yes |
| `dupe-embed`, `dupe-search` | yes (Metal GPU) | yes (CPU only) |
| `dupe-faces` | yes (CPU via ONNX Runtime) | yes (CPU via ONNX Runtime) |
| HEIC thumbnails/decoding (report, faces, embed) | yes (via `qlmanage`) | no |
| HEIC scanning and EXIF | yes | yes |
| `created_at` field | yes | always null |

---

## Reference

### SQLite schema

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
    height      INTEGER,
    location_name TEXT
);

CREATE TABLE embeddings (
    hash        TEXT PRIMARY KEY,
    model_id    TEXT NOT NULL,
    embedding   BLOB NOT NULL,
    embedded_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS faces (
    id            INTEGER PRIMARY KEY,
    hash          TEXT NOT NULL,
    bbox          TEXT NOT NULL,
    landmark      TEXT,
    embedding     BLOB NOT NULL,
    cluster_id    INTEGER,
    person_label  TEXT,
    confirmed     INTEGER DEFAULT 0,
    is_primary    INTEGER DEFAULT 0
);
```

Re-scanning upserts existing rows by `path`. `phash` is only written with `--similar`. EXIF fields (`exif_date`, `gps_lat`, `gps_lon`, `width`, `height`) are written for jpg/jpeg/tiff/heic/dng files; null for all others. `location_name` is added by an idempotent migration on `dupe-report` startup and is not written by `dupe` itself - it's populated lazily, one coordinate at a time, when `dupe-report --show-faces` resolves and caches a reverse-geocoded location name.

### JSONL record

```json
{"path":"/Photos/2019/IMG_001.jpg","hash":"a3f2c1d8...","size_bytes":3145728,"created_at":"2019-08-12T14:22:00+00:00","modified_at":"2019-08-12T14:22:00+00:00","ext":"jpg","exif_date":"2019-08-12T14:22:00","gps_lat":41.015,"gps_lon":28.979,"width":4032,"height":3024}
```

One object per file, appended on every run. `phash` is present only with `--similar`.

### Useful queries

```bash
# Duplicate groups with file counts
sqlite3 ~/photos.db "SELECT hash, COUNT(*) n FROM file_hashes GROUP BY hash HAVING n > 1"

# Total wasted space in MB
sqlite3 ~/photos.db "SELECT SUM(size_bytes*(cnt-1))/1048576.0 FROM (SELECT size_bytes, COUNT(*) cnt FROM file_hashes GROUP BY hash HAVING cnt > 1)"

# Filter JSONL by extension
jq 'select(.ext == "heic")' /tmp/hashes

# Wasted space from JSONL
jq -s 'group_by(.hash)|map(select(length>1))|map(.[0].size_bytes*(length-1))|add/1048576' /tmp/hashes
```

## License

MIT
