---
title: Where your data lives
description: The home directory, how a database is resolved, and what VIDERE_HOME changes.
---

Everything videre creates lives in one place:

```
~/.videre/
  hashes.db      # the database
  config.toml    # your defaults
  hashes.jsonl   # only if you use `scan --output`
  locks/         # marks which command is currently running
  embeddings/    # per-model search data
  cache/         # thumbnails, when VIDERE_HOME is set
```

Nothing is created until you actually write something. Commands that only read
never create a directory or a database.

## How a database is resolved

Every command resolves its database the same way, first match wins:

1. An explicit `--db <path>`
2. `default_db` in `config.toml`, set via `videre config set db`
3. `~/.videre/hashes.db`

Readers never create a database. If the resolved path does not exist they print
this and exit nonzero, rather than silently creating an empty one:

```
no database found at <path>; run 'videre scan <dir>' first
```

Run `videre config` at any time to see what everything currently resolves to.

## Scanning more than one folder

Nothing stops you pointing `videre scan` at several folders in turn:

```bash
videre scan ~/Photos
videre scan /Volumes/Archive/Photos
```

Both end up in the **same database**, as rows in one table. There is no notion
of a library inside a database: a database *is* the library, however many roots
were scanned into it.

That is often exactly what you want. A collection split across an internal disk
and an external drive is one library, and treating it as one is the point.

### Everything then works database-wide, not folder-wide

This is the part that surprises people. No command takes a folder to limit
itself to:

| Command | What it acts on |
|---|---|
| [`dedupe`](/commands/dedupe/) | Every row, so it finds copies **across** folders |
| [`prune`](/commands/prune/) | Every row, whichever folder it came from |
| [`report`](/commands/report/), [`search`](/commands/search/), [`stats`](/commands/stats/) | Everything in the database |

Cross-folder duplicate detection is usually the reason to combine folders: it is
how you find that the archive drive holds copies of what is already on your
laptop. But it means `videre dedupe` may propose deleting a file in a folder you
were not thinking about, and the KEEP copy may live on the other drive.

Review before piping, and remember that KEEP is chosen by oldest EXIF date, not
by which folder you prefer.

### The stale-folder trap

`videre scan` with no argument scans your configured default folder, which is
the **first** folder you ever scanned:

```bash
videre scan ~/Photos                      # adopts ~/Photos as the default
videre scan /Volumes/Archive/Photos       # scanned, but does not change the default
videre scan                               # refreshes ~/Photos only
```

After that third command, the archive's rows are still there but no longer
match what is on disk. Files you deleted from it are still listed, and files you
added are missing.

Keep each root explicit when you have more than one:

```bash
videre scan ~/Photos
videre scan /Volumes/Archive/Photos
```

The same applies to [`videre watch`](/commands/watch/), which takes a single
folder. Watching two roots means running two `watch` processes, and they should
not run their HEIC or faces stages simultaneously.

### Keeping folders genuinely separate

If two collections should not see each other at all, give them their own
databases rather than sharing one:

```bash
videre scan --db ~/personal.db ~/Photos
videre scan --db ~/work.db ~/WorkShoots

videre dedupe --db ~/work.db          # only ever considers work photos
```

Separate databases also get separate embeddings and separate locks, so the two
never interfere. See
[keeping libraries separate](/guides/multiple-libraries/) for what is still
shared between them, and when `VIDERE_HOME` is the better tool.

:::caution[An unplugged drive is handled, but know the rule]
With several roots in one database, some of them may be offline at any time.
[`videre prune`](/commands/prune/) only removes a row when the file is missing
**and** its parent folder still exists, so an unmounted drive is skipped rather
than wiped. It reports how many rows it skipped and which directories were
missing.
:::

## `VIDERE_HOME`

Setting `VIDERE_HOME` moves the entire home directory, so the database, config,
locks, embeddings and cache all relocate together.

This is what makes a separate library genuinely separate: pointing
`VIDERE_HOME` at another directory gives you an independent config, an
independent default database, and independent locks. Work done under one home
does not affect the other.

```bash
VIDERE_HOME=~/videre-test videre scan ~/test-photos
```

:::note
`VIDERE_HOME` and `videre config` are different mechanisms. The environment
variable chooses *which* home directory to use; `config.toml` lives *inside*
whichever home is selected and sets defaults within it. Changing one home's
config never touches another's.
:::

## Locks

Each database gets one lock file per command, under `<home>/locks/`. This is
what lets `videre stats` report a command as currently running, and what stops
the same command running twice against one database.

Lock names include a hash of the database's canonicalised path, not just its
filename. Two libraries can both be called `photos.db` in different folders, and
keying on the name alone would make them share a lock, silently serializing
unrelated work. Canonicalising first also means a symlink and a relative path to
the same database resolve to the same lock.

Two *different* commands can run at once against the same database.

## Environment variables

| Variable | Effect |
|----------|--------|
| `VIDERE_HOME` | Use a different home directory instead of `~/.videre` |
| `VIDERE_EMBED_DTYPE` | `f16` for slightly faster search preparation. Does not affect existing data. |

Model choice is deliberately not an environment variable. Use
`videre config set model <id>`, or `--model <id>` for a single command.

## Thumbnail cache

Decoded images are cached so a slow HEIC conversion happens once rather than
every time.

```
~/.cache/videre/thumbnails/          # normally
<VIDERE_HOME>/cache/thumbnails/      # when VIDERE_HOME is set
```

Keyed by content hash, so the same photo in two databases is converted once.
This is the one store that reaches **tens of GB**, because it keeps a
full-resolution decode per HEIC file, and it has no size limit or expiry. Only
[`videre prune`](/commands/prune/) reclaims anything, and only for photos no
longer in the database.

Deleting the directory is safe; everything in it regenerates on demand.

[Caches and disk use](/guides/caches/) covers all three caches, what each costs
to lose, and how to work through reclaiming space.
