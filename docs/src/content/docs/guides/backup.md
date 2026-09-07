---
title: Backing up
description: What is worth backing up, what regenerates, and how to copy a live database safely.
---

Your photos are the thing that matters, and videre never touches them. This is
about the work videre produces *about* them.

## What is worth what

| | Cost to rebuild | Back up? |
|---|---|---|
| **Names you assigned to faces** | Manual, cannot be automated | **Yes, above all** |
| Face detections and groupings | Hours of compute | Yes, same file |
| Embeddings | Hours of compute, per model | Worth it |
| Scan rows, classifications, locations | Minutes | Comes along free |
| Thumbnail cache | Milliseconds each | No |
| Model weights | A download | No |

The asymmetry is the point. Everything except the names can be recomputed by
leaving a machine running overnight. **The names cannot**: you sat and assigned
them, and no rerun brings them back.

## What to copy

Run these from inside the library (the directory whose `.videre/` you are
backing up):

```
.videre/hashes.db          # the database: names, faces, scan rows, everything
.videre/config.toml        # small, and annoying to reconstruct
.videre/embeddings/        # optional, saves hours of recompute
```

Skip `~/.cache/videre/` and `~/.cache/huggingface/`. Both are
derived, both regenerate, and both are large.

If you only back up one thing, make it `hashes.db`. It holds the names.

## Copying a live database safely

The database is in WAL mode, so at any moment some committed data lives in a
side file rather than the main one. A plain `cp` while something is writing can
capture a torn state.

Use SQLite's own backup, which is safe on a live database:

```bash
sqlite3 .videre/hashes.db ".backup /backups/videre-$(date +%F).db"
```

Or, to get a compacted copy:

```bash
sqlite3 .videre/hashes.db "VACUUM INTO '/backups/videre-$(date +%F).db'"
```

Both produce a single self-contained file with no `-wal` or `-shm` alongside.

If you would rather just copy files, stop any running
[`videre watch`](/commands/watch/) first and copy the `-wal` and `-shm` files
too if they exist:

```bash
cp .videre/hashes.db* /backups/
```

Embeddings are ordinary SQLite files as well, so the same applies:

```bash
cp -r .videre/embeddings /backups/
```

## A whole-setup backup

```bash
#!/bin/sh
set -e
DEST="/backups/videre/$(date +%F)"
mkdir -p "$DEST"
sqlite3 .videre/hashes.db ".backup $DEST/hashes.db"
cp .videre/config.toml "$DEST/" 2>/dev/null || true
cp -r .videre/embeddings "$DEST/" 2>/dev/null || true
```

Run it after a labeling session, which is when the irreplaceable part changes.

## Restoring

Put the files back and carry on:

```bash
cp /backups/videre/2026-08-11/hashes.db .videre/hashes.db
cp -r /backups/videre/2026-08-11/embeddings .videre/
videre stats                      # confirm it reads
```

If the photos moved to different paths, re-scan and prune:

```bash
videre --library /new/location/Photos scan
videre prune
```

Faces, names, embeddings and classifications all survive that, because they are
keyed by **content hash** rather than by path. Only the paths change. This is
also why restoring onto a different machine works, as long as the files are the
same files.

## Verifying a backup

A backup you have never restored is a hypothesis. Check the file itself with
SQLite's integrity check, which needs no videre and touches nothing else:

```bash
sqlite3 /backups/videre/2026-08-11/hashes.db "PRAGMA integrity_check;"
```

To test it functionally, restore it into a copy of its **original** library
root and run `stats` there:

```bash
mkdir -p /tmp/restore-test/.videre
cp /backups/videre/2026-08-11/hashes.db /tmp/restore-test/.videre/hashes.db
# only works if the library was rooted at /tmp/restore-test; see the caution below
videre --library /tmp/restore-test stats
```

If `stats` reports the file counts and people you expect, the backup is good.

:::caution[A database is bound to its library root]
videre refuses a database whose stored paths fall outside the library root it
is opened under, so you cannot verify a backup by dropping the database alone
into an arbitrary directory. Restore into the original root, or copy the whole
`.videre/` tree together, so the paths and the root still agree.
:::

## Caveats

**videre is not a photo backup tool.** It records paths and hashes, not
contents. A restored database plus missing photos gives you a detailed
description of files you no longer have. Back up the photos separately, and
first.

**Names live on individual face rows**, not on groups, which is why re-running
[`faces --recluster`](/commands/faces/) never loses them. It is also why the
database is the only thing that needs backing up to preserve them.

**Embeddings are per library and per model.** Restoring a database without its
embeddings directory leaves search empty until you re-run
[`videre embed`](/commands/embed/). Nothing is broken; it is just hours of work
you already did once.
