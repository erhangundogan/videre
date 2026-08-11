---
title: Workflows
description: What to run before what, and recipes for the common jobs.
sidebar:
  order: 3
---

Most videre commands read work that an earlier command produced. Nothing warns
you loudly when a step is missing: `videre search` with no embeddings finds
nothing, and `videre classify` with no embeddings reports success having done
nothing at all.

This page is the map.

Every command's own page has its flags and caveats; [Commands](/commands/) is
the index.

## The pipeline

```
videre scan                  <- everything starts here
  |
  |-- videre dedupe          find duplicates
  |-- videre fix-dates       correct file dates
  |-- videre locations       group by place
  |-- videre stats           what's in the library
  |-- videre report          browse and review
  |
  |-- videre embed           prepare search  (slow, one-time)
  |     |
  |     |-- videre search "..."        by description
  |     |-- videre search --image      by example
  |     |-- videre report --all        in-page similarity
  |     |-- videre classify            tag screenshots/documents/memes
  |           |
  |           |-- videre search --category
  |
  |-- videre faces           detect and group faces  (slow, one-time)
        |
        |-- videre report --faces      name people (manual step)
              |
              |-- videre search --person
              |-- videre report --show-faces
```

## What each command needs first

| To run | You need |
|---|---|
| Anything at all | [`scan`](/commands/scan/) |
| [`search`](/commands/search/) (text or `--image`) | [`embed`](/commands/embed/) |
| [`search --category`](/commands/search/) | [`embed`](/commands/embed/), then [`classify`](/commands/classify/) |
| [`search --person`](/commands/search/) | [`faces`](/commands/faces/), then naming in [`report --faces`](/commands/report/) |
| [`search --location`](/commands/search/) | GPS in your photos (from `scan`) |
| [`classify`](/commands/classify/) | [`embed`](/commands/embed/) |
| [`report --all`](/commands/report/) similarity button | [`embed`](/commands/embed/) |
| [`report --faces`](/commands/report/) | [`faces`](/commands/faces/) |
| [`report --show-faces`](/commands/report/) names | [`faces`](/commands/faces/) **and** naming done |
| [`dedupe --similar`](/commands/dedupe/) | [`scan --similar`](/commands/scan/) |
| [`locations`](/commands/locations/) | GPS in your photos (from `scan`) |

The manual naming step is easy to overlook. `videre faces` groups faces but
gives them no names, so `search --person` and the names in `--show-faces` stay
empty until you have opened `report --faces` and assigned some.

## What to run afterwards

| After you | Run |
|---|---|
| Delete files (`dedupe \| xargs trash`) | [`prune`](/commands/prune/) |
| [`fix-dates`](/commands/fix-dates/) | [`prune`](/commands/prune/), to re-sync stored timestamps |
| Move or reorganise folders | [`scan`](/commands/scan/), then [`prune`](/commands/prune/) |
| Add new photos | [`scan`](/commands/scan/), then `embed` / `faces` / `classify` again |
| Finish chunked `faces --limit` runs | [`faces --recluster`](/commands/faces/) |
| Change `classify --margin` | [`classify --reprocess`](/commands/classify/) |

`prune` is the one people forget. Until it runs, deleted files are still counted
in `stats`, and their embeddings and cached thumbnails still occupy disk.

## Recipes

### Set up a library from scratch

```bash
videre scan ~/Photos           # minutes; reads every byte
videre embed                   # hours; downloads ~780 MB first
videre faces                   # hours; downloads ~180 MB first
videre classify                # minutes; reuses embed's work
videre locations               # seconds to minutes
videre report --faces          # name the people you care about
```

Only the first is required. Stop wherever you like; each later step adds one
capability. `embed` and `faces` are both resumable, so Ctrl-C is safe.

### Clean up duplicates safely

```bash
videre scan ~/Photos
videre report                  # review groups with KEEP/REMOVE badges
videre dedupe | xargs trash    # delete, once you agree
videre prune                   # reclaim database rows and derived data
```

Add `--similar` to `scan` and `dedupe` if you also want near-duplicates, which
are reported for review only and never included in the delete list.

### Turn on search later

If you scanned a while ago and now want semantic search:

```bash
videre scan ~/Photos --retry-incomplete   # pick up anything new, fast
videre embed
videre search "sunset over water"
```

### Name people

```bash
videre watch ~/Photos --heic   # optional: makes the next step ~70x faster on HEIC
videre faces
videre report --faces          # drag clusters onto people
videre search --person "Alice"
```

On a large library, do detection in sittings:

```bash
videre faces --limit 2000      # repeat as often as you like
videre faces --recluster       # once, at the end
```

### Fix wrong dates

```bash
videre scan ~/Photos
videre fix-dates --dry-run     # check first; this writes to your files
videre fix-dates
videre prune                   # re-sync the timestamps videre stores
```

### Keep everything current

```bash
videre watch ~/Photos
```

That covers scanning, faces, HEIC caching and place names on a loop. It does
**not** cover `embed` or `classify`, so run those by hand after importing a
batch of photos.

Do not run a manual `embed` or `faces` while `watch` is running its faces or
HEIC stages. See the [caveats](/start/cautions/).

### Reclaim disk space

```bash
videre prune                              # orphaned embeddings and thumbnails
du -sh ~/.cache/videre/thumbnails/        # the cache is often the bulk of it
videre stats                              # what each model is using
```

The [thumbnail cache](/reference/paths/#thumbnail-cache) can be deleted
outright; everything in it regenerates.

### After moving files around

```bash
videre scan ~/Photos     # records the new paths
videre prune             # removes the old ones
```

Faces and embeddings survive, because they are keyed by content rather than
path. Only the paths change.

### Ask a compound question

```bash
videre search --category document --date 2025-05 \
  --location "Altunizade, Istanbul" --radius 5
```

Filters AND together, so each one narrows further. See
[compositional searches](/guides/compositional-search/).

### Try a different search model

```bash
videre embed --model google/siglip2-base-patch16-384
videre search "kids playing in snow" --scores
videre search "kids playing in snow" --scores --model google/siglip2-base-patch16-384
videre config set model google/siglip2-base-patch16-384    # if you prefer it
```

The old vectors stay intact and queryable throughout. See
[search models](/reference/models/).

## Rough costs

Worth knowing before starting something long. Figures are from a real 70,000
file library.

| Step | Order of magnitude |
|---|---|
| `scan` (full) | ~10 minutes, reads every byte |
| `scan --retry-incomplete` | ~1 second when nothing changed |
| `embed` | Hours, plus a ~780 MB download |
| `faces` | Hours, plus a ~180 MB download |
| `classify` | Minutes; reuses `embed` |
| `locations` | ~8 minutes, mostly database writes |
| `dedupe`, `stats` | Seconds; pure database reads |

`embed`, `faces` and `classify` are all resumable and only ever process what is
missing, so the second run over an unchanged library is fast.
