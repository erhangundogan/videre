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
never interfere. Set one as the default and pass `--db` for the other, or use
`VIDERE_HOME` below to switch the whole context at once.

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

Converting a HEIC file is slow, about 7.6 seconds each, so decoded images are
cached and reused. This is the one piece of videre's storage that can grow to
tens of gigabytes, and the one most worth understanding.

### Where it lives

```
~/.cache/videre/thumbnails/          # normally
<VIDERE_HOME>/cache/thumbnails/      # when VIDERE_HOME is set
```

Files are named by **content hash**, not path:

| File | What it is |
|---|---|
| `<hash>_240.jpg` | Grid thumbnail |
| `<hash>_1200.jpg` | Lightbox size |
| `<hash>_original.jpg` | **Full resolution decode** |
| `<hash>_face<id>_<size>.jpg` | A cropped face |
| `<hash>_*.tmp<pid>` | A write in progress, ignore these |

### The full-resolution copies are the size problem

`<hash>_original.jpg` is a complete decode at original dimensions, not a
thumbnail. It exists because [`videre faces`](/commands/faces/) needs full
resolution to place face boxes correctly, and reusing it is about 70x faster
than decoding again (~108 ms against ~7.6 s).

The consequence is that a HEIC-heavy library can easily produce **tens of GB** of
cache. Nothing warns you, and there is no size limit or eviction:

- No maximum size, no age-based expiry, no `--max-cache-size`.
- It only grows during normal use, since
  [`watch --heic`](/commands/watch/) fills it deliberately and
  [`report --show-faces`](/commands/report/) fills it lazily as you browse.
- **Only [`videre prune`](/commands/prune/) ever removes anything**, and only
  entries whose hash no longer appears in `file_hashes`. Cache for photos you
  still own is never reclaimed.

If you need the space back immediately, deleting the directory is safe. Every
file in it is derived, and it regenerates on demand:

```bash
du -sh ~/.cache/videre/thumbnails/
rm -rf ~/.cache/videre/thumbnails/
```

The only cost is re-conversion, which is why this is safe to do casually and an
embedding is not.

### It is shared between libraries

Unlike [embeddings](/reference/models/), which are per library, this cache is a
single global directory keyed by content.

That is mostly a benefit: the same photo scanned into two databases is converted
once. But two consequences follow.

**One library's `prune` can delete another's cache.** Cache entries are removed
when the hash is absent from *that* database's `file_hashes`, and prune cannot
see the other library. So pruning your work library may discard thumbnails your
personal library was using.

This is harmless, and deliberately so: the affected photos simply get converted
again next time they are viewed. Embeddings are kept per library precisely
because losing those would cost hours instead of milliseconds.

**`VIDERE_HOME` splits the cache.** Because the location follows `VIDERE_HOME`,
switching to a different home means starting from an empty cache, and the old
one stays on disk until you remove it. Worth knowing if you use `VIDERE_HOME` to
juggle libraries: you pay the conversion cost once per home, and orphaned caches
accumulate.

### Warming it deliberately

Running the HEIC stage before a long face-detection run avoids decoding
everything twice:

```bash
videre watch ~/Photos --heic     # decode and cache, then Ctrl-C when it settles
videre faces                     # reads the cache instead of re-decoding
```

Do not run these two at the same time. See the concurrency caveat under
[`videre faces`](/commands/faces/).
