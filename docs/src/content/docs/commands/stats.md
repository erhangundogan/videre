---
title: videre stats
description: "Library inventory: counts, sizes, disk use. Purely informational."
---

```bash
videre stats                           # library totals and inventory
videre stats --json                    # print one JSON object instead
videre --library ~/Photos stats        # select a different library
```

Pipeline health, staleness, watch liveness and what to run next live in
[`videre status`](/commands/status/), not here: `stats` is what is *in* the
library, `status` is what state the *pipeline* is in.

## Reading the output

```
Library: 70601 file(s) (398.9 GB), 57142 photo(s), 13459 video(s)
Duplicates: 0 group(s), 0 file(s), 0 B wasted
Faces: 58555 detected, 86 people named
Marks: 312 rated, 40 picked, 12 labelled, 128 liked

Embeddings:
  google/siglip-base-patch16-224            70588   768-dim   128.3 MB
  google/siglip-so400m-patch14-384          70587  1152-dim   189.6 MB
  google/siglip2-base-patch16-384           70588   768-dim   128.3 MB

By type:
  mov      video/quicktime             12564   165.4 GB
  jpeg     image/jpeg                  43524   142.4 GB
  mp4      video/mp4                     643    62.0 GB
  heic     image/heic                  10395    20.8 GB
  png      image/png                    3211     6.5 GB
  mov      video/mp4                     251   909.5 MB
  mp4      video/quicktime                 1   462.5 MB
  dng      image/tiff                      9   433.9 MB
  gif      image/gif                       2     4.8 MB
  png      image/jpeg                      1    43.7 KB

Disk use:
  embeddings           446.3 MB
  database             161.8 MB
  thumbnails            84.1 MB  (rebuildable)
  place names           11.4 MB  (rebuildable)
  database journal      32.0 KB  (rebuildable)
  total                703.7 MB  (95.6 MB of it rebuildable)

```

Real output from a 70,000-file library, so the awkward rows are the interesting
ones.

**By type** groups the library by file extension, with the mime type beside it
and the largest first. These are the values `--ext` and `--mime` take, so this is
where to look before scoping a run.

**Extension and mime disagree more often than people expect**, and both are shown
rather than reconciled. Three rows above are the same file types filed under a
different name: 251 `.mov` files holding MP4 video, one `.mp4` holding QuickTime,
and one `.png` that is really a JPEG. Reconciling them would mean choosing which
of the two to lie about, so `--ext mov` and `--mime video/quicktime` select
genuinely different sets.

**Disk use** is what videre itself stores, not your photos, largest first.
Entries marked `(rebuildable)` cost only time to recreate: thumbnails are
re-converted on demand, place names ship with videre, and the database journal is
transient. The database and embeddings are not rebuildable - an embedding run
takes hours, and the database cannot be recreated without rescanning.

Note the proportions above: 703.7 MB of videre data describing 398.9 GB of
photos, and only 95.6 MB of it disposable. **Embeddings dominate** because that
library has three models prepared.

:::note[Embeddings are counted per library]
They live under this library's own `.videre/embeddings/`, so `stats` reports
only the ones belonging to the library you asked about. Another library's
embeddings live under its own root and never appear here. See
[keeping libraries separate](/guides/multiple-libraries/).
:::

**Duplicates** counts exact copies only: the same pixels or media data, even when
their metadata differs. "Wasted" is what you
would reclaim by deleting all but one of each group, which is what
[`videre dedupe`](/commands/dedupe/) proposes.

**Faces detected** counts individual faces, not photos, so one group shot
contributes several - 58,555 faces across 57,142 photos above. **People named**
stays at 0 until you assign names in [`gallery`](/commands/gallery/); detection
alone never produces a name, however many faces it finds.

**Embeddings** lists one line per [model](/reference/models/) you have prepared.
The dimensions come from the stored data rather than a hardcoded table, so an
unfamiliar model still reports honestly - which is why the 1152-dimension
`so400m` line above needs no special-casing to appear correctly.

The counts differ slightly between models (70588, 70587, 70588) because each run
skipped whatever it could not decode at the time. A count below the library total
is normal, not a sign of a failed run.

## Numbers that look wrong but are not

**Photos plus videos may not equal the total.** In the example, 151 + 52 = 203
against 204 files. The missing one is a file whose type could not be identified
from its bytes, so it counts toward the total but is neither a photo nor a
video.

**The embedding count is lower than the file count**, and should be. Vectors are
keyed by content, so duplicate copies share one, and `.dng` files are never
embedded at all. A gap here is normal; a gap of thousands means
[`videre embed`](/commands/embed/) has more to do.

**`duration=0ms` is not an error.** Commands reading an already-warm database
genuinely finish in under a millisecond.

**A command can show `success` and still have exited nonzero.** Per-item
problems, a few unreadable files or one corrupt image, do not fail a run. Only
an unhandled error does. `fix-dates` and `faces` both return a count of problems
rather than failing outright.

## Pipeline health moved to status

Per-command last-run lines, the status table (`success`, `failed`,
`interrupted`, `crashed`), watch liveness, and the `--check` exit code for
cron all live in [`videre status`](/commands/status/) now. Keeping them in
both places would let the two surfaces disagree; one home, and it is
`status`.
