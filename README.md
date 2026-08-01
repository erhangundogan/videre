# videre

A local-first media management CLI, offering:

- file scanning (photos and videos)
- duplicate elimination
- comprehensive HTML reports
- EXIF-based date fixing
- semantic embedding and search, for images and video (single-frame)
- zero-shot classification (photo/screenshot/document/meme)
- face detection, clustering, and search
- a background watch loop that keeps everything fresh automatically
- library stats and pipeline run status
- agentic access via an MCP server

Everything runs over a single shared SQLite database.

## Why videre

Most media managers want to *own* your library: import into their storage, index into a database only they can read, and increasingly nudge you toward their cloud. videre works the other way - it's a lens over a folder you already own. Aside from `fix-dates` optionally correcting a file's mtime, nothing it does ever moves, copies, or re-encodes your files. Point it at a directory, get a SQLite database of what's there, and every other command works from that database. Stop running it and nothing changes about your files.

Three things it's built to do especially well:

- **Duplicate detection you can trust before you delete anything.** `videre dedupe` never deletes on its own - it prints REMOVE candidates for you to pipe into `trash` (or review first in `videre report`'s HTML gallery, KEEP/REMOVE badges and all). Exact BLAKE3 matches plus optional perceptual-hash near-duplicates, always review-first.
- **Face recognition that clusters for you, not the other way around.** `videre faces` detects, embeds, and automatically clusters faces into identity groups (two-stage average-linkage + centroid-merge, with a quality gate that keeps blurry/occluded/generic faces out of the noise) - so labeling is "name this cluster of 40 photos" instead of tagging one photo at a time.
- **EXIF-based date repair that fixes the actual files.** `videre fix-dates` sets each file's real mtime from its camera's `DateTimeOriginal`, not just a sort order inside some app's own index - so the fix survives moving the files anywhere else, forever.

And because it's a CLI over a plain SQLite file rather than a server you log into, it's the one media tool an LLM agent can drive directly today - `videre mcp` exposes search, duplicate review, and stats as agent tools with zero new infrastructure.

## Subcommands

| Subcommand | Purpose |
|------------|---------|
| `videre scan` | Scan a directory, hash every image, and populate the database |
| `videre dedupe` | Report duplicate files from the database, print paths to remove |
| `videre report` | Read the SQLite database, generate an HTML review page (or serve the live report/labeling UI) |
| `videre fix-dates` | Set each file's mtime to its EXIF shoot date |
| `videre prune` | Remove stale rows, sync metadata, clean orphan embeddings |
| `videre embed` | Compute SigLIP embeddings for every image in the database |
| `videre search` | Search images by text description, example image, person name, or category |
| `videre faces` | Detect, embed, and cluster faces; enables person search |
| `videre classify` | Classify images as photo/screenshot/document/meme (zero-shot, reuses embeddings) |
| `videre watch` | Background loop that keeps scan/faces/HEIC-cache/location data fresh |
| `videre config` | Show or edit videre's config and default paths (`~/.videre`) |
| `videre mcp` | Serve read-only MCP tools for LLM agents over stdio |
| `videre stats` | Show library totals and per-command pipeline run status |

## Supported file types

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.mov` `.heic` `.mp4` `.dng`

## Install

```bash
git clone git@github.com:erhangundogan/videre.git
cd videre
cargo build --release
```

The single binary lands at `./target/release/videre`.

A `Makefile` wraps the common commands - run `make` (or `make help`) to list
them: `make build`/`make build-dev`, `make test`, `make fmt`/`make lint`,
`make coverage`/`make coverage-html`, and `make install` (installs to
`~/.cargo/bin` via `cargo install`).

## Quickstart

All commands below use the default database at `~/.videre/hashes.db`, created automatically
on first write. Pass `--db <path>` (readers) or `--output-sqlite <path>` (writers) to point
at a different file instead - see [The ~/.videre home directory](#the-videre-home-directory).

```bash
# 1. Scan - everything written to the default SQLite db
videre scan ~/Photos

# 2. Preview duplicates - printed to stdout
# If you don't wanna review duplicates visually then you can start from point 4
videre dedupe

# 3. Review - open the HTML report in your browser
videre report

# 4. Delete duplicates
videre dedupe | xargs trash

# 5. Prune the database: remove stale rows for the files just deleted, sync
# metadata, clean orphan embeddings - do this before the steps below so they
# never waste time on rows for files that no longer exist
videre prune

# 6. Fix timestamps - set mtime = EXIF shoot date on remaining files
videre fix-dates

# 7. Embed images for semantic search (downloads ~1.8 GB model on first run)
videre embed

# 8. Search by text or example image
videre search "golden gate bridge at sunset"
videre search --image reference.jpg

# 9. Classify images as photo/screenshot/document/meme, then find screenshots
videre classify
videre search --category screenshot

# 10. Detect, embed, and cluster faces for person search
videre faces

# 11. Label faces in the browser UI, then save and close
videre report --faces

# 12. Find all photos of a named person
videre search --person "Alice"

# 13. Browse the full collection with in-page similarity search
videre report --all

# 14. Browse a Year/Month/Day drill-down gallery (static HTML, same as --all)
videre report --by-date

# 15. Live report with labeled-face and location metadata in the lightbox
videre report --show-faces

# 16. Keep everything fresh in the background (run alongside step 15, same db)
videre watch ~/Photos
```

To use an explicit database file instead of the default:

```bash
videre scan --output-sqlite ~/photos.db ~/Photos
videre dedupe --db ~/photos.db
videre report --db ~/photos.db
videre search --db ~/photos.db "golden gate bridge at sunset"
videre classify --db ~/photos.db
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

The directory and its files are created lazily by writers (`scan`, `watch`, `config set`) -
nothing is written just by running a reader.

**Database resolution order**, used by every subcommand that reads or writes SQLite:

1. An explicit path: `--db <path>` on readers (`report`, `fix-dates`, `prune`, `embed`,
   `search`, `faces`, `mcp`, `dedupe`), `--output-sqlite <path>` on writers (`scan`, `watch`)
2. `default_db` in `~/.videre/config.toml`, if set
3. `~/.videre/hashes.db`

Readers never create a database. If the resolved path doesn't exist, they print:

```
no database found at <path>; run 'videre scan <dir>' first
```

and exit 1 (under `search --json` this arrives as the JSON error object instead).

**`videre config`** shows the resolved paths and current settings:

```bash
videre config                        # show home dir, config.toml path, db setting, resolved db, jsonl path
videre config set db ~/photos.db     # persist default_db (written as an absolute path)
videre config set path ~/Photos      # persist default_path: scan/watch use it when the directory is omitted
# 'videre scan <dir>' also does this automatically the first time (when no path is set yet); it prints a note unless --silent
videre config unset path             # remove it; scan/watch require an explicit directory again
videre config unset db               # remove default_db, falling back to ~/.videre/hashes.db
```

`config set`/`config unset` preserve any other keys already in `config.toml`.

---

## videre scan

```
videre scan [OPTIONS] [directory]   # directory optional when 'path' is set in videre config
```

| Flag | Description |
|------|-------------|
| `--output-sqlite <path>` | Write results to SQLite (upserts by path on each run); with neither this nor `--output`, records go to the resolved default db (see [The ~/.videre home directory](#the-videre-home-directory)) |
| `--output [<path>]` | Write results to JSONL (appended on each run) instead of SQLite. A bare `--output` (no value) targets `~/.videre/hashes.jsonl` - it must come *after* the directory positional, or clap consumes the directory as the flag's value and fails with "required argument DIRECTORY" |
| `--similar` | Also compute and store perceptual hashes for near-duplicate detection |
| `--silent` | Suppress progress on stderr |
| `--json` | Emit a single JSON object on stdout instead of text |

`--output` and `--output-sqlite` are mutually exclusive.

**stdout** is empty in text mode: scanning only populates the database (or JSONL file), it doesn't report duplicates - that's `videre dedupe`'s job.

**stderr** shows scan progress and a summary. Suppressed by `--silent`.

```bash
videre scan ~/Photos                                          # populate the default db
videre scan --silent ~/Photos                                 # quiet mode
videre scan ~/Photos --output                                 # write JSONL to ~/.videre/hashes.jsonl
videre scan --output-sqlite ~/photos.db ~/Photos              # write to an explicit db instead
videre scan --similar --output-sqlite ~/photos.db ~/Photos    # also compute perceptual hashes
```

---

## videre dedupe

```
videre dedupe [OPTIONS]   # reads the database; no directory argument
```

| Flag | Description |
|------|-------------|
| `--db <path>` | SQLite database to read (default: resolved from `~/.videre`; see [The ~/.videre home directory](#the-videre-home-directory)) |
| `--similar` | Also report perceptual-hash near-duplicate clusters (review-only) |
| `--silent` | Suppress progress on stderr (stdout paths are always written) |
| `--json` | Emit a single JSON object on stdout instead of text |

**stdout** receives REMOVE candidate paths, one per line - pipe directly into any deletion tool. The KEEP candidate in each group is the file with the oldest `exif_date`; falls back to `min(created_at, modified_at)` when EXIF is absent. `0000-*` EXIF dates (cameras with unset clocks) are treated as absent.

**stderr** shows a summary. Suppressed by `--silent`.

```bash
videre scan ~/Photos                                          # populate the default db
videre dedupe                                                 # preview removals
videre dedupe | xargs trash                                   # delete immediately
videre dedupe --silent > to_delete.txt                        # save list for later
videre scan --output ~/Photos                                 # write JSONL to ~/.videre/hashes.jsonl
videre scan --output-sqlite ~/photos.db ~/Photos              # scan to an explicit db
videre dedupe --db ~/photos.db --similar                      # explicit db, include visual duplicates
```

Visual duplicates use [dHash](http://www.hackerfactor.com/blog/index.php?/archives/529-Kind-of-Like-That.html): images are resized to 9x8 grayscale, adjacent pixel pairs produce a 64-bit fingerprint, and pairs with Hamming distance <= 10 are grouped as similar. Visual groups are logged to stderr only - review with `videre report` before deleting.

---

## videre report

Reads the SQLite database and generates a self-contained HTML file. There are two distinct phases where the report is useful.

**Phase 1: review before deleting.** Run `videre scan` then `videre dedupe` to visually inspect duplicate groups and confirm KEEP/REMOVE decisions before touching any files.

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

**Live report with face and location metadata.** `--show-faces` serves the report from a local server (`localhost:7878`) instead of a static file, so the lightbox can show labeled faces and reverse-geocoded locations on demand.

```bash
videre report --show-faces
```

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

This can be a **long-running process** - detection time scales with how many images are in your library, not a fixed cost. Non-HEIC images process in well under a second each, but HEIC files decode through a `qlmanage` subprocess that can take several seconds per file, so a library with a substantial HEIC share can take anywhere from minutes to hours depending on size. It's fully **resumable**: every processed image (including ones where no face was found) is recorded as scanned, so interrupting with Ctrl-C and re-running later continues exactly where it left off rather than starting over. Use `--limit <n>` to deliberately process the library in bounded chunks instead of one long run.

```bash
videre faces                              # process new hashes only (resumable), default db
videre faces --db <path>                  # explicit db
videre faces --reprocess                  # re-detect and re-embed all hashes
videre faces --recluster                  # skip detection; re-run clustering only
videre faces --dry-run                    # detect and embed but do not write to db
videre faces --limit <n>                  # process at most N not-yet-scanned images, then stop
videre faces --batch <n>                  # images per ONNX batch (default: 8)
videre faces --silent                     # suppress per-image progress
videre faces --eps <f32>                  # average-linkage cosine-distance radius (default: 0.6)
videre faces --min-cluster-size <n>       # minimum faces per cluster (default: 3)
videre faces --merge-sim <f32>            # centroid-merge similarity threshold (default: 0.35; 1 disables)
videre faces --min-face-size <px>         # min face bbox side (px) to cluster (default: 80; 0 disables)
videre faces --max-generic-sim <f32>      # distinctiveness gate (default: 0.4; 1 disables)
videre faces --workers <n>                # worker threads for detection/embedding (default: 2x available core count)
videre faces --profile                    # print per-stage timing (load/detect/align/embed/db_write) after the run
videre faces --qlmanage-concurrency <n>   # max concurrent qlmanage (HEIC decode) subprocesses, process-wide (default: 6)
```

Face detection uses InsightFace buffalo_l (SCRFD-10GF detector + ArcFace w600k_r50 embedder) via ONNX Runtime. Model weights are downloaded automatically on first run and cached in `~/.cache/ort/`. HEIC images are converted via `qlmanage`, matching the rest of the pipeline (see Platform notes) - unless a full-resolution decode is already cached at `~/.cache/videre/thumbnails/<hash>_original.jpg` (written by `videre watch --heic`), in which case that cached JPEG is read directly instead of paying for another `qlmanage` subprocess (~108ms vs. ~7.6s per file in one real measurement). Falls back to a fresh live decode when the cache hasn't been populated yet, so detection works correctly even if `watch --heic` has never run.

Detection runs on multiple worker threads by default (`--workers`, 2x your machine's core count), each with its own ONNX sessions, processing a round-robin-assigned slice of the work so one worker doesn't inherit a disproportionately HEIC-heavy (slower) subset. A real measurement on a 10-core machine found this gives a ~3.23x wall-clock speedup over running single-threaded. HEIC decoding is further bounded independently of `--workers` by a process-wide cap on concurrent `qlmanage` subprocesses (`--qlmanage-concurrency`, default 6, raised from an earlier default of 3) - QuickLook's thumbnail agent doesn't scale with parallel callers, so this keeps it well-behaved rather than queuing up. Use `--profile` to see real per-stage timing for your own library if you want to tune either value.

Clustering is two-stage: average-linkage agglomeration (`--eps`) followed by a centroid-merge pass (`--merge-sim`) that reunites one person's photos when pose/lighting spread them across several clusters. Before clustering, a two-signal quality gate holds low-quality faces out of the automatic grouping: faces smaller than `--min-face-size` pixels, and faces whose embedding is too close to the population-average face (`--max-generic-sim`) because they are occluded (sunglasses/masks), non-frontal, blurry, or false detections. Such faces embed into near-generic vectors that would otherwise pile into one large mixed cluster.

Faces below `--min-cluster-size`, and any face held out by the quality gate, stay as unassigned singletons instead of being grouped. This is why a library can show many singletons: they are the low-resolution or low-quality crops that ArcFace cannot embed into a reliable identity, so they cannot be clustered safely. You can still hand-assign any individual one you recognize in the labeling UI.

Recovering low-quality faces: a real person who appears mostly in distant or occluded photos will have those specific faces in the singletons. To pull more of them back in, re-cluster with looser gates, for example `videre faces --recluster --min-face-size 60 --max-generic-sim 0.45` (or `--eps`/`--merge-sim` to retune cluster tightness) - no re-detection or re-embedding runs, only clustering. Expect some mixed "junk" grouping to reappear as you loosen; the 80px / 0.4 defaults are tuned to avoid it. Residual mixed clusters are best cleared with the "Dissolve cluster" button in the labeling UI rather than by over-loosening.

**Faces workflow:**

```bash
videre scan ~/Photos                      # scan images into the default db
videre faces                              # detect + embed + cluster faces
videre report --faces                     # label in browser, save and close
videre search --person "Alice"            # find all photos of Alice
```

---

## videre classify

Classifies every embedded image as `photo`, `screenshot`, `document`, or
`meme` using zero-shot classification against the SigLIP embeddings
`videre embed` already computed - no new model, no re-reading image files.
Requires a prior `videre embed` run.

```bash
videre classify                     # classify all embedded-but-unclassified hashes, default db
videre classify --db <path>         # explicit db
videre classify --reprocess         # re-classify everything, including already-classified hashes
videre classify --silent            # suppress per-image progress
videre classify --margin <f32>      # min similarity gap to accept a category, else "unknown" (default: 0.05)
```

Each image's stored embedding is scored against 4 fixed text prompts (one
per category) via cosine similarity; the best match wins unless the top two
scores are too close together, in which case the image is stored as
`unknown` rather than a low-confidence guess. Resumable like `videre embed`/
`videre faces`: re-running only classifies hashes that don't have a
classification yet, unless `--reprocess`.

```bash
videre search --category screenshot          # print paths of all screenshots
videre search --category document --json     # same, JSON output
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
videre watch ~/Photos --prune                                     # opt-in: also reclaim stale rows/cache each cycle
```

| Flag | Description |
|------|-------------|
| `--output-sqlite <path>` | Database to populate; defaults to the resolved db (see [The ~/.videre home directory](#the-videre-home-directory)) if omitted |
| `--scan` | Rescan the directory and update `file_hashes` (same as running `videre scan`) |
| `--faces` | Detect, embed, and cluster faces on any images not yet processed |
| `--heic` | Pre-convert and cache HEIC thumbnails (240px and 1200px) per photo, plus a full-resolution original `videre faces` reuses to skip its own conversion |
| `--location` | Reverse-geocode any GPS coordinates not yet resolved to a place name |
| `--prune` | Sync stale rows/cache and clean orphans each cycle (same cleanup as `videre prune`); never deletes real files, only stale db rows and cache entries for files already gone from disk |
| `--interval <seconds>` | Time between cycles (default: 300) |
| `--silent` | Suppress per-cycle progress output |

Pass none of the stage flags and the original four (`--scan`/`--faces`/`--heic`/`--location`) run every cycle - that's the intended default for "just keep my library up to date." `--prune` is opt-in only and never runs unless passed explicitly, so existing `videre watch` invocations keep their current behavior unchanged. Pass any subset to run only those stages.

Cached HEIC thumbnails (and a full-resolution original) land in `~/.cache/videre/thumbnails/`, keyed by the photo's content hash so the same file is never converted twice even across different databases. On first run, if the pre-rename cache at `~/.cache/dupe/thumbnails/` still exists and the new one doesn't, it's migrated automatically (a plain rename, so it's atomic and a no-op on error, since the cache regenerates lazily anyway). `videre report --show-faces` checks the cache before falling back to a live conversion, and `videre faces` reuses the same full-resolution original for detection, so running `videre watch --heic` in the background makes both browsing and face detection on HEIC-heavy libraries noticeably faster. This cache has no size cap or expiry - only `videre prune`'s orphan cleanup (deleting entries for hashes no longer in the database) reclaims space, so it grows in proportion to your HEIC library size over time.

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
- **Cleans orphan cache files**: deletes `~/.cache/videre/thumbnails/` entries (240/1200px thumbnails, face crops, full-res originals) whose hash has no remaining `file_hashes` entry - the only bound on that cache's otherwise-unlimited growth; in-flight `.tmp*` writes from a concurrently running `videre watch` are never touched

In dry-run mode, the orphan embedding and cache-file counts are lower bounds: they reflect only pre-existing orphans, not ones that would be created by the would-be row removals.

---

## videre embed and videre search

`videre embed` computes SigLIP embeddings (google/siglip-so400m-patch14-384, 1152-dim f16) for every image in the database and stores them keyed by content hash. Re-running only processes images not yet embedded. `.mov`/`.mp4` are embedded too, via one representative frame extracted the same way as HEIC (`qlmanage -t`, macOS only) rather than decoding the full video - a cheap, single-frame, not-motion-aware embedding, so video search quality is weaker than photo search. Video hashes are excluded from `videre classify` (none of its four categories fit a video frame). `.dng` is still skipped (the `image` crate has no DNG decoder) - excluded from the
pending-images query up front, so it's never queried and attempted-then-failed on
every run. `videre classify` (see above) reuses these embeddings for zero-shot photo/screenshot/document/meme classification, so it's worth running `videre embed` even if you don't need text/image search.

```bash
videre embed                        # embed all unprocessed images in the default db
videre embed --db <path>            # explicit db
videre embed --batch 64             # larger inference batch size (default: 32)
videre embed --chunk 1000           # rows written per transaction / resume granularity (default: 500)
videre embed --silent               # suppress per-image output
```

**First run downloads ~1.8 GB of model weights from Hugging Face.** Weights are cached in `~/.cache/huggingface/` and reused on every subsequent run. If all images are already embedded, the command exits immediately without loading the model.

```bash
videre search "sunset on beach"                     # text query, default db
videre search --db <path> "sunset on beach"         # explicit db
videre search --image query.jpg                     # find images similar to an example
videre search "birthday cake" -k 10 --scores        # top 10 with cosine scores
videre search --person "Alice"                      # find all photos of Alice (requires videre faces)
```

| Flag | Description |
|------|-------------|
| `--db <path>` | SQLite database with embeddings (default: resolved from `~/.videre`) |
| `--image <path>` | Search by example image instead of a text query (mutually exclusive with a text query) |
| `--person <name>` | Return paths containing a named person - confirmed faces only (mutually exclusive with a text query or `--image`) |
| `--category <name>` | Filter by classified category: `photo`/`screenshot`/`document`/`meme`/`unknown` (mutually exclusive with a text query, `--image`, or `--person`; requires a prior `videre classify` run) |
| `-k, --top-k <n>` | Number of results (default: 20) |
| `--scores` | Prepend the cosine score to each output line |
| `--json` | Emit a single JSON object on stdout instead of text |

`--scores` is a no-op under `--json`: the score is always included in each result.

On macOS, inference uses Metal (Apple Silicon GPU). On Linux, CPU only - embedding large collections will be significantly slower. CUDA support can be enabled by adding `features = ["cuda"]` to the candle dependencies in `crates/videre-ml/Cargo.toml`.

---

## videre fix-dates

Sets each file's `modified_at` timestamp to its EXIF shoot date, so Finder, sort-by-date views, and backup tools see the correct original capture time.

```bash
videre fix-dates --dry-run       # preview without changing anything, default db (never prompts)
videre fix-dates --db <path>     # explicit db
videre fix-dates                 # prompts for confirmation, then applies
videre fix-dates --yes           # skip the confirmation prompt (also: -y)
videre fix-dates --silent        # apply without per-file output (confirmation prompt is unaffected)
```

Before mutating anything, it prints the count of files that will be touched and asks `[y/N]` on stderr; anything other than `y`/`yes` (including EOF, e.g. stdin piped from `/dev/null`) aborts with no changes and exit code 0. `--yes`/`-y` skips the prompt for scripted/non-interactive use.

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

## MCP server (agentic use)

`videre mcp` serves three read-only tools over stdio for MCP clients (Claude Code,
Cursor, and others): `search` (text/person/image), `find_duplicates` (keep/remove
groups; review-only similar clusters with include_similar), and `stats` (library
summary). It binds one database at startup, resolved like every reader:
`--db <path>`, else `default_db` from `~/.videre/config.toml`, else
`~/.videre/hashes.db`; the file must exist. Results reflect the last scan (keep it
fresh with `videre watch`), and tool documents reuse the same shapes and
`"schema_version": 1` as the CLI `--json` output. The first text/image search loads
the embedding model (slow once, then cached for the life of the server).

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

Add `"--db", "/path/to/other.db"` to `args` to serve a non-default library.

---

## videre stats

Prints library totals and per-command pipeline run status in one shot: total
files/size, photo/video split, duplicate groups/files/wasted space, faces
detected/people named, then a line per tracked command (`scan`, `faces`,
`embed`, `classify`, `dedupe`, `fix-dates`, `prune`) showing its last-run
time, status, and duration.

```bash
videre stats                # default db
videre stats --db <path>    # explicit db
videre stats --json         # single JSON object instead of text
videre stats --check        # exit nonzero if any tracked command last failed or crashed
```

A command that has never run shows `never run` / `-`; one currently in
progress is marked `(running now)`. `--json` emits
`{"schema_version": 1, "library": {...}, "pipelines": [...]}`, with
`pipelines` always containing exactly seven entries in a fixed order (`scan`/`faces`/`embed`/`classify`/`dedupe`/`fix-dates`/`prune`; `report`/`search`/`mcp`/`config` are deliberately not tracked - see CLAUDE.md for why).

`--check` doesn't change the printed output (text or `--json`) at all - it only adds an exit code, so cron/launchd can act on a failed pipeline without parsing either output format. Exits nonzero if any tracked command's last recorded run is `failed` or `crashed`; a clean `interrupted` (Ctrl-C) is not treated as a problem.

Per-item errors within a run (a few unreadable files, one corrupted image)
don't mark a row `failed` - only a hard crash does, so `fix-dates`/`faces` can
exit nonzero on real per-file problems while still recording `status:
"success"`. Requires an existing database - an explicit `--db` to a
nonexistent path fails cleanly rather than creating an empty one.

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

CREATE TABLE IF NOT EXISTS faces_scanned (
    hash        TEXT PRIMARY KEY,
    scanned_at  TEXT DEFAULT (datetime('now'))
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

Re-scanning upserts existing rows by `path`. `phash` is only written with `--similar`. EXIF fields (`exif_date`, `gps_lat`, `gps_lon`, `width`, `height`) are written for jpg/jpeg/tiff/heic/dng files; null for all others. `location_name` is added by an idempotent migration on `videre report` startup and is not written by `videre scan` itself - it's populated lazily, one coordinate at a time, when `videre report --show-faces` (or `videre watch --location`) resolves and caches a reverse-geocoded location name. `faces_scanned` records every hash `videre faces` has processed, including images with zero detected faces (which leave no `faces` row) - this is what makes detection resumable. `classifications` is populated by `videre classify` (zero-shot, reuses `embeddings` - no new model or image decoding) and queried via `videre search --category <name>`. `pipeline_runs` holds one upserted row per tracked command (`scan`/`faces`/`embed`/`classify`/`dedupe`/`fix-dates`/`prune`) with its last run's status and duration; a `crashed` status is never stored, only computed when `videre stats` reads a `running` row whose lock is no longer held by a live process.

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

Apache License 2.0 - see [LICENSE](LICENSE).
