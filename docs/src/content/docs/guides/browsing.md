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

The command decides it, not a flag:

:::note[`gallery` is a server. `--html` writes a file.]
[`videre gallery`](/commands/gallery/) serves on `localhost:7878` and writes
nothing. [`dedupe --html`](/commands/dedupe/) and
[`search --html`](/commands/search/) write a self-contained page and start
nothing.
:::

Until 0.18.0 this was decided by which combination of flags you passed to one
command, and combining them needed a table to explain which won. Splitting them
into separate commands is what removed that question.

### Why the difference exists

A static file has to contain everything, because it is opened via `file://` with
nothing running. That makes it portable and permanent, but it cannot show
anything computed on demand.

Named faces and place names are looked up per photo, so they need a backend.
That is the whole reason `gallery` is a server rather than something that bakes
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

One server, every view on its own route. The lightbox shows each photo's named
faces, clicking one jumps to that person, and places are reverse-geocoded on
demand.

| Route | Shows |
|---|---|
| `/` | Every file |
| `/people` | Face groups, and where you name them |
| `/date` | Year, month, day drill-down |

A strip along the top of every page switches between the three, so none of them
needs to be typed.

Those used to be flags on a single page, and combining them needed a table to
explain which one won. Routes need no such explanation, which is why they
replaced the flags in 0.18.0.

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
videre dedupe --html for-sharing.html
```

The page links to your files by path, so it is small but only complete on the
machine holding them. Embedding the images instead is not currently offered:
`report --heic` used to do it, and went with that command in 0.20.0.

## Caveats

**One server at a time.** `gallery` binds port 7878 by default, so a second
invocation fails while the first is running. Give it `--port` to run two at
once, against different libraries.

**Served image bytes come from an allowlist.** The endpoint only serves paths
already recorded in the database, so it is not a general file server. It does
mean anything that can reach `localhost:7878` can read those images while it is
running.

**A static report is a snapshot.** It reflects the database when generated.
After deleting duplicates, regenerate it.

**Files missing from disk are still listed.** A static page reads the database,
so a photo deleted outside videre appears until
[`videre prune`](/commands/prune/) removes its row.

**Similarity search needs [`videre embed`](/commands/embed/), and lives only in
`gallery`.** A static page cannot carry it: matching a query against every
vector needs the database, which an exported file does not have. Without
embeddings, `gallery` still serves every view and says so rather than failing.

**Nothing leaves your machine.** The server binds to `127.0.0.1`, and the pages
make no external requests.
