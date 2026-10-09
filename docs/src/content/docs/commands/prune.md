---
title: videre prune
description: Clean up database entries for files deleted or moved outside videre. Never touches real files.
---

Syncs the database with what is actually on disk. It **never deletes real
files**: only database rows, and cached data derived from photos that are
already gone.

```bash
videre prune --dry-run                 # show what would be removed
videre prune                           # remove stale entries and refresh metadata
videre prune --silent                  # no per-file output
videre --library ~/Photos prune        # select a different library
videre prune --prune-unreachable       # also drop entries whose folder is gone
videre prune --force                   # allow an unusually large cleanup
```

## When to run it

After a change made outside videre. videre's own commands clean up after
themselves: [`dedupe trash`](/commands/dedupe/), `dedupe delete` and the
gallery's Delete and Trash run this pass for what they remove, and [`fix-dates`](/commands/fix-dates/)
deletes nothing and keeps each row's stored time in step with the file. Run it
by hand when:

- **you deleted files** in Finder, the Photos app or any other tool. The files
  are gone, but their rows stay;
- **you moved, renamed or reorganised folders**. Rows are keyed by path, so a
  moved file looks like a deletion plus a new file: [`scan`](/commands/scan/)
  records the new path, and the old row lingers until pruned;
- **a folder is gone for good**, such as one on a drive you no longer have:
  `videre prune --prune-unreachable`. An unreachable folder is otherwise left
  alone, in case its drive is only unplugged.

Until then, `videre stats` still counts the files, and reports still list them
(they are filtered out at generation time, but the rows remain).

[`videre watch`](/commands/watch/), and so the gallery, does this for you on
its startup and hourly passes, unless started with `--no-prune`. Only a folder
gone for good still needs you, since watch never passes
`--prune-unreachable`.

Always look first on a library you care about:

```bash
videre prune --dry-run
```

## What one pass does

1. Removes rows for files no longer on disk
2. Refreshes the stored timestamp of any file whose modification time has
   changed since the last pass
3. Removes face detections and face-scan markers whose content has no indexed
   path. Person names and the learning journal remain; evidence from removed
   faces becomes ineligible, pending questions are withdrawn, and an active
   learned profile is retired if an eligible event lost its source
4. Deletes embeddings whose photo is gone, across **every**
   [model](/reference/models/), and the photo's marks and the pixel
   signature [dedupe](/commands/dedupe/) keeps to confirm resized copies
5. Deletes [cached thumbnails](/guides/caches/#thumbnail-cache) whose photo is
   gone
6. Removes any [model database](/reference/models/) the sweep has left with no
   embeddings at all, typically one an interrupted or failed `videre embed`
   left behind

Steps 4 and 5 reclaim derived data on disk. Step 3 keeps the People page and
face learning consistent with the indexed files.

Step 2 only touches rows that actually differ, so the count tells you something.
Its usual cause is another program changing a file's modification time.
[`videre fix-dates`](/commands/fix-dates/) is not one: it records the times it
writes. On a library nothing has touched, the count is zero and a second pass
is a no-op.

If two paths share the same content and only one is deleted, the shared
embedding and cache entries are **kept**. They are keyed by content, so they are
still in use by the surviving copy. The same rule protects faces and face-scan
markers.

:::note[`--dry-run` undercounts orphans]
The orphan counts in a dry run, including faces, scan markers, affected learning
events and questions, only include entries that are *already* orphaned,
not the ones the pending row deletions would create. The real run usually
reclaims more than the preview suggests. Row counts are exact. The same lower
bound applies to empty model databases: one whose only embeddings are about to
be orphaned still counts as non-empty in the preview.
Dry-run opens the database read-only and records no pipeline run. A schema-3
library must first be upgraded by a writable command such as `videre scan`.
:::

## If a drive is not plugged in, prune leaves it alone

A row is only removed when the file is missing **and** its parent folder still
exists. A missing folder means the drive or directory is gone, not that you
deleted the photos:

```
12,431 row(s) skipped as unreachable (1 directory missing: /Volumes/Photos)
  run with --prune-unreachable to remove them anyway
```

This is a data-safety guard, not tidiness. prune used to treat every unreadable
file as deleted, so running it with a drive unplugged removed every row for that
drive. The rows are the cheap part: once they are gone, their embeddings and
cached thumbnails look orphaned and the sweeps take those too. That is hours of
recompute against minutes to re-scan.

Consequences worth knowing:

- **Skipped rows keep everything downstream safe.** Protecting the row protects
  its embeddings and thumbnails automatically.
- **The skip count prints even under `--silent`**, and names up to 5 missing
  directories. A run that quietly skips thousands of rows is exactly the silence
  this fixes.
- **A deliberately deleted subfolder is skipped too**, and its rows linger until
  you pass `--prune-unreachable`. The conservative direction is the safe one.

Use `--prune-unreachable` when a folder really is gone for good, having checked
that the drive is not merely unmounted.

## Two further guards

**Bulk deletion.** A run removing more than 20% of the library *and* at least 100
rows stops before deleting anything, and needs `--force`. Both conditions are
required: a percentage alone would block a five-row library where three files
were legitimately deleted, and a count alone would never trip on a small one.
This catches what the folder check misses, such as a volume that remounts empty.

**Repeated failure.** After 10 consecutive errors the run stops, printing the
first error rather than one near-identical line per row. Consecutive, not
cumulative, so a few scattered unreadable files do not abort a good run.

Earlier changes stay committed, and prune is idempotent, so rerunning after
fixing the cause continues safely. That is also why hitting a guard is not a
problem: nothing is half-done in a way a second run cannot finish.

## Caveats

**It acts on the whole database, not a folder.** If you scanned several roots
into one database, prune considers all of them, and there is no way to limit it
to one. See
[scanning more than one folder](/guides/multiple-libraries/).

**It is the only thing that reclaims cache space**, and only for photos already
removed from the database. Cache for photos you still own grows without bound
and is never touched here.

**Watch's prune pass can override neither guard.** It runs unattended and
cannot ask, so bulk deletion and repeated failure remain active. That makes it
safe to leave on, but it also means an unattended prune can quietly decline to
do the thing you wanted; check `videre stats` if you expected space back.

**Exits nonzero if any row update or cache removal failed**, which is worth
checking in a script.

## More detail

- [Caches and disk use](/guides/caches/) covers everything this can and cannot reclaim.
- [Backing up](/guides/backup/) covers what is worth keeping before a large cleanup.
