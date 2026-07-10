# dupe-faces Design

**Date:** 2026-07-10
**Status:** Approved

## Overview

`dupe-faces` detects, embeds, and clusters faces found in the image collection stored in a dupe SQLite database. It populates a `faces` table that the existing `dupe-report` and `dupe-search` binaries consume for labeling and search. No new binary is introduced for the labeling UI - `dupe-report --faces` starts a local HTTP server that serves an interactive face labeling page with write-back to SQLite.

## Goals

- Detect all faces in the scanned collection and store per-face embeddings
- Cluster faces by identity using DBSCAN so likely-same-person faces group automatically
- Provide an interactive labeling UI served at localhost for human review and approval
- Enable `dupe-search --person "Alice"` to find all photos containing a named person
- Keep search logic in `dupe-core` so both the CLI and the browser server reuse it without duplication

## Out of scope

- Cloud or multi-user identity sync
- Real-time detection during scan (runs as a separate post-scan step)
- Video frame extraction for `.mov` / `.mp4`

---

## SQLite schema

New table added to the existing database:

```sql
CREATE TABLE IF NOT EXISTS faces (
    id            INTEGER PRIMARY KEY,
    hash          TEXT NOT NULL,
    bbox          TEXT NOT NULL,      -- "x,y,w,h" pixel coords in original image
    landmark      TEXT,               -- "x1,y1,x2,y2,x3,y3,x4,y4,x5,y5"
    embedding     BLOB NOT NULL,      -- 512-dim ArcFace L2-normalized f16
    cluster_id    INTEGER,            -- NULL = DBSCAN outlier (singleton)
    person_label  TEXT,               -- set by user via labeling UI
    confirmed     INTEGER DEFAULT 0   -- 1 = user approved, 0 = unreviewed
);
```

`hash` references `file_hashes.hash`. One image can have multiple face rows. `cluster_id` values are local integers assigned per `dupe-faces` run; they carry no meaning across runs. Re-running replaces existing rows for affected hashes.

---

## dupe-faces binary

### Pipeline

1. **Load** - read all `hash` values from `file_hashes` that have no existing face rows (or all, if `--reprocess` is passed). Resolve each hash to one representative file path.

2. **Detect** - run SCRFD 10g (ONNX, via `ort` crate) on each decoded image. Produces zero or more detections per image, each with a bounding box and 5 facial landmarks. Images are decoded with the `image` crate. HEIC files are converted to JPEG via `sips` before decoding (same as `dupe-embed`). `.mov`, `.mp4`, and `.dng` files are skipped.

3. **Align** - warp each detected face crop to the canonical 112x112 ArcFace template using the 5 landmarks (similarity transform). Alignment is mandatory: skipping it causes significant accuracy degradation in ArcFace.

4. **Embed** - run ArcFace w600k_r50 (ONNX, via `ort`) on each 112x112 aligned crop. Output is a 512-dim f32 embedding. L2-normalize and store as f16 BLOB.

5. **Cluster** - run DBSCAN over all embeddings in the current database (not just newly processed ones) using cosine distance. Parameters: `eps = 0.4`, `min_samples = 2`. Faces that do not meet the cluster threshold become outliers (`cluster_id = NULL`). Cluster IDs are reassigned from scratch on every run.

6. **Write** - upsert into `faces`. Existing rows for the same `hash` are replaced. `person_label` and `confirmed` values are preserved if the row already exists and the user has reviewed it (merge by `id`).

### Flags

```
dupe-faces <db>                  # process new hashes only
dupe-faces <db> --reprocess      # re-detect and re-embed all hashes
dupe-faces <db> --dry-run        # detect and embed but do not write to db
dupe-faces <db> --batch <n>      # images per ONNX inference batch (default: 8)
dupe-faces <db> --silent         # suppress per-image progress
```

### Model weights

Both models auto-download from Hugging Face on first run (~200 MB total). Weights cache in `~/.cache/huggingface/`. If all hashes already have face rows, the binary exits without loading the models.

### Crate placement

- ONNX inference wrappers (SCRFD, alignment, ArcFace) go in `crates/dupe-ml`
- DBSCAN implementation goes in `crates/dupe-core`
- Binary entry point: `crates/dupe/src/bin/dupe_faces.rs`

---

## dupe-report --faces (labeling server)

`dupe-report <db> --faces` starts a local axum HTTP server, prints the URL to stderr, opens the browser automatically (`open` on macOS, `xdg-open` on Linux), and runs until the user clicks "Save & close" or sends Ctrl+C.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET /` | Serve the self-contained faces HTML page |
| `GET /api/faces` | Return current people, clusters, singletons as JSON |
| `POST /api/assign` | Assign a cluster or singleton to an existing person |
| `POST /api/new-person` | Create a new person from a cluster or singleton |
| `POST /api/remove-face` | Remove a face from its cluster (sets `cluster_id = NULL`) |
| `POST /api/set-primary` | Change the representative face for a person |
| `GET /api/search/person?name=Alice` | Return file paths containing that person as JSON |
| `POST /api/quit` | Graceful shutdown triggered by "Save & close" button |

All mutations write directly to the `faces` table. `/api/assign` and `/api/new-person` set `confirmed = 1` on the affected rows. The browser re-fetches `/api/faces` after each mutation.

Port: `7878` (fixed). If the port is in use, print an error and exit with code 1.

### Labeling UI: three sections

**Section 1 - People (identified)**

A horizontal grid of person cards. Each card shows one representative cropped face photo and the person's label. Clicking a card opens a modal showing all approved photos for that person; any photo in the modal can be set as the new representative. Person cards are drag-and-drop targets: dropping a cluster card or singleton card onto a person assigns all faces in that card to that person.

**Section 2 - Unassigned clusters**

A grid of cluster cards. Each card shows a drag handle, cluster ID, face count badge, and a thumbnail strip (up to 4 visible, "+N" overflow label). Hovering a face thumbnail shows a "−" remove button. At the bottom: a "New Person" button. Clicking it hides the button and shows a name input with a submit button; submitting creates a new person and moves all cluster faces to them. Dragging the card to Section 1 assigns the cluster to an existing person.

**Section 3 - Unassigned singletons**

Same card component as Section 2 but shows a single large face photo instead of a thumbnail strip. Same interactions: drag to Section 1, hover shows "−" remove, "New Person" button creates a new person for this face.

### Crate placement

- axum server: `crates/dupe/src/bin/dupe_report.rs` (added behind `--faces` flag, not a separate binary)
- Person search core logic: `crates/dupe-core` (shared with `dupe-search`)

---

## dupe-search --person

```bash
dupe-search <db> --person "Alice"
dupe-search <db> --person "Alice" -k 20
dupe-search <db> --person "Alice" --scores
```

Queries `faces` for all rows where `person_label = ?` and `confirmed = 1`. Collects distinct `hash` values, joins to `file_hashes` to resolve paths, prints one path per line to stdout.

`--scores` prepends the cosine similarity of the closest matching face in each photo. `--person` is mutually exclusive with text query and `--image` modes.

Only `confirmed = 1` rows are used. DBSCAN-assigned cluster memberships (`confirmed = 0`) are excluded to prevent false positives from unreviewed assignments.

The search logic (SQL query + optional cosine ranking) lives in `dupe-core` so the axum server's `/api/search/person` endpoint reuses it without duplication.

---

## Data flow summary

```
dupe --output-sqlite <db>        # scan: populates file_hashes
dupe-faces <db>                  # detect + embed + cluster: populates faces
dupe-report <db> --faces         # serve labeling UI at localhost:7878
                                 # user assigns clusters/singletons to people
                                 # confirmed=1 rows written back to faces table
dupe-search <db> --person "Alice"  # find all photos of Alice (stdout)
```

---

## Testing approach

- Unit tests for DBSCAN in `dupe-core` with synthetic 512-dim vectors
- Integration test for `dupe-faces` binary: fixture DB with 2 images each containing a synthetic face crop, assert `faces` rows are written
- Integration test for `dupe-report --faces`: start server, send HTTP requests to `/api/new-person` and `/api/assign`, assert SQLite state reflects changes
- Integration test for `dupe-search --person`: fixture DB with pre-populated `faces` rows, assert correct paths on stdout

---

## Runtime notes

- `ort` (ONNX Runtime) and `candle` (SigLIP) coexist without symbol conflicts. Both are already used in the workspace; no exclusion or feature-flag required.
- Metal acceleration is not available for ONNX Runtime on macOS without the `ort` CoreML execution provider feature. CPU inference is the baseline; CoreML EP can be added later as an optional feature flag.
- HEIC face detection requires `sips` (macOS only). On Linux, HEIC images are skipped for face detection but EXIF metadata remains available from the scan.
