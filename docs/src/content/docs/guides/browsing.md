---
title: Browsing and labeling in a browser
description: The three different interfaces videre dedupe --html produces, and which one you get.
---

`videre dedupe --html` produces three quite different things depending on its flags:
a static file you can keep, a labeling application, and a live gallery. They
share a command but are not really the same tool.

| You want to | Command | What you get |
|---|---|---|
| Check duplicates before deleting | `videre dedupe --html` | A file |
| Browse everything you own | `videre gallery` | A file |
| Find photos from a trip | `videre gallery` | A file |
| Name people | `videre gallery` | A local server |
| Browse with names and places shown | `videre gallery` | A local server |

## Static file or local server

One rule decides it:

:::note[`--faces` or `--show-faces` means server. Anything else means a file.]
Either flag starts a server on `localhost:7878` and writes nothing. Without
both, you get a self-contained HTML file and no server.
:::

That is worth internalising, because the flags combine freely and the result is
not always what the names suggest:

```bash
videre gallery --show-faces --heic
```

That is **server mode**. `--show-faces` wins, `--all` adds the gallery, and
`--heic` is silently ignored, since server mode converts HEIC lazily per request
instead of embedding it up front.

### Why the difference exists

A static file has to contain everything, because it is opened via `file://` with
nothing running. That makes it portable and permanent, but it cannot show
anything computed on demand.

Named faces and place names are looked up per photo, so they need a backend.
That is the whole reason `--show-faces` starts a server rather than baking
results into a file.

It also changes how images load. A static report links to your files with
`file://`. A served page cannot, because browsers refuse to load a `file://`
subresource from an `http://` page, so image bytes come through an endpoint
instead.

## Reviewing duplicates

The default, and the one to run before deleting anything.

```bash
videre dedupe --html
```

Groups are sorted by wasted space, each file badged KEEP or REMOVE, with
thumbnails, sizes, dates, GPS and paths. Expand or collapse all, and re-sort by
wasted space or by date.

This is the same grouping and the same KEEP choice that
[`videre dedupe`](/commands/dedupe/) will print, so what you see is what will
happen. Since group members are byte-identical, what you are really reviewing is
**which path survives**, which matters most when one copy is on a drive you
think of as the backup.

Near-duplicates from [`scan --similar`](/commands/scan/) appear here for
eyeballing, and deliberately never in `dedupe`'s pipeable output.

## Naming people

```bash
videre faces                   # detect and group first
videre gallery          # then name them
```

Opens `http://localhost:7878` with three sections:

- **People**, ones you have named
- **Unassigned clusters**, groups it is confident about but has no name for
- **Singletons**, faces it could not group

Drag a cluster's handle onto a person card to assign it, or click **New Person**
to create one from it. One drag can name forty photos, which is the whole point.

Each cluster links to a detail page showing every face full size, with per-face
remove and assign for the odd wrong member. **Dissolve cluster** breaks a
wrongly-merged group back into singletons without deleting any faces, and is
usually a better fix than retuning the whole clustering.

Clicking a face opens the full-resolution original, served by the backend rather
than linked, since a page on `http://` cannot navigate to `file://`.

Names are written back as you go. Stop with Ctrl-C or **Save & Close**, then
[`search --person`](/commands/search/) works.

Retuning later with [`faces --recluster`](/commands/faces/) does **not** lose
names, because they are stored per face rather than per group.

## Live browsing

```bash
videre gallery
```

The report itself, but served, with the lightbox showing each photo's named
faces (clicking one jumps to that person) and a reverse-geocoded place name.

Combine as you like:

```bash
videre gallery --show-faces      # whole library, live metadata
videre gallery    # report at /, labeling UI at /faces
```

Route split when combining:

| Flags | `/` serves | `/faces` |
|---|---|---|
| `--faces` | Labeling UI | not routed |
| `--show-faces` | Live report | not routed |
| Both | Live report | Labeling UI |

### HEIC is faster here than it looks

Server mode never converts HEIC up front. Thumbnails are produced per request
and taken from the [cache](/guides/caches/) when present, which is why the page
opens immediately instead of taking minutes on a HEIC-heavy library.

Warm the cache first and the first click is fast too:

```bash
videre watch ~/Photos --heic     # Ctrl-C once counts settle
videre gallery
```

## Sharing a report

Static reports link to your photos with `file://`, which resolves only on the
machine that generated them. Sent to someone else, the page loads and every
image is broken.

To produce something that travels, embed the images:

```bash
videre gallery --heic -o for-sharing.html
```

`--heic` inlines 240px thumbnails; `--heic-original` adds 1200px lightbox
versions. Both make the file substantially larger, since everything is base64
encoded, and both are macOS only. That size is the price of a page that works
anywhere with nothing running.

## Caveats

**One server at a time.** Both `--faces` and `--show-faces` bind port 7878, so a
second invocation fails while the first is running.

**Served image bytes come from an allowlist.** The endpoint only serves paths
already recorded in the database, so it is not a general file server. It does
mean anything that can reach `localhost:7878` can read those images while it is
running.

**A static report is a snapshot.** It reflects the database when generated.
After deleting duplicates, regenerate it.

**Files missing from disk are excluded** at generation time, without changing the
database. [`videre prune`](/commands/prune/) removes those rows for good.

**The similarity button needs [`videre embed`](/commands/embed/).** Without it,
`--all` still works and the button is disabled with a note rather than failing.

**Nothing leaves your machine.** Both server modes bind locally, and the pages
make no external requests.
