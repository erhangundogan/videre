---
title: videre watch
description: Keep everything current in the background. Runs until you stop it.
---

A long-running loop that keeps the pipeline populated, so readers always see
fresh data without you rerunning things by hand. No server and no UI: it runs in
the foreground logging to stderr until you stop it with Ctrl-C.

```bash
videre watch ~/Photos                  # scan, faces, HEIC cache, and locations every 5 minutes
videre watch                           # same, using the folder from `videre config set path`
videre watch ~/Photos --scan --faces   # only these stages
videre watch ~/Photos --heic           # only pre-convert HEIC thumbnails
videre watch ~/Photos --location       # only look up place names
videre watch ~/Photos --prune          # also clean stale entries (off by default)
videre watch ~/Photos --interval 60    # seconds between cycles (default 300)
videre watch ~/Photos --silent         # no per-cycle output
videre watch ~/Photos --db ~/photos.db # use a specific database
videre watch ~/Photos --type image     # only watch for new images
videre watch ~/Photos --path ~/Photos/Inbox # only watch one subfolder
```

:::tip
These filters work the same way across commands, and combine. See
[scoping a run](/guides/scoping-a-run/).
:::

`--output-sqlite` still works as an alias for `--db`, the name it had
originally. Existing scripts do not need changing.

## Stages

If none of `--scan`, `--faces`, `--heic` or `--location` are given, all four
run. `--prune` is the exception: it is opt-in and never defaults on.

| Stage | What it does |
|---|---|
| `--scan` | Same scan and hash pipeline as [`videre scan`](/commands/scan/) |
| `--faces` | Detects faces in new images, then regroups everything |
| `--heic` | Pre-converts and caches HEIC thumbnails |
| `--location` | Looks up place names for GPS coordinates that have none |
| `--prune` | Same cleanup as [`videre prune`](/commands/prune/) |

Note that [`embed`](/commands/embed/) and [`classify`](/commands/classify/) are
**not** stages. Semantic search data is not kept current automatically; run
`videre embed` yourself after adding a batch of photos.

## Choosing what to run

**Everything, and forget about it.** The common case:

```bash
videre watch ~/Photos
```

**Just keep the index current**, leaving face detection for when you are not
using the machine:

```bash
videre watch ~/Photos --scan --location --interval 120
```

**Warm the cache before a big job**, then stop it:

```bash
videre watch ~/Photos --heic       # Ctrl-C once the per-cycle counts settle
videre faces
```

**Include cleanup**, if your photos live on an always-connected disk:

```bash
videre watch ~/Photos --scan --faces --heic --location --prune
```

Passing `--prune` requires listing the other stages you want, since naming any
stage disables the defaults.

## Running it for real

There is no daemon mode and no service unit. It runs in the foreground until
interrupted.

```bash
# a tmux pane
tmux new -s videre 'videre watch ~/Photos'

# or with a log
videre watch ~/Photos 2>> ~/.videre/watch.log
```

For something that survives a reboot, wrap it in a launchd agent on macOS or a
systemd user unit on Linux. It expects to be restarted freely: every stage is
resumable and idempotent, so a kill at any point loses at most the work in
flight.

Check on it from another terminal:

```bash
videre stats                 # last run and status per command
videre stats --check         # exit non-zero if anything failed, for cron
```

## Interval

`--interval` is the sleep *between* cycles, not a schedule. A cycle that takes
ten minutes followed by `--interval 300` means a new cycle every fifteen.

The default of 300 suits a library that changes occasionally. Lower it if you
import often and want photos searchable sooner; raise it if cycles are long and
you would rather they not overlap with your own work.

The first cycle on a large library is by far the longest, because everything is
new. Later cycles typically do nothing and finish in seconds.

## Caveats

:::caution[Do not run watch alongside a manual embed or faces]
`watch --faces` and `watch --heic` both drive HEIC conversion, as do
[`videre embed`](/commands/embed/) and [`videre faces`](/commands/faces/). The
concurrency limit is per process, so two videre processes together permit twice
as many conversions against one shared macOS service.

Measured: a HEIC load averaged over 16 seconds against about 7.6 uncontended,
and one file exceeded the timeout that converted in 0.39 s alone. Nothing is
lost, since skipped files retry next cycle, but both jobs get much slower.

If you are about to run a long `embed` or `faces` by hand, stop `watch` first,
or start it with only `--scan --location`.
:::

**`--prune` cannot override prune's safety guards.** It runs unattended and
cannot ask, so the bulk-deletion and repeated-failure guards are always active.
An unplugged drive is skipped rather than wiped. See
[`videre prune`](/commands/prune/) for the rules.

**The HEIC cache grows without limit.** `--heic` caches a full-resolution decode
per HEIC file, which is what makes face detection fast, and can reach tens of
GB. Only `prune` reclaims any of it, and only for photos no longer in the
database. See the [thumbnail cache](/reference/paths/#thumbnail-cache).

**One folder per process.** `watch` takes a single directory. Watching two roots
means two processes, and they should not run their HEIC or faces stages at the
same time, for the reason above. See
[scanning more than one folder](/reference/paths/#scanning-more-than-one-folder).

**Reading while it runs is fine.** The database is opened in WAL mode, so
[`videre gallery`](/commands/gallery/), `search`, `stats` and your own
`sqlite3` queries all work against a live `watch`. Those two are designed to run
together.

**A crash is visible, not silent.** If a stage fails, `videre stats` shows the
command as failed or crashed, which is what `--check` is for.

## Scoping the run

Every flag below narrows an existing set, never widens it, and they combine:
each condition must hold.

| Flag | Selects |
|---|---|
| `--type` | `image` or `video`. Repeatable, or comma-separated |
| `--ext` | file extension, e.g. `mov`. Repeatable, or comma-separated |
| `--mime` | exact type, e.g. `video/quicktime`. Repeatable, or comma-separated |
| `--path` | only files under this directory. Repeatable |

`--date` and `--location` are deliberately absent: this walks the filesystem and
has not opened the file yet, so it cannot answer them without doing the
expensive work the filter exists to avoid.

A scoped run prints `N of M`, so a filter that matches nothing is
distinguishable from an empty library. Full detail, including how missing data
excludes a file, is in [scoping a run](/guides/scoping-a-run/).

## More detail

- [Long-running jobs](/guides/long-running-jobs/) covers what is safe to run while this is going.
- [Caches and disk use](/guides/caches/) covers what the HEIC stage stores and how large it gets.
