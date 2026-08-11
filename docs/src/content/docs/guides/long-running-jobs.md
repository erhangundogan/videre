---
title: Long-running jobs and running things at once
description: What is safe to run in parallel, what is not, and what happens when you interrupt.
---

`embed`, `faces` and a full `scan` can each run for hours. This is what you can
safely do while they are going, and what happens when you stop them.

## What is safe to run together

**Reading, always.** The database is opened in WAL mode, which allows one writer
and many readers at once. All of these are fine against a live
[`videre watch`](/commands/watch/) or a running `embed`:

```bash
videre search "sunset"          # reads stored vectors
videre stats                    # reads the database
videre report --show-faces      # serves a live page
sqlite3 ~/.videre/hashes.db "SELECT COUNT(*) FROM file_hashes"
```

`videre watch` and `videre report --show-faces` are specifically designed to run
at the same time.

**Two different commands.** Locks are per command per database, so
`videre embed` and `videre locations` can run together as far as locking is
concerned.

**The same command against different databases.** Locks are keyed by the
database path, so two libraries never block each other.

## What is refused

**The same command twice against one database.** The second invocation is
refused rather than allowed to interleave writes. This is what stops a cron job
from stacking up when a run takes longer than its interval.

## What is allowed but a bad idea

:::danger[Do not run two HEIC-converting jobs at once]
[`embed`](/commands/embed/), [`faces`](/commands/faces/) and
[`watch`](/commands/watch/)'s faces and HEIC stages all convert HEIC and video
through the same macOS QuickLook service. Each limits its own concurrency, but
the limit is **per process**, so two videre processes together permit twice as
many conversions against one shared service.

Measured with `faces` and `embed` running simultaneously: HEIC loading averaged
**16.3 seconds** per file against about 7.6 uncontended, and one file exceeded
the 20 second timeout entirely, having converted in 0.39 s standalone
immediately afterwards.

Nothing is corrupted, and the skipped file is correctly not marked as done, so
it retries next run. But both jobs get dramatically slower, which is the
opposite of what running them in parallel was meant to achieve.
:::

Run them one after another instead:

```bash
videre embed && videre faces
```

If you keep `watch` running, stop it first or restrict it to stages that do not
decode:

```bash
videre watch ~/Photos --scan --location    # safe alongside a manual embed
```

**`videre locations` blocks writers for its whole run.** It does its work in a
single transaction, measured at about 8 minutes on a 70,000 file library, and
holds the write lock throughout. A concurrent `watch` write will wait. Run it
when nothing else needs to write.

## Interrupting

Ctrl-C is safe on every long-running command. None of them can leave the
database in a broken state, because work is committed as it goes rather than at
the end.

| Command | What a Ctrl-C costs |
|---|---|
| [`scan`](/commands/scan/) | Files not yet recorded. `--retry-incomplete` picks them up |
| [`embed`](/commands/embed/) | Up to `--chunk` rows, 500 by default |
| [`faces`](/commands/faces/) | Up to `workers x batch` images, ~160 with defaults |
| [`classify`](/commands/classify/) | Very little; it is fast to redo |
| [`prune`](/commands/prune/) | Nothing. Already-committed changes stand, and it is idempotent |
| [`locations`](/commands/locations/) | The whole run, since it is one transaction. Nothing is left half-done |
| [`fix-dates`](/commands/fix-dates/) | **Files already changed stay changed.** See below |

### Resuming

There is no resume flag. Rerunning *is* resuming, because each command only
looks for work that has not been done:

```bash
videre embed        # stopped after 12,000 photos
videre embed        # continues from 12,001
```

What makes that reliable is that each command records work that produced **no
result**, not just work that produced one:

- `faces` records every image it examined, including images with no faces in
  them. Otherwise every landscape photo would be re-examined forever.
- `scan` records a type of `application/octet-stream` for files it could not
  identify, so they are not reopened on every `--retry-incomplete`.

### The exception: `fix-dates`

`fix-dates` writes to your files, and an interrupt leaves the files it already
processed with their new timestamps and the rest with their old ones. That is
not corruption, but it is a partial result you cannot tell apart from a complete
one by looking.

Rerunning is harmless: it sets the same timestamps again. Use `--dry-run` first
so you know the full scope before starting.

## Checking on a background job

```bash
videre stats
```

`(running now)` appears next to a command whose lock is currently held by a live
process. That is how you tell a running job from a crashed one.

`videre stats --check` exits nonzero if any command's last run failed or
crashed, which is what to put in cron. A clean Ctrl-C records `interrupted` and
is deliberately **not** treated as a problem.

If a job died without cleaning up, for example a `kill -9`, a power loss, or an
out-of-memory kill, its row says `running` while no process holds the lock, and
stats reports it as `crashed`. Simply rerun the command.

## Suggested order for a fresh library

```bash
videre scan ~/Photos          # first, everything depends on it
videre watch ~/Photos --heic  # optional: makes faces ~70x faster on HEIC, then Ctrl-C
videre embed                  # hours
videre faces                  # hours
videre classify               # minutes, needs embed
videre locations              # minutes
```

Sequential, deliberately. The parallelism that would help is already inside
`faces`, which uses twice your core count by default.
