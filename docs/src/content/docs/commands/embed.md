---
title: videre embed
description: Prepare photos so they can be searched by description. One-time and resumable.
---

Prepares photos so they can be searched by description. One-time per photo, and
resumable.

```bash
videre embed                           # process everything not done yet
videre embed --db ~/photos.db          # use a specific database
videre embed --model <model-id>        # prepare with a specific model, kept separately
videre embed --batch 64                # images per inference batch (default 32, max 96)
videre embed --chunk 1000              # rows saved per transaction (default 500)
videre embed --silent                  # no per-image progress
```

Safe to Ctrl-C: rerunning continues where it stopped.

The first run downloads about 780 MB of model data. See
[search models](/reference/models/) for choosing a different one and for where
the data is kept.

:::caution[Raising `--batch` is not a way to make this faster]
Values above 96 are capped automatically, with a warning.

Above a threshold measured between 121 and 127, the batched path silently
returns embeddings that do not match a one-at-a-time baseline: no error, no NaN,
just wrong vectors. Checking output for zero or NaN values does not detect it.
Larger batches also buy no speed — 31.0 ms/image at 96 against 39.1 ms at 768.
:::

## What gets embedded

Photos, plus `.mov` and `.mp4` via one representative frame. Video embedding is
single-frame and not motion-aware, so video search quality is weaker than photo
search.

`.dng` is skipped: there is no DNG decoder available. EXIF metadata from those
files is still recorded by [`videre scan`](/commands/scan/).

HEIC needs macOS. See [platform support](/reference/platforms/).

## Tuning

`VIDERE_EMBED_DTYPE=f16` switches inference to half precision: about 11% faster
on pure JPEG/PNG, 7% on a realistic mix, with no meaningful quality change. It
is opt-in because 7% did not justify perturbing an existing library.
