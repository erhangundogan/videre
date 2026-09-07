---
title: Where your data lives
description: How a library is selected, what it stores, and where its caches live.
---

A videre library is a directory. Everything it accumulates lives in one state
directory at its root:

```
<library>/.videre/
  hashes.db      # the database
  config.toml    # this library's settings
  hashes.jsonl   # only after `videre export --jsonl`
  locks/         # marks which command is currently running
  embeddings/    # per-model search data
```

Nothing is created until you actually write something. Commands that only read
never create a directory or a database.

## How a library is selected

Every command resolves its library the same way. First match wins:

1. The directory given by `--library <dir>`
2. The invocation directory (the current working directory)

There is no saved default, no environment variable, and no ancestor search: the
library is always exactly the directory you named or the one you are standing
in. The authoritative database is `<library>/.videre/hashes.db`, and its
location is fixed; no command takes a database or output-location selector.

```bash
cd ~/Photos && videre stats        # the library at ~/Photos
videre --library ~/Photos stats    # the same library, from anywhere
```

Readers never create a database. If the selected library has no `.videre/`
state yet, they print that it is not initialized and exit nonzero rather than
silently creating an empty one:

```
library <path> is not initialized: run 'videre scan' there first
```

Only [`scan`](/commands/scan/) and [`watch`](/commands/watch/) bring a library
into being; [`config`](/commands/config/) may create the local config file.

## Settings resolution

Within a selected library, a value is resolved:

1. A flag on the command (for example `--model`)
2. The library's own `.videre/config.toml`
3. The built-in default

The config keys are fixed: `db = "hashes.db"`, `jsonl = "hashes.jsonl"`,
`default_model`, `xmp_precedence`, and `export_xmp_on_watch`. Each library has
its own config, so a setting in one is invisible in another. Your `$HOME` has no
special role: `~/.videre` is a library only if you deliberately select `~` as a
library root.

## Sources, filters, and operands

Three kinds of path appear on the command line, and they are not the same:

| Kind | Example | Resolved against |
|---|---|---|
| The **library** | `--library ~/Photos` | itself (named or the cwd) |
| A **filter** | `search --path Trips` | the library root; must stay inside it |
| A file **operand** | `import ~/Takeout` | the invocation directory |

A `--path` filter is a subtree of the library. It is accepted in its given and
canonical forms, and a path that resolves outside the library root, or through a
symlink that escapes it, is rejected rather than silently widening the request.
Only the database-backed commands (`search`, `embed`, `faces`, `classify`,
`mark`, `export`, `tag`) take `--path`; `scan` and `watch` walk the whole
library. Existing `.videre` state is never treated as media.

## One library is one directory tree

A library is rooted at a single directory and covers everything beneath it. A
collection split across, say, an internal disk and an external drive is one
library only if you root it at a directory that contains both; otherwise each
tree is its own library. Combining unrelated trees into a single library is no
longer possible, and two collections in two directories are already fully
separate. See [keeping libraries separate](/guides/multiple-libraries/).

:::caution[An unplugged drive is handled]
[`videre prune`](/commands/prune/) only removes a row when the file is missing
**and** its parent folder still exists, so an unmounted drive under the library
is skipped rather than wiped. It reports how many rows it skipped and which
directories were missing.
:::

## Locks

Each library gets one lock file per command, under `<library>/.videre/locks/`.
This is what lets `videre stats` report a command as currently running, what
stops the same command running twice against one library, and what lets
maintenance (`prune`) exclude other work in that library while unrelated
libraries proceed.

Lock names include a hash of the library root's canonicalised path, so a
symlink or a relative path to the same library resolves to the same locks, and
two libraries in different directories never share one.

## Caches

Two caches sit outside the library, under your user cache directory:

```
~/.cache/videre/            # per-library thumbnail and geocoding caches
~/.cache/huggingface/hub/   # shared model weights (honours HF_HOME)
```

Each library has its own thumbnail and geocoding cache namespace, so one
library's [`prune`](/commands/prune/) only reclaims its own entries. The Hugging
Face model-weights cache is shared across every library on the machine, so a
model is downloaded once. Deleting a cache is safe; everything in it regenerates
on demand. [Caches and disk use](/guides/caches/) covers what each costs to lose.

## Environment variables

| Variable | Effect |
|----------|--------|
| `HF_HOME` | Where model weights are cached (default `~/.cache/huggingface`) |
| `VIDERE_EMBED_DTYPE` | `f16` for slightly faster search preparation. Does not affect existing data. |

Model choice is deliberately not an environment variable. Use
`videre config set model <id>`, or `--model <id>` for a single command.

## Upgrading from earlier versions

Earlier videre kept a single global library under your home directory and let a
command point at other databases directly. That is gone: a command now always
operates on the directory you select, and its database is fixed inside that
directory's `.videre/`. To bring an old collection forward, run videre in its
directory (or pass `--library`); the first `scan` there builds its local state.
