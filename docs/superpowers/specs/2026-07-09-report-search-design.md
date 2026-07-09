# dupe-report: all-files gallery and in-page similarity search

Date: 2026-07-09
Status: Approved

## Goal

Make the semantic search feature usable visually. `dupe-search` prints paths to
stdout; reviewing results means opening files one by one. This feature adds an
optional all-files gallery to the `dupe-report` HTML page with client-side
"find similar" image search powered by the existing SigLIP embeddings.

The report stays a fully static, self-contained, double-clickable HTML file.
No server process, no external assets. Text-to-image search stays in the CLI
(`dupe-search`): embedding a text query requires the SigLIP model, which cannot
run inside a static page.

## CLI behavior

`dupe-report` gains one flag: `--all`.

- Without `--all` (default): output is unchanged from today. Duplicate groups
  only, no vectors embedded, no size increase. For the same database the output
  is byte-identical to the current implementation.
- With `--all`: the page adds an "All files" gallery section below the
  duplicate groups, listing every row in `file_hashes` (singular files
  included). If the database has an `embeddings` table with rows for the
  current model id (`google/siglip-so400m-patch14-384`), the vectors are
  embedded into the page and "find similar" is enabled. If not, the gallery
  still renders, search controls are omitted, and a hint is printed to stderr:
  "no embeddings found; run dupe-embed for similarity search".

`--all` composes with `--heic` / `--heic-original` exactly as duplicate groups
do today (same thumbnail embedding rules).

## Page layout

Top to bottom:

1. **Stats header**: as today, plus an "N embedded" stat when `--all` is set
   and embeddings exist.
2. **Results panel**: hidden by default. When a search runs it shows the query
   image thumbnail on the left, then the top-k matches (default k = 24) ranked
   by cosine score, with the score rendered as a small overlay label on each
   card, plus a Clear button. Results are per-hash: a hash with multiple
   duplicate paths renders one card with an "xN copies" badge.
3. **Duplicate groups**: unchanged, KEEP/REMOVE badges as today. Each image
   card gains a small "Similar" button (only when the card's hash is embedded).
4. **All files gallery** (only with `--all`): grid of thumbnail cards using the
   same lazy-loaded `file://` thumbnail and lightbox machinery as duplicate
   groups. No KEEP/REMOVE badges on singular files. Each card shows thumbnail,
   filename, size, best date, and a "Similar" button when the hash is embedded.
   A count header shows the total ("5441 files").

Every image card (duplicate or gallery) carries a `data-hash` attribute so
results can locate, highlight, and scroll to cards.

## Search data flow

Rust side (report generation, `--all` only):

- Read `embeddings` rows for `MODEL_ID`, ordered by hash.
- Concatenate the raw f16 little-endian BLOBs in that order into one buffer.
- Base64-encode once (reusing the existing `base64_encode` helper) and emit two
  script constants: `VEC_B64` (one base64 string) and `VEC_HASHES` (JSON array
  of hashes, parallel to the vector order).
- Emit `VEC_DIM = 1152` so JS does not hardcode the dimension.

JS side:

- At page load: decode `VEC_B64` to bytes, convert f16 to f32 into one flat
  `Float32Array` of length `hashes.length * VEC_DIM`. The f16 decoder is a
  ~15-line function; no `Float16Array` dependency (too new), no libraries.
- On "Similar" click: dot product of the clicked hash's vector against all
  vectors, rank descending, exclude the query hash itself, take top-k, render
  the results panel. Non-finite scores are skipped (mirrors `search.rs`
  semantics). Vectors are L2-normalized at embed time, so dot product equals
  cosine similarity.
- Expected cost at 4k images: about 4.6M multiply-adds, under 50ms.

Payload size: 1152 dims x 2 bytes x ~4000 images is about 9.2 MB raw, about
12.3 MB after base64. Accepted cost for a local file. If library size grows
10x, int8 quantization (per-vector scale) can halve this; the vector block is
isolated so this swap stays local to one function on each side.

## Edge cases

- File in `file_hashes` but not embedded (mov, mp4, dng, HEIC on Linux, files
  added after the last dupe-embed run): shown in the gallery without a Similar
  button.
- Hash embedded but all its files deleted since the scan (trashed duplicates
  still in the DB): appears in gallery and results; the thumbnail renders as
  the browser's broken-image state, same as today's behavior for moved files.
  Not treated specially in v1.
- No `embeddings` table at all: gallery renders, search absent, stderr hint.
- Dimension mismatch in a stored vector (corrupt row): skipped during JS
  ranking, same as `search.rs::top_k`.

## Testing

Rust unit/integration tests (extend `crates/dupe/tests/integration.rs` or a
report-specific test file):

- Vector block generation: correct base64 for known synthetic vectors, correct
  hash ordering, empty `embeddings` table case, absent table case.
- Gallery HTML: singular files present without KEEP/REMOVE badge markup;
  duplicate files keep their badges.
- Regression: without `--all`, output for a fixture DB is byte-identical to the
  pre-change implementation.
- f16 sanity: a known f16 byte pattern in a fixture asserts the exact base64
  the JS decoder will receive, pinning the byte order contract between the
  Rust writer and the JS reader.

Manual verification on the real photo database (`~/dupe-demo` scan output):
gallery renders, Similar returns visually related images, Clear works,
lightbox still works from both sections.

## Out of scope (deliberately)

- Text search in the page (requires the model; stays in `dupe-search`).
- ANN index (brute force is fine at this scale).
- Server mode.
- Special handling for deleted-but-embedded files.
- Faces, captions (separate upcoming features; the results panel and
  `data-hash` card linkage are the surfaces they will plug into later).
