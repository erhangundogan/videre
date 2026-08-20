---
title: Cautions
description: The parts of videre that change something, and the situations that surprise people.
---

Most of videre is read-only. These are the parts that are not, plus the
situations that surprise people.

## `videre dedupe` prints files to delete

Its output is the REMOVE side of each duplicate group, so
`videre dedupe | xargs trash` deletes those files immediately.

Look before you pipe: run [`videre dedupe --html`](/commands/dedupe/) first and review
the KEEP/REMOVE badges, or send the list to a file and read it.

Near-duplicate groups from `--similar` are deliberately kept out of this output,
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

## Do not run two heavy commands at the same time

`embed`, `faces`, and `watch` all convert HEIC and video through macOS
QuickLook, and each limits itself to a few conversions at a time. That limit is
per command, not system-wide, so two at once can overwhelm QuickLook.

Measured on a real library: a single file took over 16 seconds against about 7.6
seconds normally, and one exceeded the timeout entirely. Nothing is lost, since
skipped files are simply retried next run, but it is much slower than doing one
thing at a time.

## Disk use grows quietly

Each search model keeps its own data, roughly 130 MB to 190 MB per model for a
70,000 photo library, and the HEIC thumbnail cache can reach tens of GB.

Only `videre prune` reclaims any of it, and nothing warns you first.
`videre stats` shows what each model is using.

## `videre scan` remembers the first folder you give it

It adopts that folder as your default so later commands can be run without
repeating it. It says so when it happens, and `videre config set path` changes
it.
