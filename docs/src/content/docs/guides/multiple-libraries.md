---
title: Keeping libraries separate
description: Run several collections without them interfering with each other.
---

A library is a directory. Everything a collection accumulates lives in the
`.videre/` state directory at its root, so two collections in two different
directories are already completely separate: separate database, separate
per-model embeddings, separate locks, separate config, and separate caches.
There is nothing to configure and no shared default to get wrong.

You pick which library a command acts on the same way every time: it is the
directory you run the command in, or the one named by `--library <dir>`. See
[where your data lives](/reference/paths/) for the full resolution rule.

:::note[One library can hold several folders]
Scanning a tree that spans more than one folder into the *same* library is a
different thing, and often the right one: a collection split across an internal
disk and an external drive is one library rooted at their common parent. This
page is about collections that should **not** see each other.
:::

## Two collections, side by side

```bash
videre --library ~/Photos scan          # the personal library
videre --library ~/WorkShoots scan      # the work library

videre --library ~/WorkShoots dedupe    # only ever considers work photos
videre --library ~/WorkShoots search "client logo"
```

Or run each command from inside the library, with no flag at all:

```bash
cd ~/WorkShoots
videre scan
videre dedupe                           # still only work photos
```

Each library keeps its own embeddings directory and its own locks, so the two
never block or contaminate each other. `dedupe` on one cannot propose deleting
a file recorded in the other, because it cannot see it.

## What each library owns

| | Per library |
|---|---|
| Database (`.videre/hashes.db`) | separate |
| Per-model embeddings | separate |
| Locks | separate |
| Config (`.videre/config.toml`) | separate |
| Thumbnail and geocoding caches | separate |

A command run against one library cannot read or write another's state, and
`prune` sweeps only the cache entries belonging to the library it runs in, so it
can never delete another library's cached thumbnails.

## What is still shared

**The Hugging Face model cache**, at `~/.cache/huggingface/hub/`, is shared by
everything on the machine unless you set `HF_HOME`. That is a benefit, not a
leak: model *weights* are downloaded once and reused, while each library's own
*embeddings* stay private to it. See [caches](/guides/caches/).

## A scratch library

Because a library is just a directory, an experiment is a throwaway directory:

```bash
videre --library /tmp/videre-scratch scan ~/some-folder
```

Nothing you do there can touch a real collection: its database, config and
caches all live under `/tmp/videre-scratch/.videre/`, and deleting the directory
removes every trace.

## Caveats

**Nothing warns you when you point at the wrong library.** A command run in the
wrong directory, or with the wrong `--library`, acts on whatever library is
there. The scoped output always prints `N of M`, so a filter matching nothing in
the library you meant is visible rather than silent.

**A directory with no `.videre/` is not yet a library.** The commands that read
a library (`search`, `stats`, `dedupe`, and the rest) report that it is not
initialized rather than creating an empty one; only `scan` and `watch` bring a
library into being.

**Locks are keyed by the canonical library root**, so a symlink or a relative
path to the same library resolves to the same locks, and two libraries in
different directories never share one.
