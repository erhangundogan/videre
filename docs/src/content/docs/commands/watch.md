---
title: videre watch
description: Keep everything current in the background. Runs until you stop it.
---

A long-running loop that keeps the pipeline populated, so readers always see
fresh data without you rerunning things by hand. No server and no UI: it runs in
the foreground logging to stderr until you stop it with Ctrl-C.

```bash
videre watch                           # scan, faces, HEIC cache, and locations every 5 minutes
videre --library ~/Photos watch        # watch a different library
videre watch --scan --faces            # only these stages
videre watch --heic                    # only pre-convert HEIC thumbnails
videre watch --location                # only look up place names
videre watch --prune                   # also clean stale entries (off by default)
videre watch --interval 60             # seconds between cycles (default 300)
videre watch --silent                  # no per-cycle output
videre watch --type image              # only watch for new images
```

Like every command, `videre watch` operates on the library in the current
directory, or the one named by `--library`. It scans that library's own root.

:::tip
These filters work the same way across commands, and combine. See
[scoping a run](/guides/scoping-a-run/).
:::

## Stages

If none of `--scan`, `--faces`, `--heic` or `--location` are given, all four
run. `--prune` and `--export-xmp` are the exceptions: they are opt-in and never
default on.

| Stage | What it does |
|---|---|
| `--scan` | Same scan and hash pipeline as [`videre scan`](/commands/scan/), including reading marks from XMP (`--xmp db\|file\|newest`, see [scan](/commands/scan/#reading-marks-from-xmp---xmp)) |
| `--faces` | Detects faces in new images, then regroups everything |
| `--heic` | Pre-converts and caches HEIC thumbnails |
| `--location` | Looks up place names for GPS coordinates that have none |
| `--prune` | Same cleanup as [`videre prune`](/commands/prune/) |
| `--export-xmp` | Writes labels to `.xmp` sidecars, same as [`videre export`](/commands/export/) |

The export stage keeps sidecars current while you work in another tool. It is
opt-in per run with `--export-xmp`, or always-on by setting
`videre config set export-xmp-on-watch true`. It merges into existing sidecars,
so it never clobbers another tool's data.

Note that [`embed`](/commands/embed/) and [`classify`](/commands/classify/) are
**not** stages. Semantic search data is not kept current automatically; run
`videre embed` yourself after adding a batch of photos.

## Choosing what to run

**Everything, and forget about it.** The common case:

```bash
videre --library ~/Photos watch
```

**Just keep the index current**, leaving face detection for when you are not
using the machine:

```bash
videre --library ~/Photos watch --scan --location --interval 120
```

**Warm the cache before a big job**, then stop it:

```bash
videre --library ~/Photos watch --heic       # Ctrl-C once the per-cycle counts settle
videre faces
```

**Include cleanup**, if your photos live on an always-connected disk:

```bash
videre --library ~/Photos watch --scan --faces --heic --location --prune
```

Passing `--prune` requires listing the other stages you want, since naming any
stage disables the defaults.

## Running it for real

There is no daemon mode and no service unit. It runs in the foreground until
interrupted.

```bash
# a tmux pane
tmux new -s videre 'videre --library ~/Photos watch'

# or with a log
videre --library ~/Photos watch 2>> ~/Photos/.videre/watch.log
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
database. See the [thumbnail cache](/guides/caches/#thumbnail-cache).

**One library per process.** `watch` watches the single library it was started
in (or the one named by `--library`). Watching two libraries means two
processes, and they should not run their HEIC or faces stages at the same time,
for the reason above. See
[keeping libraries separate](/guides/multiple-libraries/).

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

`--path`, `--date` and `--location` are deliberately absent: this walks the
selected library's own root and
has not opened the file yet, so it cannot answer them without doing the
expensive work the filter exists to avoid.

A scoped run prints `N of M`, so a filter that matches nothing is
distinguishable from an empty library. Full detail, including how missing data
excludes a file, is in [scoping a run](/guides/scoping-a-run/).

## More detail

- [Long-running jobs](/guides/long-running-jobs/) covers what is safe to run while this is going.
- [Caches and disk use](/guides/caches/) covers what the HEIC stage stores and how large it gets.
