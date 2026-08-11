---
title: videre prune
description: Clean up database entries for files you have deleted. Never touches real files.
---

Syncs the database with what is actually on disk. Run it after deleting
duplicates. It never touches real files, only database rows and cached data.

```bash
videre prune --dry-run                 # show what would be removed
videre prune                           # remove stale entries and refresh metadata
videre prune --silent                  # no per-file output
videre prune --db ~/photos.db          # use a specific database
videre prune --prune-unreachable       # also drop entries whose folder is gone
videre prune --force                   # allow an unusually large cleanup
```

In a single pass it removes rows for files no longer on disk, refreshes
timestamps for surviving files, and deletes now-orphaned search embeddings
(across every model) and cached thumbnails.

If two paths share the same content and only one is deleted, the shared
embedding and cache entries are kept.

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
cached thumbnails look orphaned and the cleanup sweeps take those too. That is
hours of recompute against minutes to re-scan.

Consequences worth knowing:

- **Skipped rows keep everything downstream safe.** Protecting the row protects
  its embeddings and thumbnails automatically.
- **The skip count prints even under `--silent`**, and names up to 5 missing
  directories. A run that quietly skips thousands of rows is exactly the silence
  this fixes.
- **A deliberately deleted subfolder is skipped too**, and its rows linger until
  you pass `--prune-unreachable`. The conservative direction is the safe one.

## Two further guards

**Bulk deletion.** A run removing more than 20% of the library *and* at least 100
rows stops before deleting anything, and needs `--force`. Both conditions are
required: a percentage alone would block a five-row library where three files
were legitimately deleted, and a count alone would never trip on a small one.
This catches what the folder check misses, such as a volume that remounts empty.

**Repeated failure.** After 10 consecutive errors the run stops, printing the
first error rather than emitting one near-identical line per row. Consecutive,
not cumulative, so a few scattered unreadable files do not abort a good run.
Earlier changes stay committed, and prune is idempotent, so rerunning after
fixing the cause continues safely.

`videre watch --prune` can override **neither** guard: it runs unattended and
cannot ask.
