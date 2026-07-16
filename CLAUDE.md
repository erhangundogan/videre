# videre

A fast Rust CLI tool for managing a local media library: duplicate detection, semantic search, and face recognition, all built around a single SQLite database.

## What it does

`videre` is a single binary with ten subcommands. `videre dedupe` scans a directory recursively, hashes every image file (BLAKE3), and writes REMOVE candidates to stdout one per line: ready to pipe into `trash` or `rm`. Bare `videre dedupe <dir>` writes SQLite to the resolved default database (see `~/.videre` below); JSONL output requires `--output`. `videre report` reads the SQLite database and generates an HTML review page (or serves a live web UI). The remaining subcommands (`fix-dates`, `prune`, `embed`, `search`, `faces`, `watch`) operate on the same SQLite database to fix timestamps, sync metadata, compute semantic embeddings, run text/image/person search, and detect/label faces. `videre config` shows or edits the resolved paths and `~/.videre/config.toml` settings. `videre mcp` serves read-only search/find_duplicates/stats tools over stdio for LLM agents.

Note: `docs/superpowers/` design specs and implementation plans predate the videre rename and refer to the old `dupe-*` binary names historically; they are not rewritten here.

## Usage

```
videre dedupe [OPTIONS] [directory]   # directory optional when 'path' is set in videre config

Options:
  --output [<path>]        JSONL output file (appended). Bare --output (no value) targets ~/.videre/hashes.jsonl; must come AFTER the directory positional, or clap consumes the directory as the flag's value. Mutually exclusive with --output-sqlite
  --output-sqlite <path>   SQLite output file; upserts by path; mutually exclusive with --output. When neither --output nor --output-sqlite is given, records go to the resolved default db (see ~/.videre below)
  --similar                Also find visually similar images (perceptual hash)
  --silent                 Suppress progress output on stderr (stdout paths are always written)
  --json                   Emit a single JSON object on stdout instead of human-readable text
```

`--output` and `--output-sqlite` cannot be used together: passing both is an error.

## Output behavior

- **stdout**: REMOVE candidate paths, one per line (pipe-ready)
- **stderr**: scan progress and summary (suppressed by `--silent`)

KEEP candidate within each group = oldest `exif_date`; falls back to `min(created_at, modified_at)` if absent. `exif_date` values of `0000-00-00T00:00:00` (cameras with unset clocks) are treated as absent.

With `--json`, stdout is instead one compact JSON object, always (an error object plus a nonzero exit code on failure), never the REMOVE-path lines above.

Bare `videre dedupe <dir>` writes SQLite to the resolved default database (no JSONL). JSONL output only happens when `--output` is passed, with or without a value.

## Build & run

```bash
cargo build --release
./target/release/videre dedupe ~/Photos                                  # preview removals, writes SQLite to the default db
./target/release/videre dedupe ~/Photos | xargs trash                    # delete duplicates
./target/release/videre dedupe --output-sqlite ~/photos.db ~/Photos      # scan to an explicit SQLite db
./target/release/videre report                                           # generate HTML report from the default db
./target/release/videre report --db ~/photos.db                          # generate HTML report from an explicit db
./target/release/videre fix-dates --dry-run                              # preview date fixes on the default db
./target/release/videre fix-dates                                        # apply date fixes
./target/release/videre prune --dry-run                                  # preview prune
./target/release/videre prune                                            # prune stale rows + sync metadata
./target/release/videre embed                                            # embed all images (resumable)
./target/release/videre search "sunset on beach"                         # text search
./target/release/videre search --image query.jpg                         # find similar images
./target/release/videre faces                                            # detect, embed, and cluster faces
./target/release/videre report --faces                                   # label faces in browser UI (localhost:7878)
./target/release/videre search --person "Alice"                          # find photos of Alice
./target/release/videre report --by-date                                 # static Year/Month/Day drill-down gallery
./target/release/videre report --show-faces                              # live report with face/location lightbox metadata
./target/release/videre watch ~/Photos                                   # background: scan + faces + HEIC cache + location, looping, default db
./target/release/videre watch --output-sqlite ~/photos.db ~/Photos       # same, against an explicit db
./target/release/videre config                                           # show resolved home dir, config.toml, and db paths
./target/release/videre config set db ~/photos.db                        # persist a default db in config.toml
./target/release/videre config set path ~/Photos                         # persist a default directory for dedupe/watch
./target/release/videre mcp                                              # serve MCP tools over stdio, default db
./target/release/videre mcp --db ~/photos.db                             # same, against an explicit db
```

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic` `.mp4` `.dng`

## ~/.videre home directory

Every subcommand shares a home directory at `~/.videre` (override with the `VIDERE_HOME` env var), created lazily by writers (`dedupe`, `watch`, `config set`) - readers never create it. It holds `hashes.db` (default SQLite database), `hashes.jsonl` (default JSONL output, only written when `--output` is used bare), and `config.toml` (optional overrides, currently just `default_db`).

Database resolution order for every subcommand: explicit path (`--db` on the seven readers - `report`, `fix-dates`, `prune`, `embed`, `search`, `faces`, `mcp`; `--output-sqlite` on the two writers - `dedupe`, `watch`) > `default_db` in `config.toml` > `~/.videre/hashes.db`. Readers never create a database; if the resolved path doesn't exist they print `no database found at <path>; run 'videre dedupe <dir>' first` and exit 1 (arrives as the JSON error object under `search --json`).

`videre config` shows the resolved home dir, `config.toml` path, the `db` and `path` settings (labeled by their settable keys, with a set-command hint when unset), resolved db, and jsonl path. `videre config set db <path>` writes an absolute path to `config.toml` as `default_db`; `videre config set path <dir>` writes `default_path`, which `videre dedupe` and `videre watch` use when their directory positional is omitted (no built-in fallback: without it, the directory is required). Both setters preserve any other keys already present; `videre config unset db|path` removes a key. `videre dedupe <dir>` also adopts `<dir>` as `default_path` automatically the first time it is run with no `default_path` already set (a one-time convenience for the common case of a single photo library); it prints a one-line stderr note when it does (suppressed by `--silent`), and never overwrites an already-configured `default_path` on later runs.

## Project structure

```
crates/
  videre/
    Cargo.toml
    src/main.rs
    src/commands/{mod.rs,dedupe.rs,report.rs,fix_dates.rs,prune.rs,embed.rs,search.rs,faces.rs,watch.rs,config.rs,mcp.rs}
    src/{lib.rs,scanner.rs,hasher.rs,output.rs,sqlite_output.rs,types.rs}
    tests/{integration.rs,report.rs,prune.rs,watch.rs,faces_pipeline.rs,faces_server.rs,person_search.rs,mcp.rs}
  videre-core/
    Cargo.toml
    src/lib.rs
    src/vectors.rs
    src/embeddings.rs
    src/face_db.rs
    src/face_cluster.rs
    src/person_search.rs
    src/db.rs
    src/heic.rs
    src/location.rs
    src/thumb_cache.rs
    src/home.rs
  videre-ml/
    Cargo.toml (lib-only, no binaries)
    src/lib.rs
    src/{device.rs,model.rs,preprocess.rs,search.rs,pipeline.rs}
    src/{face_models.rs,face_detect.rs,face_align.rs,face_embed.rs}
```

The `videre` crate builds a single `[[bin]]` (`videre`, from `src/main.rs`) plus a lib target (`src/lib.rs`) exposing `scanner`, `hasher`, `output`, `sqlite_output`, and `types` to both the binary and the integration tests under `tests/`. `main.rs` dispatches to one module per subcommand under `src/commands/`. `videre-core` holds shared SQLite/db/cache/search helpers used by both `videre` and `videre-ml`. `videre-ml` is lib-only: all inference logic lives there, but every user-facing entry point is a subcommand in `videre`.

## Key crates

- `clap`: CLI parsing (derive-based subcommands)
- `blake3`: fast exact hashing
- `rayon`: parallel hashing across CPU cores
- `walkdir`: recursive traversal
- `serde_json`: JSONL output
- `chrono`: date formatting
- `image`: image decoding and dHash perceptual hashing for `--similar` (implemented inline, no img_hash crate)
- `kamadak-exif`: EXIF metadata extraction (always on for jpg/jpeg/tiff/heic/dng)
- `rusqlite` (bundled): SQLite output for `--output-sqlite` and `videre report`
- `filetime`: set file `mtime` portably for `videre fix-dates`
- `candle-core` / `candle-nn` / `candle-transformers`: SigLIP inference, Metal on macOS
- `tokenizers`: text tokenization for SigLIP
- `hf-hub`: Hugging Face model weight downloads
- `half`: f16 storage for embeddings
- `ort`: ONNX Runtime bindings for face detection and embedding
- InsightFace buffalo_l: SCRFD-10GF face detector + ArcFace w600k_r50 embedder (ONNX weights, auto-downloaded to `~/.cache/ort/`)
- `rmcp`: official Rust MCP SDK, stdio server for `videre mcp`
- `schemars`: JSON-schema generation for MCP tool parameters

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
    height      INTEGER,
    location_name TEXT
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

Re-scanning the same folder with the same SQLite file upserts (overwrites) existing rows via `INSERT OR REPLACE`. `phash` is stored as signed `INTEGER` (cast from `u64`).

`faces` rows are keyed by `id` (auto-increment). `hash` links to `file_hashes`. `bbox` and `landmark` are JSON strings. `embedding` is a raw f32 BLOB (512-dim ArcFace). `cluster_id` is assigned by DBSCAN; `person_label` and `confirmed` are set via `videre report --faces`.

`location_name` is a nullable TEXT column added by an idempotent `ALTER TABLE file_hashes ADD COLUMN location_name TEXT` migration (run on every `videre report` startup; harmless if the column already exists) - it is not populated by the initial `videre dedupe` scan. It is populated lazily, one GPS coordinate at a time, by the `/api/location` endpoint when `--show-faces` is used: the first lightbox view of a photo at a given `(gps_lat, gps_lon)` triggers a reverse-geocode lookup, and the result is cached back into this column so later lookups for the same coordinate are free.

Every subcommand opens the database via `videre_core::db::open_wal`, which switches the connection to SQLite's WAL journal mode (`PRAGMA journal_mode = WAL`). WAL mode persists in the database file itself once set, so `open_wal` is idempotent - safe to call on every connection open, not just the first. This allows one writer plus many concurrent readers without "database is locked" errors, which matters now that `videre watch` can run in the background writing to the same file that a `videre report --show-faces` server has open for reading (and occasional writes, e.g. `/api/location`).

## EXIF fields

EXIF extraction runs automatically for `jpg`, `jpeg`, `tiff`, `heic`, and `dng` files. Fields are `null`/absent when the file has no EXIF data.

| Field | Type | Notes |
|-------|------|-------|
| `exif_date` | string | `DateTimeOriginal` formatted as `YYYY-MM-DDTHH:MM:SS`, camera-local time, no timezone; `0000-*` values from cameras with unset clocks are discarded (stored as null) |
| `gps_lat` | float | Decimal degrees, negative = South |
| `gps_lon` | float | Decimal degrees, negative = West |
| `width` | integer | From `PixelXDimension` |
| `height` | integer | From `PixelYDimension` |

## videre report

Reads `file_hashes` from a SQLite database and writes a self-contained HTML file. Two usage phases:

**Phase 1 (pre-deletion):** run without `--all` to review duplicate groups with KEEP/REMOVE badges before deleting anything.

**Phase 2 (post-deletion):** run with `--all` to browse the full cleaned collection with in-page similarity search. Files recorded in the database but no longer on disk are automatically excluded (checked at generation time; the database is not modified). `videre prune` removes stale rows permanently.

```bash
videre report                         # default db, output: <db>_report.html
videre report --db <db>               # explicit db
videre report -o <out>                # explicit output path
videre report --heic                  # embed HEIC thumbnails as base64 JPEG (macOS/qlmanage)
videre report --heic-original         # embed HEIC thumbnails + 1200px lightbox version
videre report --all                   # all-files gallery + in-page similarity search
videre report --faces                 # face labeling UI on localhost:7878 (requires videre faces)
videre report --by-date               # static Year/Month/Day drill-down gallery over KEEP files
videre report --show-faces            # live server: report with labeled-face + location metadata in the lightbox
```

`--by-date` is fully static: it writes an HTML file just like the default report or `--all` (same additive model - it can be combined with `--all`/`--heic`/`--heic-original`), grouping KEEP files into a clickable Year > Month > Day hierarchy. No server is involved.

`--show-faces` is different: it switches `videre report` into server mode (the same `axum` server on `localhost:7878` that `--faces` starts), because the lightbox now shows each photo's labeled faces (clicking one navigates to `/person/<name>`) and a reverse-geocoded location name, both of which need a live backend - labeled faces are queried from the `faces` table per request, and location names are resolved on demand via `/api/location` (see the `location_name` column below) rather than baked into a static file. Route split when combining with `--faces`:
- `--faces` alone: `/` serves the labeling UI (unchanged, no live report route).
- `--show-faces` alone: `/` serves the live report (with face/location metadata); no `/faces` route.
- `--faces --show-faces` together: `/` serves the live report, `/faces` serves the labeling UI.

Thumbnails and the lightbox also switch URL scheme in server mode: browsers refuse to load a `file://` subresource from an `http://`-served page, so `--show-faces` serves image/video bytes through `GET /api/raw?path=<path>` instead (a `LIVE_SERVER` flag baked into the page picks the URL scheme). `/api/raw` only serves paths already present in `file_hashes.path` - it's a deliberate allowlist, not a general file server. Static reports (no `--show-faces`) keep `file://` links, since the report itself is opened via `file://` there.

Report includes:

- Stats header (files scanned always shown; duplicate groups/files/wasted-space tiles and the toolbar only appear when there's at least one duplicate group)
- Toolbar: Expand all / Collapse all / Sort dropdown (wasted space, date kept oldest-first, date kept newest-first)
- Duplicate groups sorted by wasted space by default; sorting is instant DOM reorder
- Per-file: thumbnail preview, KEEP/REMOVE badge, filename, path + copy button, size, created, modified, EXIF date, GPS link, dimensions
- Image thumbnails via `file://` URL in static mode, or `/api/raw?path=...` in server mode (lazy-loaded, force-loaded on group expand)
- `.mov` and `.mp4` files shown as `<video>` thumbnail; click opens lightbox with playback controls
- `.heic` files: in static mode, "HEIC" text by default; `--heic` embeds a 240px JPEG thumbnail; `--heic-original` also embeds a 1200px lightbox version (macOS only, requires `qlmanage`, part of Quick Look/CoreServices). In server mode (`--show-faces`), HEIC always renders automatically - `--heic`/`--heic-original` are ignored there, since thumbnails are converted lazily per request via `/api/raw?path=...&size=N`, checking `videre watch`'s `~/.cache/videre/thumbnails/` cache first before falling back to a live `qlmanage` conversion (eagerly converting every HEIC file before responding made server mode take minutes on a collection with many HEIC files)
- Lightbox overlay for full-size image/video viewing; Escape or backdrop click closes
- `--all`: gallery of files that exist on disk (200-card pages, lazy thumbnails) + "Similar" button per file; click opens a results panel with top-24 cosine matches using inline SigLIP f16 embeddings (requires prior `videre embed` run)

HEIC conversion (`--heic`/`--heic-original`, face thumbnails, and the original-image
endpoint) uses `qlmanage -t` (QuickLook), not `sips -s format jpeg`. Some HEIC files
(notably iPhone photos where iOS encodes rotation via the HEIF `irot` transform box
rather than a classic EXIF Orientation tag) come out sideways with plain `sips`
conversion because it copies the raw sensor-buffer pixels unrotated; `qlmanage`
applies the same rotation Finder/Preview/Photos do. This affects `videre faces`
detection, `videre embed`/`videre search` preprocessing, and every HEIC thumbnail path
in `videre report` - all of them shell out to `qlmanage`, not `sips`, for this reason.

## videre fix-dates

Reads `file_hashes` from a SQLite database and sets `modified_at` on each file to its `exif_date`. Only files with `exif_date` present are touched. Operates on all such files (KEEP and REMOVE alike: REMOVE files will be deleted afterward anyway).

```bash
videre fix-dates                 # default db; apply: set mtime = exif_date for all files with EXIF
videre fix-dates --db <db>       # explicit db
videre fix-dates --dry-run       # preview without modifying anything
videre fix-dates --silent        # suppress per-file output (errors always shown)
```

- `exif_date` is camera-local time with no timezone; treated as local system time when computing the UNIX timestamp
- Only `modified_at` is set (`created_at` / birth time requires a macOS-only syscall and is not supported)
- Files that no longer exist on disk (e.g. trashed duplicates still in the DB) are silently skipped and reported in the summary as "no longer on disk (skipped)"
- Exits with code 1 if any file could not be updated (missing files are not counted as errors)

## videre prune

Syncs the SQLite database with the current filesystem state. Run after deleting duplicates and fixing dates.

```bash
videre prune                 # default db; apply
videre prune --db <db>       # explicit db
videre prune --dry-run       # preview without modifying the database
videre prune --silent        # apply without per-file output
```

In a single pass:
- Deletes `file_hashes` rows for files no longer on disk
- Refreshes `modified_at` for surviving files from their current filesystem mtime
- Deletes `embeddings` rows whose hash has no remaining `file_hashes` entry (orphan cleanup)

Shared-hash safety: if two paths share the same hash and one file is deleted, the embedding is only removed if no `file_hashes` row for that hash survives. Dry-run orphan count is a lower bound (pre-existing orphans only; does not account for orphans created by the would-be deletions). Exits with code 1 if any row update fails.

## videre embed / videre search

`videre embed` (optionally `--db <db>`) embeds every unique image hash (SigLIP so400m/14-384, 1152-dim,
L2-normalized f16 BLOB) into an `embeddings` table keyed by content hash. Resumable:
re-running processes only missing hashes. `--batch` (default 32), `--chunk` (rows per
transaction, default 500), `--silent`. HEIC via `qlmanage` (see videre report HEIC note
above); DNG, mov, and mp4 skipped (the `image` crate has no DNG decoder; EXIF metadata
is still available from the scan).

`videre search "query"` or `videre search --image photo.jpg` (optionally `--db <db>`) prints matching
paths to stdout (all duplicate paths per matched hash). `-k` top-k (default 20),
`--scores` prepends cosine score. Brute-force exact scan; no ANN index at this scale.
`videre search ... --json` emits a single JSON document (`schema_version`, `query`,
`count`, `results` with per-path `hash`/`score`; `--person` hits carry `path` only)
instead of the printed paths above; `--scores` is a no-op under `--json` since the
score is always included.

`videre search --person "Alice"` queries the `faces` table for confirmed rows whose `person_label` matches (case-insensitive prefix) and prints matching image paths. Requires a prior `videre faces` run and labels applied via `videre report --faces`.

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

## videre faces

Detects faces in every image recorded in the database, embeds each face with ArcFace, and clusters detected faces into identity groups using DBSCAN.

```
videre faces                            # default db; process new hashes only
videre faces --db <db>                  # explicit db
videre faces --reprocess                # re-detect and re-embed all hashes
videre faces --recluster                # skip detection; re-run DBSCAN on existing embeddings
videre faces --dry-run                  # detect and embed but do not write to db
videre faces --batch <n>                # images per ONNX batch (default: 8)
videre faces --silent                   # suppress per-image progress
videre faces --eps <f32>                # DBSCAN cosine-distance radius (default: 0.6)
videre faces --min-cluster-size <n>     # minimum faces per cluster (default: 3)
```

Uses InsightFace buffalo_l: SCRFD-10GF for detection, 5-point landmark alignment, ArcFace w600k_r50 for 512-dim L2-normalized embeddings. Weights are downloaded from `hf-hub` on first run. ONNX Runtime (`ort`) runs inference on CPU. HEIC images are converted via `qlmanage` (see videre report HEIC note above) before detection.

Faces below `--min-cluster-size` are left as unassigned singletons rather than forming
a small cluster. `--recluster` re-runs DBSCAN with new `--eps`/`--min-cluster-size`
values without re-detecting or re-embedding - useful for tuning cluster tightness
after an initial `videre faces` run.

`videre report --faces` starts an `axum` web server on `localhost:7878` serving a face-labeling UI:
- **People** (blue), **Unassigned Clusters** (green), **Singletons** (orange) sections, each color-coded consistently across cards, badges, and titles
- Drag a cluster/singleton card's handle onto a person card to assign it, or click "New Person" to create one
- Each unassigned cluster/singleton card links to a detail page (`/cluster/{id}` or via the card thumbnail) showing every face at full size with per-face remove/assign
- "Dissolve cluster" on the cluster detail page ungroups a wrongly-merged cluster back into singletons (faces are not deleted)
- Each person links to `/person/{name}`, listing their confirmed faces with per-face remove
- Click any face thumbnail to open the full-resolution original photo via `/api/original-image/{id}` (a live server request, not a `file://` link - browsers block navigating from `http://` to `file://` for security)
- Labels are written back to `faces.person_label` and `faces.confirmed`; close the browser tab or press Ctrl-C (or use the "Save & Close" button, which calls `/api/quit`) to stop the server

`videre search --person "Alice"` queries the `faces` table for confirmed rows with the given label and prints the paths of all matching images.

## videre watch

Long-running background process that keeps the pipeline populated so `videre report --show-faces` (or any other reader) always sees fresh data, without anyone manually re-running `videre dedupe`, `videre faces`, or waiting on lazy HEIC/location conversions. No server, no UI: it loops in the foreground, logging progress to stderr, until killed with Ctrl-C.

```bash
videre watch [directory]                                             # default db; all four stages, every 300s; directory optional when 'path' is set in videre config
videre watch <directory> --scan --faces                              # only these stages
videre watch <directory> --interval 60                                # custom cycle interval (seconds)
videre watch <directory> --silent                                    # suppress per-cycle stderr output
videre watch --output-sqlite <db> <directory>                        # explicit db instead of the default
```

Four independent stages, selected with `--scan` / `--faces` / `--heic` / `--location`. If none of the four flags are passed, all four run (the common case is "just keep everything up to date", not memorizing four flags):

- `--scan`: re-runs the same scan/hash/EXIF pipeline as `videre dedupe`, upserting `file_hashes` for the given directory
- `--faces`: incremental face detection - queries hashes not yet in the `faces` table, runs detection/embedding/clustering only on those, then re-runs DBSCAN clustering over all existing embeddings (same defaults as `videre faces`: `eps` 0.6, `min-cluster-size` 3)
- `--heic`: pre-converts and caches HEIC thumbnails (240px and 1200px) for every HEIC file's content hash, skipping hashes already cached; one `qlmanage` conversion per hash, downscaled in memory for each missing size rather than re-converting per size
- `--location`: reverse-geocodes every distinct `(gps_lat, gps_lon)` pair with `location_name IS NULL` and writes the result back to `file_hashes`, the same lookup `--show-faces`'s `/api/location` endpoint performs on demand

`--interval <seconds>` (default 300) is the sleep between cycles; each cycle runs the selected stages once, logs a per-stage summary to stderr (unless `--silent`), then sleeps. There's no daemonization or systemd unit - run it in a terminal, tmux/screen pane, or your own process supervisor, and stop it with Ctrl-C.

Thumbnails land in `~/.cache/videre/thumbnails/`, keyed by content hash rather than file path (`<hash>_240.jpg`, `<hash>_1200.jpg`) - mirrors the project's existing `~/.cache/ort/` convention for cached model weights, and means the same photo scanned into a different database only needs converting once. On first run of any `videre` subcommand, if the pre-rename cache at `~/.cache/dupe/thumbnails/` still exists and `~/.cache/videre/thumbnails/` doesn't, it's migrated automatically (a plain directory rename, atomic on the same filesystem, and a no-op on any error since the cache regenerates lazily). `videre report`'s `/api/raw?path=...&size=N` endpoint (server mode, `--show-faces`) checks this cache first for HEIC requests and serves the cached JPEG directly if present, falling back to a live `qlmanage` conversion otherwise - so running `videre watch --heic` alongside `videre report --show-faces` eliminates the per-request HEIC conversion cost for anything already warmed.

`videre watch` and `videre report --show-faces` are designed to run concurrently against the same SQLite file (see the WAL-mode note in the SQLite schema section above).

## videre mcp

Serves three read-only tools over stdio (line-delimited JSON-RPC, the standard MCP client transport) using the official `rmcp` SDK: `search` (text/person/image, same modes as `videre search`), `find_duplicates` (keep/remove groups, plus review-only similar clusters via `include_similar`), and `stats` (library summary, no params).

```bash
videre mcp                # default db
videre mcp --db <path>    # explicit db
```

Database resolution is identical to every other reader (`--db` > `default_db` in `config.toml` > `~/.videre/hashes.db`), but `mcp` binds the resolved path once at startup for the life of the process rather than per-invocation, so the resolved db must already exist - even an explicit `--db` to a nonexistent path fails at startup with `no database found at <path>; run 'videre dedupe <dir>' first` on stderr, nothing on stdout, exit 1.

Once serving, a failing tool call returns `isError: true` with the rendered anyhow error chain as the result text; the server itself stays alive and keeps serving subsequent calls. All three tools' result documents share `"schema_version": 1` with the CLI's `--json` output and reuse the same shapes (`duplicate_groups`/`keep`/`remove`, `similar_groups`, `results` with `hash`/`score`, omitted for person hits).

The SigLIP embedding model loads lazily on the first text/image search and stays cached in server memory for the life of the process, unlike the CLI which reloads it per invocation. Person search never touches the model.

Client configuration:

```json
{
  "mcpServers": {
    "videre": {
      "command": "/path/to/videre",
      "args": ["mcp"]
    }
  }
}
```

## Design specs

- `docs/superpowers/specs/2026-06-09-dupe-design.md` - core dedupe tool
- `docs/superpowers/specs/2026-06-10-exif-extraction-design.md` - EXIF metadata extraction
- `docs/superpowers/specs/2026-07-08-image-search-design.md` - semantic image search (dupe-embed, dupe-search; SigLIP + candle/Metal; Cargo workspace restructure)
- `docs/superpowers/specs/2026-07-09-report-search-design.md` - in-page similarity search in `dupe-report --all`
- `docs/superpowers/specs/2026-07-10-dupe-faces-design.md` - face detection, embedding, clustering, and labeling UI
- `docs/superpowers/specs/2026-07-12-date-grouping-design.md` - `--by-date` gallery and `--show-faces` lightbox metadata
- `docs/superpowers/specs/2026-07-13-dupe-watch-design.md` - background pipeline populator
