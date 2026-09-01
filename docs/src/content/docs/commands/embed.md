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
videre embed --type video              # only videos
videre embed --after 2024-01-01        # only files from this year on
```

:::tip
These filters work the same way across commands, and combine. See
[scoping a run](/guides/scoping-a-run/).
:::

## A first run on a real library

The first run downloads about 780 MB of model data, then works through every
image. On a large library this takes hours, so plan to leave it running.

```bash
videre embed                    # start; Ctrl-C whenever you like
videre stats                    # how far it got
videre embed                    # continue from there
```

`videre stats` reports a row count per model, so the gap between that and your
photo count is what remains:

```
embeddings
  google/siglip-base-patch16-224   12,481 rows   768 dims   28.4 MB
```

Work is committed every `--chunk` rows (500 by default), so an interrupt loses
at most that much. There is no separate resume flag: rerunning *is* resuming,
because the command only ever looks for hashes that have no vector yet.

Adding photos later works the same way. Run [`videre scan`](/commands/scan/) to
pick them up, then `videre embed` again to cover only the new ones.

## What gets skipped

| Type | Embedded? |
|---|---|
| jpg, png, tiff, webp, bmp, gif | yes |
| heic | macOS only |
| mov, mp4 | macOS only, from one frame |
| dng | never |

`.dng` is excluded up front rather than attempted and failed, so a library full
of raw files does not waste a decode attempt on each of them every run. Their
EXIF is still recorded by `scan`.

Video embedding uses a single representative frame, not the motion, so video
search is noticeably weaker than photo search. A clip whose subject appears
later than its opening frame will not match well.

On Linux, HEIC and video are skipped entirely. See
[platform support](/reference/platforms/).

## Running more than one model

Each model writes to its own database, so they never overwrite each other and
you can hold several at once:

```bash
videre embed                                              # the default model
videre embed --model google/siglip2-base-patch16-384      # a second, larger one
videre stats                                              # row counts per model
videre search "sunset" --model google/siglip2-base-patch16-384
```

Preparing a second model does not disturb the first, and switching between them
invalidates nothing. That makes it practical to try a larger model on a real
library and keep the old vectors until you are convinced.

Be aware of the cost before starting: a second model means a second full pass
over every image, plus its own download (1.4 GB for
[`siglip2-base-patch16-384`](https://huggingface.co/google/siglip2-base-patch16-384),
3.3 GB for
[`siglip-so400m-patch14-384`](https://huggingface.co/google/siglip-so400m-patch14-384))
and its own 130 MB to 190 MB of vectors
per 70,000 photos.

If you settle on the new one, make it the default and the old vectors can be
deleted by removing that model's file:

```bash
videre config set model google/siglip2-base-patch16-384
```

See [search models](/reference/models/) for where the files live.

## Caveats

:::caution[Raising `--batch` is not a way to make this faster]
Values above 96 are capped automatically, with a warning.

Above a threshold measured between 121 and 127, the batched path silently
returns embeddings that do not match a one-at-a-time baseline: no error, no NaN,
just wrong vectors. Checking output for zero or NaN values does not detect it.

Larger batches also buy no speed. Measured: 31.0 ms/image at batch 96 against
39.1 ms at 768.
:::

**Do not run `embed` and [`faces`](/commands/faces/) at the same time.** Both
drive HEIC conversion through the same macOS service, and each limits itself
independently, so together they overwhelm it. Measured with both running: a HEIC
file took over 16 seconds against about 7.6 normally, and one exceeded the
timeout entirely. Nothing is lost, since skipped files retry next run, but it is
slower than doing one after the other. The same applies to
[`videre watch`](/commands/watch/) if it is running with its faces or HEIC
stages.

**Vectors are keyed by content, not path.** Two copies of one photo cost a
single embedding, and moving a file keeps its vector as soon as it is
re-scanned.

## Tuning

`--chunk` controls how often work is committed. Larger values are slightly
faster and lose more on an interrupt.

`VIDERE_EMBED_DTYPE=f16` switches inference to half precision: about 11% faster
on pure JPEG/PNG, 7% on a realistic mix, with no meaningful quality change. It
is opt-in because 7% did not justify perturbing an existing library, and it
does not affect vectors already written.

## Scoping the run

Every flag below narrows an existing set, never widens it, and they combine:
each condition must hold.

| Flag | Selects |
|---|---|
| `--type` | `image` or `video`. Repeatable, or comma-separated |
| `--ext` | file extension, e.g. `mov`. Repeatable, or comma-separated |
| `--mime` | exact type, e.g. `video/quicktime`. Repeatable, or comma-separated |
| `--after` | date on or after this (inclusive) |
| `--before` | date before this (exclusive) |
| `--date` | a whole year, month or day: `YYYY`, `YYYY-MM`, `YYYY-MM-DD` |
| `--location` | within `--radius` km of a place, e.g. `"Berlin, Germany"` |
| `--radius` | radius in km for `--location` (default 20) |
| `--path` | only files under this directory. Repeatable |
| `--has` | only files with this metadata. Supported fields: `gps`, `date` |
| `--missing` | only files missing this metadata. Supported fields: `gps`, `date` |

`--person` and `--category` are deliberately absent: both are derived from data
this command produces, so selecting its input by one would be circular.

A scoped run prints `N of M`, so a filter that matches nothing is
distinguishable from an empty library. Full detail, including how missing data
excludes a file, is in [scoping a run](/guides/scoping-a-run/).

## More detail

- [Long-running jobs](/guides/long-running-jobs/) covers what is safe to run alongside this, and what an interrupt costs.
- [Using several search models](/guides/multiple-models/) covers trying a bigger model without losing this work.
