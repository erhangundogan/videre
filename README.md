# videre

A local-first media library toolkit: dedupe, semantic search, faces, and reports over one SQLite database.

Scans recursively, hashes every image with BLAKE3, and writes duplicate paths to stdout one per line - ready to pipe into `trash` or `rm`. Results persist in a single SQLite database shared by every subcommand: date-fixing, pruning, semantic embedding/search, face detection and labeling, and HTML reports.

## Subcommands

| Subcommand | Purpose |
|------------|---------|
| `videre dedupe` | Scan a directory, print duplicate paths to stdout |
| `videre report` | Read the SQLite database, generate an HTML review page (or serve the live report/labeling UI) |
| `videre fix-dates` | Set each file's mtime to its EXIF shoot date |
| `videre prune` | Remove stale rows, sync metadata, clean orphan embeddings |
| `videre embed` | Compute SigLIP embeddings for every image in the database |
| `videre search` | Search images by text description, example image, or person name |
| `videre faces` | Detect, embed, and cluster faces; enables person search |
| `videre watch` | Background loop that keeps scan/faces/HEIC-cache/location data fresh |
| `videre config` | Show or edit videre's config and default paths (`~/.videre`) |

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic` `.mp4` `.dng`

## Install

```bash
git clone git@github.com:erhangundogan/videre.git
cd videre
cargo build --release
```

The single binary lands at `./target/release/videre`.

## Quickstart

All commands below use the default database at `~/.videre/hashes.db`, created automatically
on first write. Pass `--db <path>` (readers) or `--output-sqlite <path>` (writers) to point
at a different file instead - see [The ~/.videre home directory](#the-videre-home-directory).

```bash
# 1. Scan - duplicates printed to stdout, everything written to the default SQLite db
# If you don't wanna review duplicates visually then you can start from point 3
videre dedupe ~/Photos

# 2. Review - open the HTML report in your browser
videre report

# 3. Delete duplicates
videre dedupe ~/Photos | xargs trash

# 4. Fix timestamps - set mtime = EXIF shoot date on remaining files
videre fix-dates

# 5. Embed images for semantic search (downloads ~1.8 GB model on first run)
videre embed

# 6. Search by text or example image
videre search "golden gate bridge at sunset"
videre search --image reference.jpg

# 7. Detect, embed, and cluster faces for person search
videre faces

# 8. Label faces in the browser UI, then save and close
videre report --faces

# 9. Find all photos of a named person
videre search --person "Alice"

# 10. Prune the database: remove stale rows, sync metadata, clean orphan embeddings
videre prune

# 11. Browse the full collection with in-page similarity search
videre report --all

# 12. Browse a Year/Month/Day drill-down gallery (static HTML, same as --all)
videre report --by-date

# 13. Live report with labeled-face and location metadata in the lightbox
videre report --show-faces

# 14. Keep everything fresh in the background (run alongside step 13, same db)
videre watch ~/Photos
```

To use an explicit database file instead of the default:

```bash
videre dedupe --output-sqlite ~/photos.db ~/Photos
videre report --db ~/photos.db
videre search --db ~/photos.db "golden gate bridge at sunset"
videre watch --output-sqlite ~/photos.db ~/Photos
```

---

## The ~/.videre home directory

Every subcommand shares a home directory at `~/.videre` (override with the `VIDERE_HOME`
environment variable). It holds:

```
~/.videre/
  hashes.db      # default SQLite database
  hashes.jsonl   # default JSONL output (only written when --output is used bare)
  config.toml    # optional overrides, e.g. default_db
```

The directory and its files are created lazily by writers (`dedupe`, `watch`, `config set`) -
nothing is written just by running a reader.

**Database resolution order**, used by every subcommand that reads or writes SQLite:

1. An explicit path: `--db <path>` on readers (`report`, `fix-dates`, `prune`, `embed`,
   `search`, `faces`), `--output-sqlite <path>` on writers (`dedupe`, `watch`)
2. `default_db` in `~/.videre/config.toml`, if set
3. `~/.videre/hashes.db`

Readers never create a database. If the resolved path doesn't exist, they print:

```
no database found at <path>; run 'videre dedupe <dir>' first
```

and exit 1 (under `search --json` this arrives as the JSON error object instead).

**`videre config`** shows the resolved paths and current settings:

```bash
videre config                        # show home dir, config.toml path, default_db, resolved db, jsonl path
videre config set db ~/photos.db     # persist default_db (written as an absolute path)
videre config unset db               # remove default_db, falling back to ~/.videre/hashes.db
```

`config set`/`config unset` preserve any other keys already in `config.toml`.

### Breaking changes

If you're upgrading from an earlier version, three behaviors changed:

1. The six reader commands (`report`, `fix-dates`, `prune`, `embed`, `search`, `faces`) no
   longer take a database positional argument - pass `--db <path>` instead.
2. Bare `videre dedupe <dir>` now writes SQLite to the resolved default db instead of JSONL
   to `/tmp/hashes`. Use `--output` (with or without a value) to get JSONL again.
3. `videre watch --output-sqlite <path>` is now optional; it defaults to the resolved db.

---

## videre dedupe

```
videre dedupe [OPTIONS] <directory>
```

| Flag | Description |
|------|-------------|
| `--output-sqlite <path>` | Write results to SQLite (upserts by path on each run); with neither this nor `--output`, records go to the resolved default db (see [The ~/.videre home directory](#the-videre-home-directory)) |
| `--output [<path>]` | Write results to JSONL (appended on each run) instead of SQLite. A bare `--output` (no value) targets `~/.videre/hashes.jsonl` - it must come *after* the directory positional, or clap consumes the directory as the flag's value and fails with "required argument DIRECTORY" |
| `--similar` | Also find visually similar images via dHash perceptual hashing |
| `--silent` | Suppress progress on stderr (stdout paths are always written) |
| `--json` | Emit a single JSON object on stdout instead of text |

`--output` and `--output-sqlite` are mutually exclusive.

**stdout** receives REMOVE candidate paths, one per line - pipe directly into any deletion tool. The KEEP candidate in each group is the file with the oldest `exif_date`; falls back to `min(created_at, modified_at)` when EXIF is absent. `0000-*` EXIF dates (cameras with unset clocks) are treated as absent.

**stderr** shows scan progress and a summary. Suppressed by `--silent`.

```bash
videre dedupe ~/Photos                                        # preview removals, write to default db
videre dedupe ~/Photos | xargs trash                          # delete immediately
videre dedupe --silent ~/Photos > to_delete.txt                # save list for later
videre dedupe ~/Photos --output                                # write JSONL to ~/.videre/hashes.jsonl
videre dedupe --output-sqlite ~/photos.db ~/Photos             # write to an explicit db instead
videre dedupe --similar --output-sqlite ~/photos.db ~/Photos  # include visual duplicates
```

Visual duplicates use [dHash](http://www.hackerfactor.com/blog/index.php?/archives/529-Kind-of-Like-That.html): images are resized to 9x8 grayscale, adjacent pixel pairs produce a 64-bit fingerprint, and pairs with Hamming distance <= 10 are grouped as similar. Visual groups are logged to stderr only - review with `videre report` before deleting.

---

## videre report

Reads the SQLite database and generates a self-contained HTML file. There are two distinct phases where the report is useful.

**Phase 1: review before deleting.** Run immediately after `videre dedupe` to visually inspect duplicate groups and confirm KEEP/REMOVE decisions before touching any files.

```bash
videre report                          # reads the default db, output: <db>_report.html
videre report --db ~/photos.db         # explicit db
videre report -o out.html              # explicit output path
videre report --heic                   # embed HEIC thumbnails as JPEG (macOS only, requires qlmanage)
videre report --heic-original          # same + 1200px lightbox version
```

**Phase 2: browse after cleaning.** Run with `--all` once duplicates have been deleted. The report becomes a full gallery of your cleaned collection with in-page semantic search.

```bash
videre report --all
```

`--all` automatically skips files that were recorded in the database but no longer exist on disk, so the gallery always reflects the current state of your collection. Files are checked at report generation time; the database itself is not modified. Run `videre prune` to permanently clean up stale rows and sync metadata.

**Drill-down by date.** `--by-date` adds a static Year > Month > Day gallery over your KEEP files, generated the same way as `--all` (no server involved - it's plain HTML and can be combined with `--all`, `--heic`, and `--heic-original`).

```bash
videre report --by-date
```

**Live report with face and location metadata.** `--show-faces` switches `videre report` into server mode: it starts the same local server `--faces` uses (`localhost:7878`), but serves the interactive report (not the labeling UI) at `/`. The lightbox for each photo shows its labeled faces - click one to jump to `/person/<name>` - and a reverse-geocoded location name looked up on demand via a `/api/location` call and cached into the database for next time.

```bash
videre report --show-faces
```

Passing `--faces` and `--show-faces` together moves the report to `/` and the labeling UI to `/faces` (with `--faces` alone, `/` stays the labeling UI as before).

Thumbnails and the lightbox load differently depending on mode: static reports point at `file://` paths (the report itself is opened via `file://`, so that works fine), but `--show-faces` serves images/videos through `GET /api/raw?path=...` instead, since browsers block a `file://` subresource on an `http://`-served page. `/api/raw` only serves paths already known to the database.

The report includes:

- Stats header: files scanned always shown; duplicate groups/files/wasted-space tiles and the toolbar only appear when at least one duplicate group exists
- Toolbar: Expand all / Collapse all, sort by wasted space / date kept oldest-first / newest-first
- Duplicate groups with KEEP/REMOVE badges, image thumbnails, EXIF date, GPS map links, copy-path buttons
- Lightbox for full-size images and video playback (`.mov`, `.mp4`)
- `--all`: paginated gallery of every file on disk (200 per page) with a "Similar" button on each card that opens a results panel showing the top 24 cosine-similar images, computed client-side from SigLIP embeddings inlined in the page (requires a prior `videre embed` run)

In static mode, HEIC files show a "HEIC" placeholder by default; `--heic` embeds a 240px JPEG thumbnail via `qlmanage` (QuickLook, macOS only) - not `sips`, which silently skips the rotation some iPhone HEIC files need (see Platform notes). In server mode (`--show-faces`), HEIC always renders automatically instead - `--heic`/`--heic-original` have no effect there, since thumbnails convert lazily per request through `/api/raw`, checking `videre watch`'s pre-populated thumbnail cache first and only falling back to a live conversion on a cache miss, rather than all up front (which used to make server mode take minutes to load a single page on a collection with many HEIC files).

`--faces` starts a local web server on `localhost:7878` for interactive face labeling: color-coded People / Unassigned Clusters / Singletons sections, drag-and-drop assignment, a "New Person" form, per-cluster detail pages with a "Dissolve cluster" action for bad groupings, per-person detail pages, and click-to-view original photos. Labels are saved back to the `faces` table as `person_label`. Close the browser tab or press Ctrl-C to stop the server.

---

## videre faces

Detects faces in every image in the database, embeds each face with ArcFace, and clusters faces across images into identity groups. Run this after `videre embed` (or independently) to enable person search.

```bash
videre faces                              # process new hashes only (resumable), default db
videre faces --db <path>                  # explicit db
videre faces --reprocess                  # re-detect and re-embed all hashes
videre faces --recluster                  # skip detection; re-run clustering only
videre faces --dry-run                    # detect and embed but do not write to db
videre faces --batch <n>                  # images per ONNX batch (default: 8)
videre faces --silent                     # suppress per-image progress
videre faces --eps <f32>                  # DBSCAN cosine-distance radius (default: 0.6)
videre faces --min-cluster-size <n>       # minimum faces per cluster (default: 3)
```

Face detection uses InsightFace buffalo_l (SCRFD-10GF detector + ArcFace w600k_r50 embedder) via ONNX Runtime. Model weights are downloaded automatically on first run and cached in `~/.cache/ort/`. HEIC images are converted via `qlmanage`, matching the rest of the pipeline (see Platform notes).

Faces below `--min-cluster-size` stay as unassigned singletons instead of forming a tiny cluster. Use `--recluster --eps <value>` to retune clustering tightness without re-running detection.

**Faces workflow:**

```bash
videre dedupe ~/Photos                    # scan images into the default db
videre faces                              # detect + embed + cluster faces
videre report --faces                     # label in browser, save and close
videre search --person "Alice"            # find all photos of Alice
```

---

## videre watch

A background loop that keeps your database warm: rescans for new photos, detects faces on them, pre-converts HEIC thumbnails, and resolves GPS coordinates to place names - all on a timer, so `videre report --show-faces` never has to do this work on the fly. It's a simple foreground loop, not a daemon: run it in its own terminal or tmux pane, watch its progress on stderr, and stop it with Ctrl-C.

```bash
videre watch ~/Photos                                             # all four stages, default db, every 5 minutes
videre watch ~/Photos --interval 60                               # check every 60 seconds instead
videre watch ~/Photos --scan --faces                              # only rescan and detect faces
videre watch ~/Photos --silent                                    # quiet mode
videre watch --output-sqlite ~/photos.db ~/Photos                 # explicit db instead of the default
```

| Flag | Description |
|------|-------------|
| `--output-sqlite <path>` | Database to populate; defaults to the resolved db (see [The ~/.videre home directory](#the-videre-home-directory)) if omitted |
| `--scan` | Rescan the directory and update `file_hashes` (same as running `videre dedupe`) |
| `--faces` | Detect, embed, and cluster faces on any images not yet processed |
| `--heic` | Pre-convert and cache HEIC thumbnails (240px and 1200px) per photo |
| `--location` | Reverse-geocode any GPS coordinates not yet resolved to a place name |
| `--interval <seconds>` | Time between cycles (default: 300) |
| `--silent` | Suppress per-cycle progress output |

Pass none of the four stage flags and all four run every cycle - that's the intended default for "just keep my library up to date." Pass any subset to run only those stages.

Cached HEIC thumbnails land in `~/.cache/videre/thumbnails/`, keyed by the photo's content hash so the same file is never converted twice even across different databases. On first run, if the pre-rename cache at `~/.cache/dupe/thumbnails/` still exists and the new one doesn't, it's migrated automatically (a plain rename, so it's atomic and a no-op on error, since the cache regenerates lazily anyway). `videre report --show-faces` checks the cache before falling back to a live conversion, so running `videre watch --heic` in the background makes browsing HEIC-heavy libraries noticeably snappier.

`videre watch` and `videre report --show-faces` are safe to run at the same time against the same database file - both open it in SQLite's WAL mode, which allows concurrent readers and a writer without lock errors.

---

## videre prune

Syncs the database with the current state of the filesystem. Run this after deleting duplicates and fixing dates to keep the database consistent.

```bash
videre prune                 # apply all cleanup on the default db
videre prune --db <path>     # explicit db
videre prune --dry-run       # preview without modifying the database
videre prune --silent        # apply without per-file output
```

What it does in a single pass:

- **Removes stale rows**: deletes `file_hashes` rows for files that no longer exist on disk (e.g. duplicates that were trashed)
- **Syncs modified_at**: refreshes the `modified_at` column for surviving files from the current filesystem mtime - picks up changes made by `videre fix-dates` or any other tool
- **Cleans orphan embeddings**: deletes rows from `embeddings` whose hash has no remaining `file_hashes` entry

In dry-run mode, the orphan embedding count is a lower bound: it reflects only pre-existing orphans, not ones that would be created by the would-be row removals.

---

## videre embed and videre search

`videre embed` computes SigLIP embeddings (google/siglip-so400m-patch14-384, 1152-dim f16) for every image in the database and stores them keyed by content hash. Re-running only processes images not yet embedded. `.mov`, `.mp4`, and `.dng` files are skipped.

```bash
videre embed                        # embed all unprocessed images in the default db
videre embed --db <path>            # explicit db
videre embed --batch 64             # larger batch size (default: 32)
videre embed --silent               # suppress per-image output
```

**First run downloads ~1.8 GB of model weights from Hugging Face.** Weights are cached in `~/.cache/huggingface/` and reused on every subsequent run. If all images are already embedded, the command exits immediately without loading the model.

```bash
videre search "sunset on beach"                     # text query, default db
videre search --db <path> "sunset on beach"         # explicit db
videre search --image query.jpg                     # find images similar to an example
videre search "birthday cake" -k 10 --scores        # top 10 with cosine scores
videre search --person "Alice"                       # find all photos of Alice (requires videre faces)
```

| Flag | Description |
|------|-------------|
| `--json` | Emit a single JSON object on stdout instead of text |

`--scores` is a no-op under `--json`: the score is always included in each result.

On macOS, inference uses Metal (Apple Silicon GPU). On Linux, CPU only - embedding large collections will be significantly slower. CUDA support can be enabled by adding `features = ["cuda"]` to the candle dependencies in `crates/videre-ml/Cargo.toml`.

---

## videre fix-dates

Sets each file's `modified_at` timestamp to its EXIF shoot date, so Finder, sort-by-date views, and backup tools see the correct original capture time.

```bash
videre fix-dates --dry-run       # preview without changing anything, default db
videre fix-dates --db <path>     # explicit db
videre fix-dates                 # apply
videre fix-dates --silent        # apply without per-file output
```

Only files with `exif_date` in the database are touched. EXIF time is treated as local system time. Only `mtime` is updated (`created_at` / birth time is not changed). Files that no longer exist on disk are silently skipped and reported in the summary.

---

## JSON output (agentic use)

`videre search` and `videre dedupe` accept `--json`. With it, stdout is always exactly one
compact JSON object; progress stays on stderr (`--silent` suppresses it). Every document
starts with `"schema_version": 1`. On failure the object is
`{"schema_version":1,"error":{"message":"..."}}` and the exit code is nonzero, so callers can
always parse stdout first and then branch. `dedupe --json` reports exact duplicates as
`duplicate_groups` with a safe `keep`/`remove` split; with `--similar` it adds review-only
`similar_groups` (flat file clusters, no keep/remove: near-duplicates are not safe to
auto-delete). `search --json` returns per-path `results` with `hash` and `score` (omitted for
`--person` hits).

---

## Platform notes

| | macOS | Linux |
|-|-------|-------|
| `videre dedupe`, `videre report`, `videre fix-dates` | yes | yes |
| `videre embed`, `videre search` | yes (Metal GPU) | yes (CPU only) |
| `videre faces` | yes (CPU via ONNX Runtime) | yes (CPU via ONNX Runtime) |
| `videre watch` | yes | yes (`--heic` unavailable) |
| HEIC thumbnails/decoding (report, faces, embed, watch) | yes (via `qlmanage`) | no |
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

Re-scanning upserts existing rows by `path`. `phash` is only written with `--similar`. EXIF fields (`exif_date`, `gps_lat`, `gps_lon`, `width`, `height`) are written for jpg/jpeg/tiff/heic/dng files; null for all others. `location_name` is added by an idempotent migration on `videre report` startup and is not written by `videre dedupe` itself - it's populated lazily, one coordinate at a time, when `videre report --show-faces` (or `videre watch --location`) resolves and caches a reverse-geocoded location name.

Every command opens the database in SQLite's WAL journal mode, so `videre watch` and `videre report --show-faces` can safely read and write the same database file at the same time.

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

# Filter JSONL by extension (default JSONL path; use --output <path> for a custom one)
jq 'select(.ext == "heic")' ~/.videre/hashes.jsonl

# Wasted space from JSONL
jq -s 'group_by(.hash)|map(select(length>1))|map(.[0].size_bytes*(length-1))|add/1048576' ~/.videre/hashes.jsonl
```

## License

MIT
