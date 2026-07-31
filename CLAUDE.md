# videre

A fast Rust CLI tool for managing a local media library: duplicate detection, semantic search, and face recognition, all built around a single SQLite database.

## What it does

`videre` is a single binary with thirteen subcommands. `videre scan` scans a directory
recursively, hashes every image file (BLAKE3), and writes the results into the database
(or JSONL with `--output`). `videre dedupe` reads that database and writes REMOVE
candidates to stdout one per line: ready to pipe into `trash` or `rm`. Bare `videre scan
<dir>` writes SQLite to the resolved default database (see `~/.videre` below); JSONL
output requires `--output`. `videre report` reads the SQLite database and generates an
HTML review page (or serves a live web UI). The remaining subcommands (`fix-dates`,
`prune`, `embed`, `search`, `faces`, `classify`, `watch`) operate on the same SQLite
database to fix timestamps, sync metadata, compute semantic embeddings, run
text/image/person/category search, detect/label faces, and classify images as
photo/screenshot/document/meme. `videre config` shows or edits the resolved paths and
`~/.videre/config.toml` settings. `videre mcp` serves read-only search/find_duplicates/
stats tools over stdio for LLM agents. `videre stats` prints library totals and
per-command pipeline run status (last run, success/failed/crashed, duration) in one shot.

Note: `docs/superpowers/` design specs and implementation plans predate the videre rename and refer to the old `dupe-*` binary names historically; they are not rewritten here.

## Usage

```
videre scan [OPTIONS] [directory]     # directory optional when 'path' is set in videre config

Options:
  --output [<path>]        JSONL output file (appended). Bare --output (no value) targets ~/.videre/hashes.jsonl; must come AFTER the directory positional, or clap consumes the directory as the flag's value. Mutually exclusive with --output-sqlite
  --output-sqlite <path>   SQLite output file; upserts by path; mutually exclusive with --output. When neither --output nor --output-sqlite is given, records go to the resolved default db (see ~/.videre below)
  --similar                Also compute and store perceptual hashes for near-duplicate detection
  --silent                 Suppress progress output on stderr
  --json                   Emit a single JSON object on stdout instead of human-readable text
```

`--output` and `--output-sqlite` cannot be used together: passing both is an error.

```
videre dedupe [OPTIONS]               # reads the database; no directory argument

Options:
  --db <path>   SQLite database (default: resolved from ~/.videre; see 'videre config')
  --similar     Also report perceptual-hash near-duplicate clusters (review-only)
  --silent      Suppress progress output on stderr (duplicate paths are always written to stdout)
  --json        Emit a single JSON object on stdout instead of human-readable text
```

## Output behavior

- **stdout**: REMOVE candidate paths, one per line (pipe-ready)
- **stderr**: scan progress and summary (suppressed by `--silent`)

KEEP candidate within each group = oldest `exif_date`; falls back to `min(created_at, modified_at)` if absent. `exif_date` values of `0000-00-00T00:00:00` (cameras with unset clocks) are treated as absent.

With `--json`, stdout is instead one compact JSON object, always (an error object plus a nonzero exit code on failure), never the REMOVE-path lines above.

Bare `videre scan <dir>` writes SQLite to the resolved default database (no JSONL). JSONL output only happens when `--output` is passed, with or without a value.

## Build & run

```bash
cargo build --release
./target/release/videre scan ~/Photos                                    # populate the default db
./target/release/videre dedupe | xargs trash                             # delete duplicates
./target/release/videre scan --output-sqlite ~/photos.db ~/Photos        # scan to an explicit SQLite db
./target/release/videre dedupe --db ~/photos.db                          # read from an explicit db
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
./target/release/videre classify                                         # classify images as photo/screenshot/document/meme
./target/release/videre search --category screenshot                     # find images classified as screenshots
./target/release/videre report --by-date                                 # static Year/Month/Day drill-down gallery
./target/release/videre report --show-faces                              # live report with face/location lightbox metadata
./target/release/videre watch ~/Photos                                   # background: scan + faces + HEIC cache + location, looping, default db
./target/release/videre watch --output-sqlite ~/photos.db ~/Photos       # same, against an explicit db
./target/release/videre config                                           # show resolved home dir, config.toml, and db paths
./target/release/videre config set db ~/photos.db                        # persist a default db in config.toml
./target/release/videre config set path ~/Photos                         # persist a default directory for dedupe/watch
./target/release/videre mcp                                              # serve MCP tools over stdio, default db
./target/release/videre mcp --db ~/photos.db                             # same, against an explicit db
./target/release/videre stats                                            # library totals + pipeline run status, default db
```

## Test coverage

`cargo-llvm-cov` (installed via `cargo install cargo-llvm-cov`, plus the
`llvm-tools-preview` rustup component) measures unit-test line/region/function
coverage across the workspace. It must be invoked through the rustup-managed
toolchain explicitly, not plain `cargo llvm-cov` - this machine's default
`cargo`/`rustc` on `PATH` are a separate Homebrew Rust install (no rustup
component support), while `llvm-tools-preview` only installs into a
rustup-managed toolchain; mixing the two would pair an LLVM-22 rustc with
LLVM-21 coverage tools and produce incompatible profile data.

```bash
rustup run stable-aarch64-apple-darwin cargo llvm-cov --workspace --summary-only   # per-file table
rustup run stable-aarch64-apple-darwin cargo llvm-cov --workspace --html           # HTML report at target/llvm-cov/html/index.html
```

Coverage only reflects code exercised by unit tests (`#[cfg(test)]` modules)
running in-process - integration tests under `crates/*/tests/` that spawn
`videre_bin()` as a child process (most CLI subcommand tests) are NOT
instrumented, so command modules like `fix_dates.rs`/`classify.rs`/`watch.rs`
show artificially low numbers here despite being covered by those integration
tests; read the per-file table as "unit-test coverage only", not overall test
coverage.

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic` `.mp4` `.dng`

## ~/.videre home directory

Every subcommand shares a home directory at `~/.videre` (override with the `VIDERE_HOME` env var), created lazily by writers (`scan`, `watch`, `config set`) - readers never create it. It holds `hashes.db` (default SQLite database), `hashes.jsonl` (default JSONL output, only written when `--output` is used bare), and `config.toml` (optional overrides, currently just `default_db`).

Database resolution order for every subcommand: explicit path (`--db` on the nine readers - `report`, `fix-dates`, `prune`, `embed`, `search`, `faces`, `classify`, `mcp`, `dedupe`; `--output-sqlite` on the two writers - `scan`, `watch`) > `default_db` in `config.toml` > `~/.videre/hashes.db`. Readers never create a database; if the resolved path doesn't exist they print `no database found at <path>; run 'videre scan <dir>' first` and exit 1 (arrives as the JSON error object under `search --json`).

`videre config` shows the resolved home dir, `config.toml` path, the `db` and `path` settings (labeled by their settable keys, with a set-command hint when unset), resolved db, and jsonl path. `videre config set db <path>` writes an absolute path to `config.toml` as `default_db`; `videre config set path <dir>` writes `default_path`, which `videre scan` and `videre watch` use when their directory positional is omitted (no built-in fallback: without it, the directory is required). Both setters preserve any other keys already present; `videre config unset db|path` removes a key. `videre scan <dir>` also adopts `<dir>` as `default_path` automatically the first time it is run with no `default_path` already set (a one-time convenience for the common case of a single photo library); it prints a one-line stderr note when it does (suppressed by `--silent`), and never overwrites an already-configured `default_path` on later runs.

## Project structure

```
crates/
  videre/
    Cargo.toml
    src/main.rs
    src/commands/{mod.rs,dedupe.rs,report.rs,scan.rs,fix_dates.rs,prune.rs,embed.rs,search.rs,faces.rs,classify.rs,watch.rs,config.rs,mcp.rs,stats.rs}
    src/{lib.rs,scanner.rs,hasher.rs,output.rs,sqlite_output.rs,types.rs}
    tests/{integration.rs,report.rs,prune.rs,watch.rs,faces_pipeline.rs,faces_server.rs,faces_resumability.rs,person_search.rs,mcp.rs,scan.rs,config.rs,embed.rs,fix_dates.rs,stats.rs,search.rs,fixtures/}
  videre-core/
    Cargo.toml
    src/lib.rs
    src/vectors.rs
    src/embeddings.rs
    src/classify.rs
    src/face_db.rs
    src/face_cluster.rs
    src/person_search.rs
    src/db.rs
    src/heic.rs
    src/location.rs
    src/thumb_cache.rs
    src/home.rs
    src/progress.rs
    src/library_stats.rs
    src/pipeline_runs.rs
    src/io_timeout.rs
    src/semaphore.rs
  videre-ml/
    Cargo.toml (lib-only, no binaries)
    src/lib.rs
    src/{device.rs,model.rs,preprocess.rs,search.rs,pipeline.rs,classify.rs}
    src/{face_models.rs,face_detect.rs,face_align.rs,face_embed.rs}
  videre-api/
    Cargo.toml (lib-only, no binaries)
    src/lib.rs
    src/{error.rs,types.rs,label.rs,faces.rs,images.rs,stats.rs,pipeline_status.rs}
```

The `videre` crate builds a single `[[bin]]` (`videre`, from `src/main.rs`) plus a lib target (`src/lib.rs`) exposing `scanner`, `hasher`, `output`, `sqlite_output`, and `types` to both the binary and the integration tests under `tests/`. `main.rs` dispatches to one module per subcommand under `src/commands/`. `videre-core` holds shared SQLite/db/cache/search helpers used by both `videre` and `videre-ml`. `videre-ml` is lib-only: all inference logic lives there, but every user-facing entry point is a subcommand in `videre`. `videre-api` is a lib-only facade over faces-labeling operations (list/assign/rename/dissolve/etc. plus face image bytes), called by the axum `--faces`/`--show-faces` server in `videre`; it's deliberately UI-agnostic so an external, separately-versioned UI client can depend on it directly without carrying any CLI/axum-specific logic along (see `architecture-multiplatform-ui` memory) - the closed-source `videre-desktop` app is one such consumer, but lives in its own private repo, not here.

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

CREATE TABLE IF NOT EXISTS classifications (
    hash          TEXT PRIMARY KEY NOT NULL,
    category      TEXT NOT NULL,
    confidence    REAL NOT NULL,
    classified_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS pipeline_runs (
    command      TEXT PRIMARY KEY,
    started_at   TEXT NOT NULL,
    finished_at  TEXT,
    status       TEXT NOT NULL,
    duration_ms  INTEGER,
    summary      TEXT
);
```

Re-scanning the same folder with the same SQLite file upserts (overwrites) existing rows via `INSERT OR REPLACE`. `phash` is stored as signed `INTEGER` (cast from `u64`).

`faces` rows are keyed by `id` (auto-increment). `hash` links to `file_hashes`. `bbox` and `landmark` are JSON strings. `embedding` is a raw f16 BLOB (512-dim ArcFace, 1024 bytes). `cluster_id` is assigned by the two-stage clustering (average-linkage, then a centroid-merge pass); `person_label` and `confirmed` are set via `videre report --faces`.

A companion `faces_scanned` table (`hash TEXT PRIMARY KEY, scanned_at TEXT`) records every hash that face detection has processed, **including images where zero faces were found** (which produce no `faces` row). This is what makes `videre faces` resumable - the skip set is "already scanned", so no-face images are detected once rather than every run. Created by `create_faces_table` alongside `faces`; written per hash as detection proceeds.

`pipeline_runs` holds one row per tracked command (`scan`, `faces`, `embed`, `classify`, `dedupe`, `fix-dates`, `prune` - `command` is the primary key, upserted on every run, not an append-only log). `status` is `running`/`success`/`failed`/`interrupted` as stored; a `crashed` status is never written to this column - it's computed only when reading (a `running` row whose per-db-per-command `flock` sidecar lock, `<db path>.<command>.lock`, isn't currently held by a live process is reported as `crashed` at read time). `videre watch` itself takes the same kind of lock (`<db>.watch.lock`) for liveness but has no row here, since it has no "finished" moment during normal operation. See `videre stats` below for how this is surfaced.

`classifications` is populated by `videre classify` (zero-shot photo/screenshot/document/meme classification, scoring `embeddings` rows already computed by `videre embed` against 4 fixed text prompts via cosine similarity - no new model, no image re-decoding) and queried via `videre search --category <name>`. Rows below the configurable `--margin` similarity gap between the best and second-best category are stored as `category = "unknown"` rather than a low-confidence guess.

`location_name` is a nullable TEXT column added by an idempotent `ALTER TABLE file_hashes ADD COLUMN location_name TEXT` migration (run on every `videre report` startup; harmless if the column already exists) - it is not populated by the initial `videre scan`. It is populated lazily, one GPS coordinate at a time, by the `/api/location` endpoint when `--show-faces` is used: the first lightbox view of a photo at a given `(gps_lat, gps_lon)` triggers a reverse-geocode lookup, and the result is cached back into this column so later lookups for the same coordinate are free.

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
videre fix-dates                 # default db; prompts for confirmation, then sets mtime = exif_date for all files with EXIF
videre fix-dates --db <db>       # explicit db
videre fix-dates --dry-run       # preview without modifying anything (never prompts)
videre fix-dates --yes           # skip the confirmation prompt (also: -y)
videre fix-dates --silent        # suppress per-file output (errors always shown; confirmation prompt is unaffected)
```

- `exif_date` is camera-local time with no timezone; treated as local system time when computing the UNIX timestamp
- Only `modified_at` is set (`created_at` / birth time requires a macOS-only syscall and is not supported)
- Files that no longer exist on disk (e.g. trashed duplicates still in the DB) are silently skipped and reported in the summary as "no longer on disk (skipped)"
- Exits with code 1 if any file could not be updated (missing files are not counted as errors)
- Before mutating anything, prints the count of files that will be touched and asks `[y/N]` on stderr; anything other than `y`/`yes` (including EOF, e.g. stdin piped from `/dev/null`) aborts with no changes and exit code 0. `--yes`/`-y` skips the prompt for scripted/non-interactive use. `--dry-run` never prompts, since it makes no changes regardless. The prompt is skipped entirely when there are zero files to update.

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
- Deletes `~/.cache/videre/thumbnails/` cache files (240/1200px thumbnails, face crops, full-res originals) whose hash has no remaining `file_hashes` entry (orphan cleanup) - this is the only bound on that cache's otherwise-unlimited growth (see the `videre faces`/`videre watch` HEIC-caching notes above); `.tmp*` scratch files from an in-flight write are never touched

Shared-hash safety (applies to both embeddings and cache files): if two paths share the same hash and one file is deleted, the embedding/cache entry is only removed if no `file_hashes` row for that hash survives. Dry-run orphan counts are a lower bound (pre-existing orphans only; does not account for orphans created by the would-be deletions). Exits with code 1 if any row update or cache-file removal fails.

`videre prune`'s runs are tracked in `pipeline_runs` (added 2026-08-01), visible via `videre stats`.

## videre embed / videre search

`videre embed` (optionally `--db <db>`) embeds every unique image hash (SigLIP so400m/14-384, 1152-dim,
L2-normalized f16 BLOB) into an `embeddings` table keyed by content hash. Resumable:
re-running processes only missing hashes. `--batch` (default 32), `--chunk` (rows per
transaction, default 500), `--silent`. HEIC via `qlmanage` (see videre report HEIC note
above); `.mov`/`.mp4` are embedded too, via one representative frame extracted the same
way (`qlmanage -t`, macOS only) rather than decoding the full video - a single-frame,
not-motion-aware embedding, so video search quality is weaker than photo search (see
`docs/superpowers/TECH_DEBT.md` for the open follow-ups on this). Video hashes are
excluded from `videre classify` (none of its four categories fit a video frame). DNG is
still skipped (the `image` crate has no DNG decoder) - excluded from `EMBEDDABLE_EXTS`
up front (fixed 2026-08-01; previously it was queried as pending and failed to decode
every single run), so a library with DNG files no longer wastes a decode attempt on
each of them every time `videre embed` runs. EXIF metadata is still available for DNG
files from the scan, independent of embedding support.

`videre search "query"` or `videre search --image photo.jpg` (optionally `--db <db>`) prints matching
paths to stdout (all duplicate paths per matched hash). `-k` top-k (default 20),
`--scores` prepends cosine score. Brute-force exact scan; no ANN index at this scale.
`videre search ... --json` emits a single JSON document (`schema_version`, `query`,
`count`, `results` with per-path `hash`/`score`; `--person` hits carry `path` only;
`--category` hits carry `path`+`hash` but no `score`, since it's set membership, not a ranked query)
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

## videre classify

`videre classify` (optionally `--db <db>`) classifies every embedded hash as `photo`,
`screenshot`, `document`, or `meme` via zero-shot classification: each stored embedding
is scored by cosine similarity against 4 fixed text prompts (`crates/videre-ml/src/classify.rs`'s
`CATEGORY_PROMPTS`), embedded once via the same SigLIP text tower `videre search` uses.
No new model, no image re-decoding - this runs entirely over vectors `videre embed`
already computed. Resumable: re-running only classifies hashes not yet in
`classifications`, unless `--reprocess`. `--margin` (default 0.05) is the min similarity
gap between the best and second-best category to accept a result; below that, the row
is stored as `unknown` rather than a low-confidence guess.

```
videre classify                     # classify all embedded-but-unclassified hashes, default db
videre classify --db <path>         # explicit db
videre classify --reprocess         # re-classify everything, including already-classified hashes
videre classify --silent            # suppress per-image progress
videre classify --margin <f32>      # min similarity gap to accept a category (default: 0.05)
```

`videre search --category <name>` queries the `classifications` table for rows matching
`category` and prints matching image paths (or, under `--json`, a `results` array with
`path`+`hash` per entry - no `score`, since this is set membership, not a ranked query).

## videre faces

Detects faces in every image recorded in the database, embeds each face with ArcFace, and clusters detected faces into identity groups using a two-stage pipeline: average-linkage agglomeration, then a centroid-merge pass that reunites one person's fragmented sub-clusters (see below).

```
videre faces                            # default db; scan not-yet-scanned images (resumable)
videre faces --db <db>                  # explicit db
videre faces --limit <n>                # scan at most N not-yet-scanned images, then stop (resumable; skips clustering)
videre faces --reprocess                # re-detect and re-embed all hashes
videre faces --recluster                # skip detection; re-run clustering on existing embeddings
videre faces --dry-run                  # detect and embed but do not write to db
videre faces --batch <n>                # images per ONNX batch (default: 8)
videre faces --silent                   # suppress per-image progress
videre faces --eps <f32>                # average-linkage cosine-distance radius (default: 0.6)
videre faces --min-cluster-size <n>     # minimum faces per cluster (default: 3)
videre faces --merge-sim <f32>          # centroid-merge similarity threshold (default: 0.35; 1 disables)
videre faces --min-face-size <px>       # min face bbox side (px) to cluster; smaller held out (default: 80; 0 disables)
videre faces --max-generic-sim <f32>    # distinctiveness gate: hold out faces too similar to the average face (default: 0.4; 1 disables)
videre faces --workers <n>              # worker threads for detection/embedding (default: 2x available core count)
videre faces --profile                  # print per-stage timing (load/detect/align/embed/db_write) after the run
videre faces --qlmanage-concurrency <n> # max concurrent qlmanage (HEIC decode) subprocesses, process-wide (default: 6)
```

Uses InsightFace buffalo_l: SCRFD-10GF for detection, 5-point landmark alignment, ArcFace w600k_r50 for 512-dim L2-normalized embeddings. Weights are downloaded from `hf-hub` on first run. ONNX Runtime (`ort`) runs inference on CPU (an explicit per-worker intra-op thread cap - see the concurrency note below; the macOS CoreML execution provider was measured to give no speedup for these models and is not used). HEIC images are converted via `qlmanage` (see videre report HEIC note above) before detection - unless a cached full-resolution decode already exists at `~/.cache/videre/thumbnails/<hash>_original.jpg` (written by `videre watch --heic`, or lazily by `videre report --show-faces`'s original-image endpoint), in which case that cached JPEG is read directly instead of paying for another `qlmanage` subprocess. Detection's bbox coordinates are stored relative to whatever image detection ran on, so this cache must be full resolution (not the 240/1200px thumbnail sizes) - `videre watch --heic` decodes at full resolution specifically to feed both this cache and its own thumbnails from one decode. Real measurement on a real library: ~108ms per cached HEIC load vs. ~7.6s for a live decode - roughly 70x faster once the cache is warm; a single-pixel bbox rounding difference (JPEG recompression noise) was observed in 1 of 20 checked coordinates against live-decode ground truth, not a correctness issue. Falls back to a fresh live decode when the cache hasn't been populated for a hash yet, so detection works correctly even if `videre watch --heic` has never run.

Detection is **resumable**. Every processed hash is recorded in a `faces_scanned` table - including images where zero faces were detected, which leave no `faces` row. The skip set for a run is "already scanned" (unioned with "already has faces", so a first run after upgrading doesn't redo prior work), not merely "has a face", so a no-face image is detected exactly once ever rather than re-detected on every run. Faces and the scanned marker are committed per hash as the run proceeds, so an interrupt (Ctrl-C) loses at most the in-flight image and a rerun continues where it left off. `--limit <n>` processes at most N not-yet-scanned images then stops (for chipping away at a large library in bounded chunks); a limited run skips the final clustering step (it is an O(n^2) whole-library pass not worth repeating after every chunk) - run `videre faces --recluster` once scanning is complete.

`videre faces` runs `--workers` worker threads concurrently (default: 2x the machine's available core count - see below for why), each with its own ONNX sessions (intra-op-thread-capped so they don't collectively oversubscribe the machine) processing a round-robin-assigned slice of the work - not contiguous chunks, so one worker doesn't inherit a disproportionately HEIC-heavy (slower) subset. All database writes happen on a single coordinator thread that receives results from workers over a channel; workers never touch the connection directly. The 2x-cores default (rather than a flat 1:1 mapping) comes from real profiling data: HEIC file loading (via a `qlmanage` subprocess) averaged ~52x longer than non-HEIC loading in one measurement, and since that wait is I/O-bound rather than CPU-bound, oversubscribing keeps cores busy with other workers' CPU-bound detect/embed work while some workers are blocked on the subprocess. A real A/B measurement on the full pipeline (not just the profiling estimate) found a ~3.23x wall-clock speedup with default workers vs. `--workers 1` on a 10-core machine. See docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md for the full design and why this approach (a symmetric worker pool) was chosen over a producer/consumer split with separate loader/inference pools. HEIC decoding itself is further capped independently of `--workers`: all `qlmanage` subprocess launches, across every subcommand, share one process-wide semaphore (`videre_core::heic::qlmanage_semaphore`) limiting concurrent conversions - 6 by default, raised from 3 after the 3.23x measurement above showed CPU sitting at only 477% of a possible 1000%, a hint that HEIC-heavy runs were bottlenecked on this cap rather than on cores. That hint was confirmed by a real re-measurement (same 300-image sample, default workers): `--qlmanage-concurrency 3` (the old default) ran in 75.10s at 498% CPU, `--qlmanage-concurrency 6` (the new default) ran in 60.86s at 663% CPU - a further ~1.23x wall-clock improvement on top of the earlier 3.23x, for a combined ~4.48x over the original fully-serial baseline (272.78s -> 60.86s). `videre faces --qlmanage-concurrency <n>` overrides the default for a single run. Measured 2026-07-31 (same 300-image sample/methodology): raising further to 8 or 10 only buys 1.3%/4.4% more wall-clock with diminishing returns (cap=6: 51.59s/728% CPU; cap=10: 49.42s/762% CPU) - CPU still isn't fully saturated even at cap=10, but per-image detect time crept up alongside it, meaning the extra concurrency mostly shifts the bottleneck into CPU contention rather than delivering free parallelism. Default stays 6; `--qlmanage-concurrency 10` remains available as a manual opt-in for a small win, not something worth changing the shipped default for.

Resumability's correctness is unchanged: workers never touch the database, so a hash can never end up marked scanned without its faces being durably written first, no matter how many workers are running - restart always correctly continues from the true set of completed hashes. What does change is how much gets re-done after a kill: even the single-threaded pipeline already defers marking a face-bearing image as scanned until its whole `batch`-sized chunk's single embed call resolves (only zero-face images are marked immediately), so an interrupt today can already cost up to `batch` (default 8) images of reprocessing, not 1. With `--workers` workers each independently chunk-batching their own partition, that window becomes up to `workers * batch` images - e.g. 160 with the defaults on a 10-core machine (20 workers x 8 batch). Still fully correct on resume, just a larger bounded "wasted work" window than before; `--limit` remains the lever for users who want tighter control per invocation.

Faces below `--min-cluster-size` are left as unassigned singletons rather than forming
a small cluster. `--recluster` re-runs clustering with new `--eps`/`--min-cluster-size`/`--merge-sim`/`--min-face-size`
values without re-detecting or re-embedding - useful for tuning cluster tightness
after an initial `videre faces` run.

Before clustering, a two-signal quality gate holds low-quality faces out of clustering
entirely (they come back as unassigned singletons, still visible and manually labelable).
Low-quality faces embed into near-degenerate ArcFace vectors that all point the same
generic direction regardless of who they are, so if clustered they pile into one large
*mixed* junk cluster (and then get centroid-merged into an even bigger one). A face is
held out when it fails **either** signal:

- **Size** (`--min-face-size`, default 80px): faces whose smaller bbox side is under this
  many pixels. Tiny crops (e.g. distant faces in group shots) upscale to ArcFace's 112px
  input as mostly blur. On real data, genuine person clusters are essentially all >100px
  per side while a degenerate junk cluster sat at ~60px median.
- **Distinctiveness** (`--max-generic-sim`, default 0.4): faces whose embedding cosine
  similarity to the population-average embedding exceeds this. Occluded (sunglasses,
  masks), non-frontal (profile), blurry, or false-positive detections (e.g. a carved
  statue face) carry little identity information, so ArcFace maps them close to the
  generic average. This catches low-quality faces the size gate misses (a large but
  sunglassed or profile face). On real data, 0.4 removed ~78% of a mixed junk cluster
  while touching 0% of confirmed real-person clusters; a genuinely distinctive person's
  own occluded shots survive because his many high-quality frontal photos anchor a
  centroid far from the generic average.

`--min-face-size 0` and `--max-generic-sim 1` each disable their signal. Residual junk
that slips through both gates (faces in the ambiguous overlap zone) is best cleared with
the labeling UI's "Dissolve cluster" button rather than by tightening the gates further,
which would start removing real faces.

Clustering is two-stage. First, average-linkage agglomeration groups faces by
average pairwise cosine distance (`--eps`). Average-linkage alone still fragments a
single person into several clusters, because one person's photos legitimately spread
wide in embedding space (pose, lighting, age) and the average cross-cluster distance
can exceed `--eps` even for the same identity. So a second centroid-merge pass then
joins any two clusters (each already at least `--min-cluster-size`) whose L2-normalized
mean embeddings are at least `--merge-sim` cosine-similar. Deciding on centroids rather
than raw pairs cancels per-face spread: on real data, confirmed *different* people never
exceed ~0.29 centroid similarity while one person's fragments run 0.37-0.76, so the 0.35
default reunites fragments with a safe margin. Only established clusters take part in the
merge, never lone singletons - a single bad crop can sit within `--merge-sim` of a
different person's centroid, whereas a whole cluster's averaged centroid cannot.

Clustering runs after every full (non-`--limit`, non-`--dry-run`) `videre faces`
invocation, and every `videre watch --faces` cycle, with no progress output of its own
before 2026-07-29 - on a real library with tens of thousands of faces this looked
identical to a hang (the detection progress bar clears, then nothing prints until the
whole pass finishes). The average-linkage stage is O(n^2) in the number of faces that
pass the quality gate: it now prints `Clustering N face(s) (eps=X)...` and ticks a
progress bar over the O(n^2) pairwise-distance stage, so a long clustering pass is
visibly progressing rather than silent. The initial candidate-merge heap is now seeded
only with pairs already within `--eps` (not every one of the `n*(n-1)/2` pairs
unconditionally) - correctness-preserving, since a pair currently outside `--eps` that
later becomes eligible via a merge is still picked up by the existing distance-update
step, which reads the dense distance matrix directly rather than the heap (see
`one_bad_pair_does_not_block_an_otherwise_strong_merge` in `face_cluster.rs`, which
specifically exercises this). Before this fix, the heap's unconditional
`with_capacity(n*(n-1)/2)` preallocation alone could demand tens of GB on a real
library (~41GB at n=58,555, more than many machines have) before any clustering work
began; it now scales with the number of eps-eligible pairs instead.

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

Long-running background process that keeps the pipeline populated so `videre report --show-faces` (or any other reader) always sees fresh data, without anyone manually re-running `videre scan`, `videre faces`, or waiting on lazy HEIC/location conversions. No server, no UI: it loops in the foreground, logging progress to stderr, until killed with Ctrl-C.

```bash
videre watch [directory]                                             # default db; original four stages, every 300s; directory optional when 'path' is set in videre config
videre watch <directory> --scan --faces                              # only these stages
videre watch <directory> --interval 60                                # custom cycle interval (seconds)
videre watch <directory> --silent                                    # suppress per-cycle stderr output
videre watch --output-sqlite <db> <directory>                        # explicit db instead of the default
videre watch <directory> --prune                                     # opt-in: also reclaim stale rows/cache each cycle
```

Five independent stages, selected with `--scan` / `--faces` / `--heic` / `--location` / `--prune`. If none of `--scan`/`--faces`/`--heic`/`--location` are passed, all four of those run (the common case is "just keep everything up to date", not memorizing four flags) - `--prune` is the exception, opt-in only, and never defaults on even when no stage flags are passed at all (added 2026-08-01; kept out of the default set so existing `videre watch` invocations don't change behavior):

- `--scan`: re-runs the same scan/hash/EXIF pipeline as `videre scan`, upserting `file_hashes` for the given directory
- `--faces`: incremental face detection - queries hashes not yet in the `faces` table, runs detection/embedding/clustering only on those, then re-runs the two-stage clustering (average-linkage + centroid-merge, with the same size + distinctiveness quality gate) over all existing embeddings (same defaults as `videre faces`: `eps` 0.6, `min-cluster-size` 3, `merge-sim` 0.35, `min-face-size` 80, `max-generic-sim` 0.4)
- `--heic`: pre-converts and caches HEIC thumbnails (240px and 1200px) for every HEIC file's content hash, skipping hashes already cached; one full-resolution `qlmanage` conversion per hash, downscaled in memory for each missing size rather than re-converting per size. That same full-resolution decode is also cached as `<hash>_original.jpg` (skipped if already present) - `videre faces` reads this cache instead of running its own `qlmanage` decode when detecting faces on a HEIC file, so running `--heic` ahead of (or alongside) `videre faces`/`--faces` avoids a second full decode per HEIC file. Real measurement: ~108ms to read the cache vs. ~7.6s for a live decode. This full-res cache has a real disk cost at library scale (tens of GB for a HEIC-heavy library) not yet gated behind any size limit or flag.
- `--location`: reverse-geocodes every distinct `(gps_lat, gps_lon)` pair with `location_name IS NULL` and writes the result back to `file_hashes`, the same lookup `--show-faces`'s `/api/location` endpoint performs on demand
- `--prune`: runs the same cleanup as `videre prune` (stale `file_hashes` row removal, `modified_at` sync, orphan embedding/cache cleanup) against the already-open connection, via `PruneArgs::for_watch_stage` and the shared `run_prune` helper - never deletes real files, only stale db rows and cache entries for files already gone from disk. Its runs are tracked in `pipeline_runs` under `"prune"`, same as a standalone `videre prune` invocation.

`--interval <seconds>` (default 300) is the sleep between cycles; each cycle runs the selected stages once, logs a per-stage summary to stderr (unless `--silent`), then sleeps. There's no daemonization or systemd unit - run it in a terminal, tmux/screen pane, or your own process supervisor, and stop it with Ctrl-C.

Thumbnails land in `~/.cache/videre/thumbnails/`, keyed by content hash rather than file path (`<hash>_240.jpg`, `<hash>_1200.jpg`) - mirrors the project's existing `~/.cache/ort/` convention for cached model weights, and means the same photo scanned into a different database only needs converting once. On first run of any `videre` subcommand, if the pre-rename cache at `~/.cache/dupe/thumbnails/` still exists and `~/.cache/videre/thumbnails/` doesn't, it's migrated automatically (a plain directory rename, atomic on the same filesystem, and a no-op on any error since the cache regenerates lazily). `videre report`'s `/api/raw?path=...&size=N` endpoint (server mode, `--show-faces`) checks this cache first for HEIC requests and serves the cached JPEG directly if present, falling back to a live `qlmanage` conversion otherwise - so running `videre watch --heic` alongside `videre report --show-faces` eliminates the per-request HEIC conversion cost for anything already warmed.

`videre watch` and `videre report --show-faces` are designed to run concurrently against the same SQLite file (see the WAL-mode note in the SQLite schema section above).

## videre mcp

Serves three read-only tools over stdio (line-delimited JSON-RPC, the standard MCP client transport) using the official `rmcp` SDK: `search` (text/person/image - a subset of `videre search`'s modes; `--category` is CLI-only, not exposed here), `find_duplicates` (keep/remove groups, plus review-only similar clusters via `include_similar`), and `stats` (library summary, no params).

```bash
videre mcp                # default db
videre mcp --db <path>    # explicit db
```

Database resolution is identical to every other reader (`--db` > `default_db` in `config.toml` > `~/.videre/hashes.db`), but `mcp` binds the resolved path once at startup for the life of the process rather than per-invocation, so the resolved db must already exist - even an explicit `--db` to a nonexistent path fails at startup with `no database found at <path>; run 'videre scan <dir>' first` on stderr, nothing on stdout, exit 1.

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

## videre stats

Prints library totals and per-command pipeline run status in one shot - a CLI
window into `videre-core`'s `library_stats` and `pipeline_runs` modules.

```bash
videre stats                # default db
videre stats --db <path>    # explicit db
videre stats --json         # single JSON object instead of text
videre stats --check        # exit nonzero if any tracked command last failed or crashed
```

Text mode prints library totals (files/size, photo/video split, duplicate
groups/files/wasted space, faces detected/people named), then one line per
tracked command (`scan`, `faces`, `embed`, `classify`, `dedupe`, `fix-dates`,
`prune`) showing its last-run timestamp, status, and duration - `never run` /
`-` for a command that hasn't executed against this db yet, and `(running
now)` appended when its lock is currently held by a live process. Uses
`resolve_reader_db_must_exist` like `dedupe`/`mcp` (not `resolve_reader_db`
like `embed`/`classify`), so an explicit `--db` to a nonexistent path fails
cleanly rather than silently creating an empty database.

`--json` emits `{"schema_version": 1, "library": {...}, "pipelines": [...]}`,
directly reusing `videre-core`'s `LibraryStats` and `PipelineRunStatus` serde
types rather than redeclaring their fields - the `pipelines` array always has
exactly seven entries (`videre_core::pipeline_runs::TRACKED_COMMANDS`) in a
fixed command order, with `status`/`last_run_at`/`duration_ms` all `null` for
a command that has never run. `report`, `search`, `mcp`, and `config` are
deliberately not tracked here - see `TRACKED_COMMANDS`'s doc comment for why
each was left out.

`--check` (added 2026-08-01) doesn't change either output format - it only
adds an exit code, via `has_problem()` checking whether any tracked command's
last recorded status is `"failed"` or `"crashed"` (a clean `"interrupted"`
Ctrl-C is deliberately not treated as a problem). Composes with both text and
`--json` mode, so `videre stats --check` (or `--json --check`) can drive
cron/launchd failure handling without parsing either output.

Per-item errors within a run (a few unreadable files, one corrupted image) do
not mark a `pipeline_runs` row `failed` - only an unhandled exception during
the run does. `fix-dates`/`faces` can legitimately exit nonzero (bad EXIF
dates, detection failures) while still recording `status: "success"`, since
`track()` only observes the operation's returned `Result`, and both commands
return `Ok` with an error count rather than propagating those as `Err`.

## UI / desktop app

There is no desktop or web UI in this repository. The only user-facing UI is
`videre report`'s HTML output (static reports and `--faces`/`--show-faces`
server mode) - see the `videre report` section above. A closed-source desktop
app (`videre-desktop`) and a closed-source web app (`videre-web`) exist as
separate private repositories, both consuming `videre-core`/`videre-api`
directly as external dependencies rather than duplicating any of this
project's logic. `videre-core`/`videre-api` are kept deliberately UI-agnostic
for exactly this reason - see `architecture-multiplatform-ui` memory.
