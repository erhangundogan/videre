---
title: Cautions
description: The parts of videre that change something, and the situations that surprise people.
---

Most of videre is read-only. These are the parts that are not, plus the
situations that surprise people.

## `videre dedupe trash` deletes files

`videre dedupe trash` moves the duplicate copies to the system trash
(recoverable), and its plain output is the REMOVE side of each group. `trash`
asks before deleting (unless `--yes`), previews with `--dry-run`, and handles
paths with spaces correctly, as does `delete`. Use them rather than piping
`videre dedupe` into another tool, which splits every path on its spaces (a
script that must pipe uses `--print0 | xargs -0`).

Look before you delete: run [`videre dedupe review`](/commands/dedupe/) first and review
the KEEP/REMOVE badges, or use `trash --dry-run`.

Changed your mind? [`videre dedupe undo`](/commands/dedupe/#undoing-a-trash-run)
puts back the most recent `trash` run, newest first, one run per call, until
the trash is emptied.

`videre dedupe delete` removes the same copies **permanently**: nothing goes
to the trash, and it cannot be undone, not even by `undo`. It asks with that wording before it
starts; use it only once a `trash --dry-run` lists what you expect.

Near-duplicate groups from `--kind similar` are deliberately kept out of this output,
because they are for review by eye, not for automatic deletion.

## Keep your photos connected when running `prune`

[`videre prune`](/commands/prune/) and `videre watch --prune` delete database
rows for files they cannot find on disk. If your library lives on an external
drive and that drive is unplugged, every file looks missing.

videre guards against exactly this: a row is only removed when the file is
missing **and** its parent folder still exists. A missing folder means the drive
or directory is gone, not that you deleted the photos, so those rows are kept
and reported:

```
12,431 row(s) skipped as unreachable (1 directory missing: /Volumes/Photos)
  run with --prune-unreachable to remove them anyway
```

That matters because removing those rows would also throw away their search
embeddings and cached thumbnails, which take hours to rebuild. Your photos would
be untouched, but faces, names, and locations are stored against those rows and
would go with them.

## `videre import` also changes file timestamps

[`videre import`](/commands/import/) sets each file's date from whatever the
exporting tool recorded, which is the point of running it. Like `fix-dates` it
asks for confirmation first, and `--dry-run` shows exactly what it would do.
Only the modification time changes; contents are never touched.

It reads other applications' libraries, and only reads: nothing is written back
to a Photos library or a Lightroom catalog.

## `videre fix-dates` rewrites file timestamps on disk

It sets each file's modification time from its EXIF date. That is a real change
to your files and there is no undo. It asks for confirmation first, and
`--dry-run` shows you exactly what it would do.

## Disk use grows quietly

Each search model keeps its own data, roughly 130 MB to 190 MB per model for a
70,000 photo library, and the HEIC thumbnail cache can reach tens of GB.

Only `videre prune` reclaims any of it, and nothing warns you first.
`videre stats` shows what each model is using.

## A command acts on the directory you run it in

There is no remembered library and no default folder. `videre scan` with no
`--library` scans the current directory; run it from the wrong place and it
initializes a library there. `videre --library <dir>` makes the target explicit
from anywhere.
