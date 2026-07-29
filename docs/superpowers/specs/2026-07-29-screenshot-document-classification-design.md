# Screenshot/document classification design

## Background

Noted in the project roadmap as a candidate next CLI feature: photo libraries
accumulate screenshots, scanned documents/receipts, and memes alongside real
photos, which pollutes dedup review, date/location browsing, and any future
"smart albums" view. The roadmap flagged this as the cheapest of the two
"needs new capability" items (the other being duplicate-video detection)
because it can reuse `videre embed`'s existing SigLIP embeddings via
zero-shot classification, rather than requiring a new model.

This pass is backend/CLI only. Desktop app UI (a "Smart Albums" or
"Smart Photos" sidebar section - already a commented-out placeholder in
`app/src/components/application-shell11.tsx`) is an explicit non-goal here;
it's a natural follow-up once classification data exists to show.

## 1. Architecture / data flow

`videre classify` loads the SigLIP model via the same `videre_ml::model`/
`device` infrastructure `embed`/`search` already use (`Embedder::load`,
`Embedder::embed_text(&self, text: &str) -> Result<Vec<f32>>` - confirmed
existing API, `crates/videre-ml/src/model.rs:106`). It embeds the 4 fixed
category prompts once at startup, then iterates every hash present in
`embeddings` (via `videre_core::embeddings::load_embeddings`) that is not yet
present in a new `classifications` table. For each embedding (decoded f16 ->
f32 via `videre_core::vectors::from_f16_bytes`, already L2-normalized same as
`embed`/`search`), computes cosine similarity (plain dot product, since
vectors are pre-normalized - same approach as `videre_ml::search::top_k`)
against all 4 prompt vectors, picks the top match and the margin over the
second-best score. If the margin clears `--margin` (default 0.05), stores
that category; otherwise stores `"unknown"`.

No image files are read and no HEIC/qlmanage involvement at all - this runs
entirely over vectors already computed by a prior `videre embed` run, so it
is fast (linear algebra over already-in-memory f32 vectors) and inherits
`embed`'s file-type coverage automatically (whatever has an embedding gets
classified; `.mov`/`.mp4`/`.dng` are skipped the same way `embed` skips them,
with no special-casing needed here).

## 2. Schema

New table, `crates/videre-core/src/classify.rs` (new module, following the
existing one-module-per-table pattern: `embeddings.rs`, `face_db.rs`):

```sql
CREATE TABLE IF NOT EXISTS classifications (
    hash          TEXT PRIMARY KEY,
    category      TEXT NOT NULL,   -- 'photo' | 'screenshot' | 'document' | 'meme' | 'unknown'
    confidence    REAL NOT NULL,   -- cosine similarity of the winning prompt (or the top score, for 'unknown')
    classified_at TEXT NOT NULL
);
```

Keyed by content hash (same convention as `embeddings` and `faces`), so
re-scanning the same photo under a different path never re-classifies it.
`confidence` stores the raw cosine similarity of the winning prompt (not the
margin itself), so a future threshold retune could in principle reclassify
existing `unknown` rows using data already stored - not built in this pass,
just a property of storing the raw score rather than only a boolean/margin.

## 3. CLI surface

New subcommand, `crates/videre/src/commands/classify.rs`:

```bash
videre classify                     # classify all embedded-but-unclassified hashes, default db
videre classify --db <path>         # explicit db
videre classify --reprocess         # re-classify everything, including already-classified hashes
videre classify --silent            # suppress per-image progress
videre classify --margin <f32>      # min similarity gap between best and second-best category to accept (default: 0.05)
```

Requires a prior `videre embed` run - if `embeddings` is empty, prints a
message and exits cleanly (0 rows classified), same shape as `videre faces`
noting nothing to process rather than treating it as an error.

`crates/videre/src/commands/search.rs` gains a fourth mutually-exclusive
mode:

```bash
videre search --category screenshot          # print paths of all files classified as screenshots
videre search --category document --json     # same, JSON output
```

`--category` joins the existing `conflicts_with` chain alongside `query`/
`image`/`person` (SearchArgs currently has `image: Option<PathBuf>` and
`person: Option<String>` each marked `conflicts_with` the others - `category`
is added the same way). Implementation mirrors `person_hits` (a plain SQL
query, no model load needed): `SELECT DISTINCT path FROM file_hashes JOIN
classifications ON file_hashes.hash = classifications.hash WHERE
classifications.category = ?`, printing all paths (all duplicate paths per
matched hash, same as every other search mode) or the `--json` document
shape (`results` with `path`/`hash`, no `score` field - same as `--person`
hits today, since there's no per-result ranking here either, just a set
membership).

## 4. Classification prompts & threshold

```rust
const CATEGORY_PROMPTS: &[(&str, &str)] = &[
    ("photo", "a photo of a person, place, or thing"),
    ("screenshot", "a screenshot of a phone or computer screen"),
    ("document", "a photo of a document, receipt, or piece of paper"),
    ("meme", "a meme image with text captions overlaid on a picture"),
];
```

These are starting-point captions - SigLIP embeds full descriptive captions
better than bare single-word labels - not exposed as a CLI flag in this
pass, just constants in `crates/videre-ml/src/classify.rs` (new module,
alongside the pure decision function below) to tune later if real-world
results look off.

`--margin` (default `0.05`) is the only tunable knob: if the top two
categories' cosine similarities are within that gap of each other, the
result is `"unknown"` instead of the top pick.

## 5. Pure, testable decision logic

Following the pattern already used for `round_robin_partition`/
`apply_worker_msg_counts` in the faces-pipeline-parallelization work:
extract the pure decision logic as a standalone, TDD'd function that takes
already-computed similarity scores and returns the winning category (or
`"unknown"`) plus its confidence - fully unit-testable without touching
SigLIP or a real database:

```rust
/// Picks the winning category from per-prompt similarity scores, or
/// "unknown" if the top two scores are within `margin` of each other.
/// `scores` must be non-empty (one entry per CATEGORY_PROMPTS entry).
pub fn classify_from_scores(scores: &[(&str, f32)], margin: f32) -> (&'static str, f32) {
    // sort descending, compare top two, return ("unknown", top_score) if
    // margin not cleared, else (top_category, top_score)
}
```

Tests to write (TDD, before implementation):
- clear winner (large gap) returns that category with its score
- top two scores within `margin` returns `"unknown"` with the top score
- exactly-at-the-margin-boundary is treated as NOT clearing it (i.e.
  `gap > margin`, not `>=`, needs picking one and making it explicit per
  the spec-review step - **decision: `gap > margin` is required to accept,
  so a gap exactly equal to the margin falls back to `"unknown"`**)
- single-entry `scores` (degenerate case, e.g. if CATEGORY_PROMPTS somehow
  had one entry) returns that entry with its own score, no panic

The actual model-loading + DB-iteration pipeline built around this function
is verified by building and running against real data (same as every other
ML-pipeline piece in this codebase - no existing test here exercises real
model inference, confirmed during the faces-pipeline-parallelization work).

## 6. Non-goals for this pass

- No desktop app UI (no "Smart Albums"/"Smart Photos" sidebar section, no
  Tauri commands, no `videre-api` facade additions)
- No changes to `videre report`'s HTML output (no "exclude screenshots"
  toggle, no category badges in the report)
- No combined "smart albums" (person + date + location + category) feature
- No auto-tuning of the margin threshold
- No CLI-exposed prompt customization (prompts are source constants for now)
- No re-classification-on-recluster-style flag beyond `--reprocess`
  (re-running `--reprocess` after tuning `CATEGORY_PROMPTS` or `--margin` is
  the supported path for now)
