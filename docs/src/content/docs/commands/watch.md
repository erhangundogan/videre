---
title: videre watch
description: Keep everything current in the background. Runs until you stop it.
---

A live filesystem watcher that keeps the pipeline populated, so readers always
see fresh data without you rerunning things by hand. Drop a file anywhere under
the library and it is processed within seconds, and only that file; there is no
polling interval and no whole-library sweep on a timer. No server and no UI: it
runs in the foreground logging to stderr until you stop it with Ctrl-C.

```bash
videre watch                           # scan, faces, HEIC cache, and locations, live
videre --library ~/Photos watch        # watch a different library
videre watch --scan --faces            # only these stages
videre watch --heic                    # only pre-convert HEIC thumbnails
videre watch --location                # only look up place names
videre watch --prune                   # also clean stale entries (off by default)
videre watch --silent                  # no per-stage output
videre watch --type image              # only watch for new images
```

Like every command, `videre watch` operates on the library in the current
directory, or the one named by `--library`. It watches that library's own root,
every folder under it included.

:::tip
These filters work the same way across commands, and combine. See
[scoping a run](/guides/scoping-a-run/).
:::

## How watch stays current

Live filesystem events are the fast path, and they are backed by four
guarantees that do not depend on event delivery being reliable:

1. **On launch**, one incremental scan catches everything that changed while
   watch was not running.
2. **While it runs**, events drive the work: a file that appears or changes is
   scanned within seconds, coalesced through a short debounce window
   (configurable, see below) so a camera import or an `rsync` landing thousands
   of files settles into batches instead of one run per file.
3. **If the OS reports dropped events** (a burst overflowing its queue), watch
   answers with one full incremental rescan and resumes. Nothing is assumed,
   nothing is lost.
4. **About once an hour**, an internal maintenance pass reruns the same
   incremental scan plus the opt-in stages, as a safety net for mounts that
   lose events quietly (network shares, some external drives) and as the
   regular slot for cleanup work.

Where events cannot be delivered at all, or Linux inotify watches are
exhausted (`fs.inotify.max_user_watches`), watch says so once on stderr and
keeps the library current with a slow internal rescan instead. Degraded
latency, identical correctness, never silence.

Writes videre makes itself never re-trigger work: the `.videre` state
directory and `.xmp` sidecars are filtered out of the event stream.

## Stages

If none of `--scan`, `--faces`, `--heic` or `--location` are given, all four
run. `--prune` and `--export-xmp` are the exceptions: they are opt-in and never
default on.

| Stage | What it does |
|---|---|
| `--scan` | Same scan and hash pipeline as [`videre scan`](/commands/scan/), including reading marks from XMP (`--xmp db\|file\|newest`, see [scan](/commands/scan/#reading-marks-from-xmp---xmp)) |
| `--faces` | Detects faces in new images within seconds of arrival |
| `--heic` | Pre-converts and caches HEIC thumbnails |
| `--location` | Looks up place names for GPS coordinates that have none |
| `--prune` | Same cleanup as [`videre prune`](/commands/prune/); runs on the startup and maintenance passes, not per event |
| `--export-xmp` | Writes labels to `.xmp` sidecars, same as [`videre export`](/commands/export/); runs on the startup and maintenance passes, not per event |

Face detection is event-fast. The global face regrouping that follows
detection is a minutes-long whole-library pass, so it runs on the maintenance
pass instead, and only when faces were actually added since the previous
regroup. Until that pass, a brand-new face is detected but not yet assigned to
a person. If you want it grouped immediately, run [`videre
faces`](/commands/faces/), whose own regroup satisfies the same gate.

Every tracked stage records a run in the pipeline table, so
[`videre status`](/commands/status/) shows its last run and whether it
succeeded. The location stage reports as **`location-names`**, deliberately
distinct from **`locations`**, which is the row for
[`videre locations`](/commands/locations/), the clustering recompute. The face
regrouping reports as **`face-recluster`**, distinct from **`faces`**, which is
the row for a detection run. Each appears once watch has run it at least once;
a library that never used those stages does not list them.

The export stage keeps sidecars current while you work in another tool. It is
opt-in per run with `--export-xmp`, or always-on by setting
`videre config set export-xmp-on-watch true`. It merges into existing sidecars,
so it never clobbers another tool's data.

The scan stage is [incremental](/commands/scan/#incremental-by-default), like
`videre scan`: a file whose row is already current hashes nothing, whether it
was seen by an event or by a full pass, so a spurious event costs nothing.

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
videre --library ~/Photos watch --scan --location
```

**Warm the cache before a big job**, then stop it:

```bash
videre --library ~/Photos watch --heic       # Ctrl-C once the counts settle
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
flight, and the next launch's startup scan picks up anything missed.

Check on it from another terminal:

```bash
videre stats                 # inventory: counts, sizes, disk use
videre status                # pipeline health, what to run next, watch liveness
videre status --check        # exit non-zero if anything failed, for cron
```

## Tuning

The one knob is the debounce window, the time a file must stay quiet before
its event settles into a batch:

```bash
videre config set watch-debounce-ms 500     # default 1500
```

Lower it for the fastest possible reaction to a single file; raise it on a
chatty importer or a slow mount so more of a burst lands in one batch. See
[config](/commands/config/).

## Caveats

:::caution[Do not run watch alongside a manual embed or faces]
`watch --faces` and `watch --heic` both drive HEIC conversion, as do
[`videre embed`](/commands/embed/) and [`videre faces`](/commands/faces/). The
concurrency limit is per process, so two videre processes together permit twice
as many conversions against one shared macOS service.

Measured: a HEIC load averaged over 16 seconds against about 7.6 uncontended,
and one file exceeded the timeout that converted in 0.39 s alone. Nothing is
lost, since a batch that finds the library busy is held and retried
automatically, but both jobs get much slower.

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

**Several libraries, several watchers.** `watch` watches the single library it
was started in (or the one named by `--library`). Watching two libraries means
two processes, which is safe: each library has its own state and locks, so the
watchers neither conflict nor block each other, and an idle watcher costs
nothing. What they still share is the macOS QuickLook service behind HEIC
conversion, with the per-process limit noted above. See
[keeping libraries separate](/guides/multiple-libraries/). Two watchers on the
*same* library are refused: one indexer per library.

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

`--path`, `--date` and `--location` are deliberately absent: this watches the
selected library's own root and
has not opened the file yet, so it cannot answer them without doing the
expensive work the filter exists to avoid.

A scoped run prints `N of M`, so a filter that matches nothing is
distinguishable from an empty library. Full detail, including how missing data
excludes a file, is in [scoping a run](/guides/scoping-a-run/).

## More detail

- [Long-running jobs](/guides/long-running-jobs/) covers what is safe to run while this is going.
- [Caches and disk use](/guides/caches/) covers what the HEIC stage stores and how large it gets.
