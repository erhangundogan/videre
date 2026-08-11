---
title: videre prune
description: Clean up database entries for files you have deleted. Never touches real files.
---

Syncs the database with what is actually on disk. It **never deletes real
files**: only database rows, and cached data derived from photos that are
already gone.

```bash
videre prune --dry-run                 # show what would be removed
videre prune                           # remove stale entries and refresh metadata
videre prune --silent                  # no per-file output
videre prune --db ~/photos.db          # use a specific database
videre prune --prune-unreachable       # also drop entries whose folder is gone
videre prune --force                   # allow an unusually large cleanup
```

## When to run it

The usual moment is right after deleting duplicates:

```bash
videre dedupe | xargs trash    # files are gone from disk...
videre prune                   # ...now the database agrees
```

Until you do, `videre stats` still counts the deleted files, and reports still
list them (they are filtered out at generation time, but the rows remain).

Also worth running after moving or reorganising folders by hand, since rows are
keyed by path: a moved file looks like a deletion plus a new file, and the old
row lingers until pruned.

Always look first on a library you care about:

```bash
videre prune --dry-run
```

## What one pass does

1. Removes rows for files no longer on disk
2. Refreshes timestamps for files that are still there
3. Deletes embeddings whose photo is gone, across **every**
   [model](/reference/models/)
4. Deletes [cached thumbnails](/reference/paths/#thumbnail-cache) whose photo is
   gone

Steps 3 and 4 are the reason to prune at all rather than ignoring stale rows:
they are what actually reclaims disk space.

If two paths share the same content and only one is deleted, the shared
embedding and cache entries are **kept**. They are keyed by content, so they are
still in use by the surviving copy.

:::note[`--dry-run` undercounts orphans]
The orphan counts in a dry run only include entries that are *already* orphaned,
not the ones the pending row deletions would create. The real run usually
reclaims more than the preview suggests. Row counts are exact.
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
[scanning more than one folder](/reference/paths/#scanning-more-than-one-folder).

**It is the only thing that reclaims cache space**, and only for photos already
removed from the database. Cache for photos you still own grows without bound
and is never touched here.

**`videre watch --prune` can override neither guard.** It runs unattended and
cannot ask, so bulk deletion and repeated failure remain active. That makes it
safe to leave on, but it also means an unattended prune can quietly decline to
do the thing you wanted; check `videre stats` if you expected space back.

**Exits nonzero if any row update or cache removal failed**, which is worth
checking in a script.
