# Image Search Design

Date: 2026-07-08
Status: Implemented

## Goal

Semantic image search across millions of images, fully local on Apple Silicon (M1 Max, 32GB). Text-to-image ("sunset on beach") and image-to-image (find visually similar) in the first slice. Captioning and face recognition follow later on the same foundation.

## Approach

Joint embedding space (SigLIP family) + brute-force vector scan over SQLite-stored embeddings. No ANN index in v1: exact scan of ~1M f16 vectors takes a few hundred ms on M1 Max, with zero recall loss, trivial deletes, and trivial metadata prefiltering. HNSW (usearch) is deferred until real 2M+ scale and will be a derived, rebuildable artifact, never the source of truth.

Key decisions (informed by a high-effort model review):

- **Embed by content hash, not path.** The corpus is deduplicated by construction; each unique BLAKE3 hash is embedded once. Halves indexing cost on typical photo libraries and makes re-scans free.
- **SigLIP so400m/14-384** over CLIP ViT-B/32 or ViT-L/14: better retrieval quality per FLOP, better long-query handling. SigLIP 2 weights are used if they load cleanly in candle; SigLIP v1 so400m is the tested fallback. Model choice is schema-level (changing it invalidates all vectors), so `model_id` is stored per row.
- **candle + Metal** over ort + CoreML: pure Rust, predictable, no dylib packaging. Embedding is a one-time batch cost; operational simplicity beats peak throughput. Revisit only if measured throughput hurts.
- **Vectors L2-normalized before storage**, similarity is dot product.
- Separate, lazily runnable processes per workload (embed / caption / faces), resumable via SQLite state. SQLite is the queue. Run sequentially, not in parallel (shared GPU/memory budget).

## Workspace restructure

Convert the flat crate to a Cargo workspace (own commit, before any ML code):

```
dupe/
  Cargo.toml            workspace root
  crates/
    dupe-core/          shared types, SQLite schema helpers, hash utilities
    dupe/               existing binaries: dupe, dupe-report, dupe-fix-dates
    dupe-ml/            new binaries: dupe-embed, dupe-search
```

Existing binary names, CLI flags, and output behavior are unchanged. `cargo build -p dupe` stays fast; the heavy candle dependency tree lives only in dupe-ml.

## Schema

Added to the same SQLite database:

```sql
CREATE TABLE IF NOT EXISTS embeddings (
    hash        TEXT PRIMARY KEY,   -- joins file_hashes.hash
    model_id    TEXT NOT NULL,      -- e.g. "siglip-so400m-384"
    embedding   BLOB NOT NULL,      -- L2-normalized f16, little-endian
    embedded_at TEXT NOT NULL
);
```

Designed for later (documented here, not created in this slice):

```sql
CREATE TABLE captions (
    hash       TEXT PRIMARY KEY,
    model_id   TEXT NOT NULL,
    caption    TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE faces (
    id         INTEGER PRIMARY KEY,
    hash       TEXT NOT NULL,       -- source image content hash
    bbox       TEXT NOT NULL,       -- x,y,w,h
    embedding  BLOB NOT NULL,       -- 512-d ArcFace
    cluster_id INTEGER              -- person cluster, assigned offline
);
```

Faces are per-face rows (one image contains 0..N faces), never a column on an image table.

## dupe-embed

```
dupe-embed <db> [--batch 32] [--chunk 500] [--silent]
```

- Selects hashes present in `file_hashes` but missing from `embeddings`; picks one readable path per hash.
- Decode + resize on rayon threads feeding a bounded channel; batched inference (16-32) through the SigLIP vision tower on Metal; falls back to CPU off-macOS.
- Writes per chunk (~500) in a single transaction. Resumable by construction: kill anytime, rerun continues where it left off.
- HEIC decoded via `sips` (matching dupe-report); `.mov`/`.mp4` skipped in v1.
- Weights auto-download from Hugging Face on first run to `~/.cache/huggingface/` (standard hf-hub cache). Missing weights produce a clear error, not a panic.
- Corrupt/unreadable images: logged to stderr, skipped, never fatal.
- Progress on stderr; `--silent` suppresses it. Consistent with existing tools.

Expected bottleneck is JPEG decode + resize, not inference. If throughput disappoints, first reach for zune-jpeg / fast_image_resize, not a different inference stack.

## dupe-search

```
dupe-search <db> "sunset on beach"      # text query
dupe-search <db> --image photo.jpg      # image-to-image
  -k <n>        top-k results [default: 20]
  --scores      prepend cosine score to each line
```

- Text query: encode with SigLIP text tower. Image query: encode with vision tower. Same search path after that.
- Brute-force SIMD dot-product scan over all embeddings, top-k by score.
- Output: matched paths to stdout, one per line (pipe-friendly). All duplicate paths sharing a matched hash are listed with that hash's score.
- One-shot CLI with ~1s model cold start is accepted for v1; a warm daemon/server mode is a later option if interactive latency matters.

## Testing

- Unit: embedding BLOB serialization round-trip, missing-hash selection query, chunk resume behavior, top-k ordering, L2 normalization.
- Integration: end-to-end against the real binaries with a tiny fixture; full-model tests feature-gated (weights are ~1.5GB, unfit for CI). Exact mechanism decided during implementation.

## Known limitations

- SigLIP-family text weakness: text inside images, negation, counting. Partially covered later by on-demand VLM captioning (Qwen2.5-VL class), which is a presentation feature, not the search index.
- Cosine scores are not calibrated; "no good match" thresholds must be tuned empirically per model.
- Memory budget on 32GB: vision+text towers (~1GB) are fine; when captioning/faces arrive, VLM must load lazily and unload.

## Out of scope for this slice

dupe-caption, dupe-faces, ANN index (HNSW/usearch), video embedding, HTML report integration, hybrid cloud, binary quantization. Schema and architecture accommodate all of them.
