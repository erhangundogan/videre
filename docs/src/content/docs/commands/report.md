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

## Which mode do you want

| Goal | Command |
|---|---|
| Check duplicates before deleting | `videre report` |
| Browse everything you own | `videre report --all` |
| Find photos from a particular trip | `videre report --by-date` |
| Name people | `videre report --faces` |
| Browse with names and places shown | `videre report --show-faces` |
| Hand a page to someone else | `videre report --all --heic` |

The first three write a file you can open, mail, or keep. The last two start a
local server and exist only while it runs.

## Static pages

```bash
videre report                              # duplicates only
videre report --all                        # whole library, paged, with Similar buttons
videre report --by-date                    # Year > Month > Day drill-down
videre report --all --by-date -o all.html  # combine freely
```

These write a single self-contained HTML file. Open it directly, keep it as a
snapshot, or copy it to another machine. Files recorded in the database but no
longer on disk are excluded at generation time, without modifying the database;
[`videre prune`](/commands/prune/) removes those rows permanently.

`--all` adds a paged gallery plus a **Similar** button per file, giving the top
24 visually closest images. That needs a prior [`videre embed`](/commands/embed/);
without it the button is disabled with a note rather than failing the report.

### The duplicate-review page

Run it before deleting anything. Each group shows KEEP and REMOVE badges, sorted
by wasted space, with per-file size, dates, GPS and dimensions.

```bash
videre report && open ~/.videre/hashes_report.html
videre dedupe | xargs trash          # once you have looked
```

The KEEP choice is the oldest EXIF date, falling back to file timestamps. If you
disagree with a call, that is what the page is for: check before piping.

Near-duplicate groups from [`scan --similar`](/commands/scan/) appear here for
review, and deliberately never in `dedupe`'s pipeable output.

## Server modes

`--faces` and `--show-faces` start a local server on `localhost:7878` instead of
writing a file. Everything stays on your machine.

| Flags | What `/` serves |
|---|---|
| `--faces` | The labeling UI |
| `--show-faces` | The live report, with face and location metadata in the lightbox |
| Both | The live report at `/`, the labeling UI at `/faces` |

Stop the server with Ctrl-C, or the **Save & Close** button in the labeling UI.

`--show-faces` needs a server because the lightbox shows each photo's named faces
(clicking one jumps to that person) and a looked-up place name, neither of which
can be baked into a static file.

### The labeling UI

Served by `--faces`, after [`videre faces`](/commands/faces/) has run:

- **People**, **Unassigned Clusters** and **Singletons** sections, colour-coded
  consistently across cards, badges and titles.
- Drag a cluster onto a person to assign it, or click **New Person**.
- A detail page per cluster showing every face full size, with per-face remove
  and assign.
- **Dissolve cluster** ungroups a wrongly-merged cluster back into singletons.
  Faces are not deleted.
- Click any face to open the full-resolution original.

Names are written back to the database, so
[`search --person`](/commands/search/) works as soon as you have assigned them.

## HEIC thumbnails

This is where report gets slow or large, so it is worth understanding.

**In static mode**, HEIC files show as "HEIC" text by default, because embedding
them means converting every one up front and inlining the result:

| Flag | Effect | Cost |
|---|---|---|
| *(none)* | Text placeholder | Instant, small file |
| `--heic` | 240px thumbnails embedded | Slower, noticeably bigger file |
| `--heic-original` | Plus 1200px lightbox versions | Slowest, much bigger file |

On a HEIC-heavy library `--heic-original` can produce a very large HTML file,
since every image is base64 inlined. That is the price of a page that works
anywhere with no server.

**In server mode** (`--show-faces`), HEIC always renders and `--heic` /
`--heic-original` are ignored. Thumbnails are converted lazily per request and
served from the [cache](/reference/paths/#thumbnail-cache), so the page opens
immediately instead of taking minutes.

Warming that cache first makes server mode fast from the first click:

```bash
videre watch ~/Photos --heic     # then Ctrl-C once it settles
videre report --show-faces
```

Conversion uses `qlmanage`, not `sips`, because some iPhone HEIC files encode
rotation in a way `sips` ignores, producing sideways images.

## Caveats

**Static reports link to your files with `file://`.** That works on the machine
that generated them. Sent to someone else, the page loads but every image is
broken, unless you used `--heic`, which inlines the data.

**Server mode serves image bytes over HTTP**, through an endpoint that only
serves paths already present in the database. It is an allowlist, not a general
file server, but it does mean anything that can reach `localhost:7878` can read
those images.

**The page is a snapshot.** A static report reflects the database at generation
time. After deleting duplicates, regenerate it.

**Only one report server at a time.** Both `--faces` and `--show-faces` bind
7878, so a second one fails to start while the first is running.

## More detail

- [Browsing and labeling](/guides/browsing/) covers the three interfaces this produces, which flags give a file and which start a server, and the labeling workflow in depth.
