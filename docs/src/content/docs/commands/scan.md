---
title: videre scan
description: Read a folder recursively and record every media file in the database.
---

Reads a folder recursively and records every
[supported media file](/reference/file-types/) in the database. Run this first:
everything else reads from what it creates.

```bash
videre scan ~/Photos                              # scan into the default database
videre scan                                       # same, using the folder from `videre config set path`
videre scan ~/Photos --similar                    # also fingerprint images/videos for near-duplicate detection
videre scan ~/Photos --db ~/photos.db             # write to a specific database instead
videre scan ~/Photos --output                     # write JSONL to ~/.videre/hashes.jsonl instead of SQLite
videre scan ~/Photos --output out.jsonl           # write JSONL to a specific file
videre scan ~/Photos --retry-incomplete           # only files an earlier scan didn't finish
videre scan ~/Photos --silent                     # no progress output
videre scan ~/Photos --json                       # print one JSON summary object instead
```

Re-running is safe and idempotent, since existing entries are updated in place.

:::note
`--output` and `--db` cannot be combined. A bare `--output` must come *after*
the folder, or it swallows the folder as its value.

`--output-sqlite` still works as an alias for `--db`. It was the original name,
from when JSONL and SQLite were peer output *formats* rather than one
destination and one opt-out. Existing scripts do not need changing.
:::

## `--retry-incomplete`

A normal scan re-reads every byte of every file, which on a large library is the
slow part by far: about 10 minutes and 460 GB of reading for 70,000 files, of
which walking the folder is under two seconds.

`--retry-incomplete` still walks the folder but opens only files that have no
entry yet, or whose entry a previous run left unfinished — an interrupted scan,
or a file that timed out on a slow or disconnected drive. On an already-complete
library of that size it finishes in about a second, having opened nothing. New
files are picked up too, since they have no entry yet.

It needs a database to consult, so it cannot be combined with `--output`, which
writes JSONL.

## `--similar`

Also computes a perceptual fingerprint, which is what lets
[`videre dedupe --similar`](/commands/dedupe/) find photos that merely *look*
alike rather than being byte-identical.

For `.mov` and `.mp4` files this needs macOS, since the frame is extracted with
QuickLook. Elsewhere those files simply get no fingerprint, the same graceful
skip as any other undecodable file. HEIC files never get one.

## Defaults it sets for you

The first time you run `videre scan <folder>` with no default folder configured,
it adopts that folder as your default so later commands work with no arguments.
It prints a one-line note when it does, and never overwrites a folder you
configured yourself. Change it with `videre config set path <dir>`.
