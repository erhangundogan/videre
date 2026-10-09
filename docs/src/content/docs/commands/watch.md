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
videre watch                           # every stage, live: see Stages below
videre --library ~/Photos watch        # watch a different library
videre watch --scan --faces            # only these stages
videre watch --heic                    # only pre-convert HEIC thumbnails
videre watch --location                # only look up place names
videre watch --no-prune                # every stage but the cleanup
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

If none of `--scan`, `--faces`, `--heic`, `--location`, `--embed` or
`--prune` are given, all six run, so a file deleted or moved outside videre
leaves the library without a manual [`videre prune`](/commands/prune/).
`--no-prune` leaves the cleanup out. `--export-xmp` is the exception: it is
opt-in and never defaults on.

A file watch picks up goes through every stage in the same batch: it is
scanned, its faces are detected and grouped with their people, it is embedded
and classified, and it is given a place on the map, all within seconds of
arriving.

After each batch, watch says what each file got, read back from the library,
and what it did not get and why:

```text
videre watch: Kadıköy/çiçek.heic: scanned, 2 faces (1 grouped), embedded (photo), placed in Kadıköy
videre watch: Kadıköy/deniz.jpg: scanned, no faces, embed skipped: model not downloaded, placed in Kadıköy
```

A batch of more than 20 files prints the totals and only the files that were
not completed. `--silent` turns the lines off; [`videre status`](/commands/status/)
counts whatever is still outstanding. While watch has work outstanding, the
[gallery](/commands/gallery/)'s nav shows a small "still processing" note that
opens a list of what each stage has left, and disappears at zero.

| Stage | What it does |
|---|---|
| `--scan` | Same scan and hash pipeline as [`videre scan`](/commands/scan/), including reading marks from XMP (`--xmp db\|file\|newest`, see [scan](/commands/scan/#reading-marks-from-xmp---xmp)) |
| `--faces` | Detects faces in new images and groups them with their people |
| `--heic` | Pre-converts and caches HEIC thumbnails |
| `--location` | Looks up place names for GPS coordinates that have none, and gives each new photo its place on the map (at the default radius; a manual radius is respected) |
| `--embed` | Embeds and classifies new files with the library's model, the same work as [`videre embed`](/commands/embed/) and [`videre classify`](/commands/classify/). It never downloads the model: until `videre embed` has fetched it once, watch says so once and the files stay outstanding in [`videre status`](/commands/status/) |
| `--prune` | Same cleanup as [`videre prune`](/commands/prune/); runs on the startup and maintenance passes, not per event. On by default; `--no-prune` leaves it out |
| `--export-xmp` | Writes labels to `.xmp` sidecars, same as [`videre export`](/commands/export/). Each batch writes its own files' sidecars; the startup and maintenance passes rewrite every file's |

Grouping faces into people and photos into places are whole-library passes,
and how long they take depends on the size of the library, not on how many
files arrived: on a large library each takes minutes. So each batch looks at
how long the last full pass took. If it took under five seconds, as on a small
library, the batch runs the full pass. Otherwise the batch puts each new face
with the person whose face it most resembles (using the same similarity rule
as the full pass) and each new photo in the nearest place, or a new place of
its own, and the full pass runs on the maintenance pass to rebalance.

The regroup uses the same grouping values as `videre faces`: the ones the
gallery's People page saved for this library with **Recluster**, where it did,
otherwise the defaults. It prints one line when saved values are in use; see
[clustering parameters](/commands/faces/#clustering-parameters).

Every tracked stage records a run in the pipeline table, so
[`videre status`](/commands/status/) shows its last run and whether it
succeeded. The location stage reports as **`location-names`**, deliberately
distinct from **`locations`**, which is the row for
[`videre locations`](/commands/locations/), the clustering recompute; placing
only the new photos reports as **`location-assign`**. The face regrouping
reports as **`face-recluster`**, distinct from **`faces`**, which is the row for
a detection run; grouping only the new faces reports as **`face-attach`**. Each appears once watch has run it at least once;
a library that never used those stages does not list them.

The export stage keeps sidecars current while you work in another tool. It is
opt-in per run with `--export-xmp`, or always-on by setting
`videre config set export-xmp-on-watch true`. It merges into existing sidecars,
so it never clobbers another tool's data.

The scan stage is [incremental](/commands/scan/#incremental-by-default), like
`videre scan`: a file whose row is already current hashes nothing, whether it
was seen by an event or by a full pass, so a spurious event costs nothing.

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

**Leave out cleanup**, if you would rather prune by hand:

```bash
videre --library ~/Photos watch --no-prune
```

Naming stages runs only those, so add `--prune` to a list to keep the cleanup:
`videre watch --scan --faces --prune`.

## Running it for real

[`videre gallery`](/commands/gallery/) starts a watch for you and stops it
when the gallery stops; `videre config set gallery-starts-watch false` turns
that off. A watch you start yourself is used by the gallery and left running.

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

## Bulk imports

When 1,000 or more files are waiting at once, a camera import or a folder of
thousands copied in, watch switches to bulk mode. It keeps scanning as the files
arrive, so they show up in the gallery at once, but holds everything else until
no new file has arrived for 30 seconds. Then it runs every other stage once
over the whole import, each model loaded once, followed by the full face
regroup and place recompute whatever they cost, each stage showing its
progress as the standalone command does. It says when it switches:

```text
videre watch: bulk: 4213 files waiting; scanning as they arrive, everything else once no file has arrived for 30s
```

Until the import finishes, its files count as outstanding in
[`videre status`](/commands/status/).

## Tuning

The main knob is the debounce window, the time a file must stay quiet before
its event settles into a batch:

```bash
videre config set watch-debounce-ms 500     # default 1500
```

Lower it for the fastest possible reaction to a single file; raise it on a
chatty importer or a slow mount so more of a burst lands in one batch. Bulk mode has its own two, the number of
waiting files that starts it and the quiet period that finishes it:

```bash
videre config set watch-bulk-threshold 500      # default 1000 files
videre config set watch-bulk-quiet-ms 60000     # default 30000
```

See [config](/commands/config/).

## Caveats

**A manual command never runs at once with its own watch stage.** Each stage
takes the same lock as the command it stands in for, so a manual `faces` during
watch's faces stage is refused, and a stage that finds its command running is
held and retried. Commands with no matching stage, such as `embed`, run
alongside `watch`. Conversions through QuickLook share one machine-wide limit with every
other videre process; see
[running things at once](/guides/long-running-jobs/).

**`--prune` cannot override prune's safety guards.** It runs unattended and
cannot ask, so the bulk-deletion and repeated-failure guards are always active.
An unplugged drive is skipped rather than wiped. See
[`videre prune`](/commands/prune/) for the rules.

**A failing library volume backs the cycle off.** A disk-level write error (a
full disk, or a database gone read-only) doubles the loop's retry cadence, from
a few seconds up to the hourly maintenance pass: each retry still logs its
failure, but the volume is hit and reported once per backoff instead of every
2 s. Incoming file events queue during the backoff and are handled at the next
wake. The daemon stays up, and one clean cycle returns it to the normal
cadence.

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
