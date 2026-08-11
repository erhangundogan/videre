---
title: Keeping libraries separate
description: Run several collections without them interfering with each other.
---

There are two ways to keep collections apart, and they separate different
amounts.

| | `--db` / `config set db` | `VIDERE_HOME` |
|---|---|---|
| Database | separate | separate |
| Embeddings | separate | separate |
| Locks | separate | separate |
| Settings (`config.toml`) | **shared** | separate |
| Thumbnail cache | **shared** | separate |

Start with separate databases. Reach for `VIDERE_HOME` only when you want two
completely independent setups.

:::note[One database can hold several folders]
Scanning two folders into the *same* database is a different thing, and often
the right one: a collection spread over an internal disk and an external drive
is one library. See
[scanning more than one folder](/reference/paths/#scanning-more-than-one-folder).

This page is about collections that should **not** see each other.
:::

## Separate databases

```bash
videre scan --db ~/personal.db ~/Photos
videre scan --db ~/work.db ~/WorkShoots

videre dedupe --db ~/work.db        # only ever considers work photos
videre search --db ~/work.db "client logo"
```

Each database gets its own embeddings directory and its own locks, so the two
never block or contaminate each other. `videre dedupe` on one cannot propose
deleting a file recorded in the other, because it cannot see it.

To avoid typing `--db` constantly, set the one you use most as the default and
pass `--db` for the other:

```bash
videre config set db ~/personal.db
videre config set path ~/Photos

videre scan                          # personal, no arguments needed
videre scan --db ~/work.db ~/WorkShoots
```

## Separate homes

`VIDERE_HOME` relocates everything: database, settings, locks, embeddings and
cache.

```bash
export VIDERE_HOME=~/videre-work
videre scan ~/WorkShoots             # own db, own config, own cache
videre config                        # shows the work home's settings
```

Each home has its own `config.toml`, so defaults set in one are invisible in the
other. That is the point: two setups that share nothing.

A wrapper keeps it manageable:

```bash
# ~/.local/bin/videre-work
#!/bin/sh
VIDERE_HOME="$HOME/videre-work" exec videre "$@"
```

## What is still shared

**The Hugging Face model cache**, at `~/.cache/huggingface/hub/`, is shared by
everything on the machine unless you set `HF_HOME`. That is a benefit: models
are downloaded once, not once per library.

**The thumbnail cache is shared between databases** but not between homes, since
its location follows `VIDERE_HOME`. Two consequences:

- Sharing is mostly good. The same photo in two libraries is converted once.
- **One library's `prune` can delete another's cached thumbnails**, because
  prune removes entries whose hash is absent from *its own* database and cannot
  see the other. Harmless: the affected photos are simply converted again when
  next viewed.

That tradeoff is deliberate. Embeddings are kept per library precisely because
losing those costs hours, while a thumbnail costs milliseconds. See
[caches](/guides/caches/).

## Choosing which

**Separate databases** for collections you work with side by side, sharing
models and cache. Personal and work photos on one machine.

**Separate homes** when you want genuinely independent state: a scratch library
for experiments, a shared machine where two people should not see each other's
settings, or testing without touching your real setup.

```bash
VIDERE_HOME=/tmp/videre-scratch videre scan ~/some-folder
```

That last one is worth knowing. It is the safe way to try something without
risking your real database, config, or cache.

## Caveats

**Nothing warns you when you point at the wrong library.** A missing `--db` uses
the default silently. `videre config` shows what everything resolves to, and its
`resolved db` line is the one to check.

**`videre scan` adopts the first folder it ever sees** as the default path *for
that home*, so the first scan under a new `VIDERE_HOME` sets that home's
default. It never overwrites one you set yourself.

**Locks are keyed by the canonical database path**, so two databases both named
`photos.db` in different directories do not share a lock, and a symlink or
relative path to the same database resolves to the same one.
