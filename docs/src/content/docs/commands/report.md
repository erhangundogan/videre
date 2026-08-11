---
title: videre report
description: Build a browsable HTML gallery, or serve the interactive face-naming UI.
---

Builds a browsable HTML page, or serves the interactive face-naming UI.

```bash
videre report                          # duplicate-review page, written next to the database
videre report -o out.html              # write somewhere specific (--output works too)
videre report --all                    # every file, with in-page similarity search
videre report --by-date                # Year/Month/Day drill-down gallery
videre report --heic                   # embed HEIC thumbnails (macOS only, bigger file)
videre report --heic-original          # ...plus full-size versions for the lightbox
videre report --faces                  # face-naming UI at http://localhost:7878
videre report --show-faces             # live page showing names and places in the lightbox
videre report --db ~/photos.db         # use a specific database
videre report --all --model <model-id> # use a specific model for in-page similarity
```

## Two phases

**Before deleting**, run it without `--all` to review duplicate groups with
KEEP/REMOVE badges.

**After deleting**, run it with `--all` to browse the cleaned collection. Files
recorded in the database but no longer on disk are excluded automatically, at
generation time; the database itself is not modified.
[`videre prune`](/commands/prune/) removes those rows permanently.

## Static file vs. local server

`--faces` and `--show-faces` start a local server on `localhost:7878` instead of
writing a file. Everything stays on your machine.

| Flags | What `/` serves |
|---|---|
| `--faces` | The labeling UI |
| `--show-faces` | The live report, with face and location metadata in the lightbox |
| Both | The live report at `/`, the labeling UI at `/faces` |

`--by-date` is fully static, and combines with `--all`, `--heic` and
`--heic-original`.

Server mode is needed for `--show-faces` because the lightbox shows each photo's
named faces (clicking one jumps to that person) and a looked-up place name, both
of which need a live backend rather than a baked-in file.

## What the page contains

- Stats header. Duplicate tiles and the toolbar only appear when there is at
  least one duplicate group.
- Expand all / Collapse all, and sorting by wasted space or by date kept.
- Per file: thumbnail, KEEP/REMOVE badge, filename, path with a copy button,
  size, created, modified, EXIF date, GPS link, dimensions.
- `.mov` and `.mp4` shown as a video thumbnail; click for playback.
- Lightbox for full-size viewing. Escape or a backdrop click closes it.
- `--all` adds a paged gallery plus a "Similar" button per file, giving the top
  24 matches. That needs a prior [`videre embed`](/commands/embed/).

## HEIC thumbnails

In static mode, HEIC files show as "HEIC" text by default. `--heic` embeds a
240px thumbnail; `--heic-original` also embeds a 1200px version for the
lightbox. Both are macOS only.

In server mode (`--show-faces`) HEIC always renders, and `--heic` /
`--heic-original` are ignored: thumbnails are converted lazily per request,
checking [`videre watch --heic`](/commands/watch/)'s cache first. Converting
every HEIC file up front made server mode take minutes on large collections.

## The face-naming UI

`videre report --faces` serves:

- **People**, **Unassigned Clusters**, and **Singletons** sections, colour-coded
  consistently across cards, badges and titles.
- Drag a cluster onto a person to assign it, or click "New Person".
- A detail page per cluster showing every face at full size, with per-face
  remove and assign.
- "Dissolve cluster" to ungroup a wrongly-merged cluster back into singletons.
  Faces are not deleted.
- Click any face to open the full-resolution original.

Names are written back to the database. Close the tab, press Ctrl-C, or use
"Save & Close" to stop the server. Then
[`videre search --person`](/commands/search/) works.
