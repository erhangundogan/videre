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

Thumbnails go to `<VIDERE_HOME>/cache/thumbnails/` when `VIDERE_HOME` is set,
and `~/.cache/videre/thumbnails/` otherwise.

They are keyed by file content rather than path, so the same photo scanned into
a different database only needs converting once.

Only [`videre prune`](/commands/prune/) reclaims space here, and this cache can
reach tens of GB on a HEIC-heavy library.
