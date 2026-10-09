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
videre gallery      # serves a live page
sqlite3 .videre/hashes.db "SELECT COUNT(*) FROM file_hashes"
```

`videre watch` and `videre gallery` are specifically designed to run
at the same time.

**Two different commands.** Locks are per command per library, so
`videre embed` and `videre locations` can run together as far as locking is
concerned.

**The same command against different libraries.** Locks live in each
library's own `.videre/locks/`, so two libraries never block each other.

## What is refused

**The same command twice against one library.** The second invocation is
refused rather than allowed to interleave writes. This is what stops a cron job
from stacking up when a run takes longer than its interval.

## At a glance

Measured by running each pair at once on two small libraries and comparing the
result with running the same two commands one after the other. No pair
produced a different result.

| running together | what happens |
|---|---|
| the same command twice on one library | the second is refused: "the library is busy" |
| a command and `watch`'s stage for it | whichever starts second is refused; `watch` retries later |
| two `scan`s of a library not yet set up | the second is refused until the first has set it up |
| `faces` + `embed`, `watch`'s faces stage + `embed`, `gallery` + `faces` | both run, sharing QuickLook |
| the same command on two libraries | both run, sharing QuickLook and the CPU |
| two first-time model downloads | one downloads, the other waits for it |

## What slows things down

**Two jobs that convert HEIC or video share QuickLook.**
[`embed`](/commands/embed/), [`faces`](/commands/faces/), the gallery's
previews and [`watch`](/commands/watch/)'s faces and HEIC stages all convert
through the one macOS QuickLook service. Every videre process on the machine
shares one limit of simultaneous conversions (6 by default, or the largest
`--qlmanage-concurrency` in use), whichever library it works on. Running them
together is safe and nothing is skipped, but each runs slower than it would
alone.

**`videre locations` blocks writers for its whole run.** It does its work in a
single transaction and holds the write lock throughout. A concurrent `watch` write will wait. Run it
when nothing else needs to write.

## Slow and unresponsive drives

A long job on a slow drive is not cut short. Hashing a file has no total time
limit: as long as bytes keep arriving, however slowly, the read continues. A
file is skipped only when its read returns nothing for 20 seconds, or when its
metadata does not answer within five seconds. A skipped file is not marked as
done, so the next run, or the next [`watch`](/commands/watch/) cycle, tries it
again. There is no promise of a fixed total scan time on a slow drive.

A read that the operating system never returns cannot be cancelled. videre
stops waiting for it, but the helper thread stays blocked until the drive
answers, and keeps its place among the process's
[`io-workers`](/commands/config/). When every place is held by a stuck read,
new files are refused rather than queued behind them: videre warns once that
the I/O worker pool is saturated, skips the affected files so they are retried
later, and says on exit how many were refused. A refusal is reported as its
own condition, never as the drive timing out, and
[`prune`](/commands/prune/) treats it as "unknown", so it never deletes a row
because a file could not be checked.

## Interrupting

Ctrl-C is safe on every long-running command. None of them can leave the
database in a broken state, because work is committed as it goes rather than at
the end.

| Command | What a Ctrl-C costs |
|---|---|
| [`scan`](/commands/scan/) | Files not yet recorded. Re-running `scan` picks them up (it is incremental) |
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
  identify, so they are not reopened on every scan.

### The exception: `fix-dates`

`fix-dates` writes to your files, and an interrupt leaves the files it already
processed with their new timestamps and the rest with their old ones. That is
not corruption, but it is a partial result you cannot tell apart from a complete
one by looking.

Rerunning is harmless: it sets the same timestamps again. Use `--dry-run` first
so you know the full scope before starting.

## Checking on a background job

```bash
videre status
```

`(running now)` appears next to a command whose lock is currently held by a live
process. That is how you tell a running job from a crashed one.

`videre status --check` exits nonzero if any command's last run failed or
crashed, or logged an error, which is what to put in cron. A clean Ctrl-C records `interrupted` and
is deliberately **not** treated as a problem.

If a job died without cleaning up, for example a `kill -9`, a power loss, or an
out-of-memory kill, its row says `running` while no process holds the lock, and
status reports it as `crashed`. Simply rerun the command.

## Suggested order for a fresh library

```bash
videre --library ~/Photos scan          # first, everything depends on it
videre --library ~/Photos watch --heic  # optional: makes faces ~70x faster on HEIC, then Ctrl-C
videre embed                  # hours
videre faces                  # hours
videre classify               # minutes, needs embed
videre locations              # minutes
```

Sequential, deliberately. The parallelism that would help is already inside
`faces`, which uses twice your core count by default.
