---
title: videre stats
description: Library totals and what has run recently, in one shot.
---

```bash
videre stats                           # library totals and what has run recently
videre stats --json                    # print one JSON object instead
videre stats --check                   # exit non-zero if anything failed or crashed (for cron)
videre stats --db ~/photos.db          # use a specific database
```

## Reading the output

```
Library: 70601 file(s) (398.9 GB), 57142 photo(s), 13459 video(s)
Duplicates: 0 group(s), 0 file(s), 0 B wasted
Faces: 58555 detected, 86 people named
Marks: 312 rated, 40 picked, 12 labelled, 128 liked

Embeddings:
  google/siglip-base-patch16-224            70588   768-dim   128.3 MB
  google/siglip-so400m-patch14-384          70587  1152-dim   189.6 MB
  google/siglip2-base-patch16-384           70588   768-dim   128.3 MB

By type:
  mov      video/quicktime             12564   165.4 GB
  jpeg     image/jpeg                  43524   142.4 GB
  mp4      video/mp4                     643    62.0 GB
  heic     image/heic                  10395    20.8 GB
  png      image/png                    3211     6.5 GB
  mov      video/mp4                     251   909.5 MB
  mp4      video/quicktime                 1   462.5 MB
  dng      image/tiff                      9   433.9 MB
  gif      image/gif                       2     4.8 MB
  png      image/jpeg                      1    43.7 KB

Disk use:
  embeddings           446.3 MB
  database             161.8 MB
  thumbnails            84.1 MB  (rebuildable)
  place names           11.4 MB  (rebuildable)
  database journal      32.0 KB  (rebuildable)
  total                703.7 MB  (95.6 MB of it rebuildable)

Pipeline status:
  scan       success      last_run=2026-08-09 16:17:43  duration=0ms
  faces      interrupted  last_run=2026-08-03 10:11:45  duration=9m 6s
  embed      success      last_run=2026-08-17 13:29:01  duration=21s
  classify   success      last_run=2026-08-17 13:29:30  duration=1.5s
  dedupe     success      last_run=2026-08-05 11:35:38  duration=132ms
  fix-dates  success      last_run=2026-07-31 10:33:15  duration=50ms
  prune      success      last_run=2026-07-31 21:02:36  duration=479ms
  locations  success      last_run=2026-08-18 09:32:47  duration=1m 17s
```

Real output from a 70,000-file library, so the awkward rows are the interesting
ones.

**By type** groups the library by file extension, with the mime type beside it
and the largest first. These are the values `--ext` and `--mime` take, so this is
where to look before scoping a run.

**Extension and mime disagree more often than people expect**, and both are shown
rather than reconciled. Three rows above are the same file types filed under a
different name: 251 `.mov` files holding MP4 video, one `.mp4` holding QuickTime,
and one `.png` that is really a JPEG. Reconciling them would mean choosing which
of the two to lie about, so `--ext mov` and `--mime video/quicktime` select
genuinely different sets.

**Disk use** is what videre itself stores, not your photos, largest first.
Entries marked `(rebuildable)` cost only time to recreate: thumbnails are
re-converted on demand, place names ship with videre, and the database journal is
transient. The database and embeddings are not rebuildable - an embedding run
takes hours, and the database cannot be recreated without rescanning.

Note the proportions above: 703.7 MB of videre data describing 398.9 GB of
photos, and only 95.6 MB of it disposable. **Embeddings dominate** because that
library has three models prepared.

:::note[Embeddings are counted per library]
They live in a separate directory per database, so `stats` reports only the ones
belonging to the database you asked about. If other libraries share the same
[`VIDERE_HOME`](/guides/multiple-libraries/), theirs appear as a separate
`embeddings (other libraries)` row - real disk use, but not this library's.
:::

**Duplicates** counts exact, byte-identical copies only. "Wasted" is what you
would reclaim by deleting all but one of each group, which is what
[`videre dedupe`](/commands/dedupe/) proposes.

**Faces detected** counts individual faces, not photos, so one group shot
contributes several - 58,555 faces across 57,142 photos above. **People named**
stays at 0 until you assign names in [`gallery`](/commands/gallery/); detection
alone never produces a name, however many faces it finds.

**Embeddings** lists one line per [model](/reference/models/) you have prepared.
The dimensions come from the stored data rather than a hardcoded table, so an
unfamiliar model still reports honestly - which is why the 1152-dimension
`so400m` line above needs no special-casing to appear correctly.

The counts differ slightly between models (70588, 70587, 70588) because each run
skipped whatever it could not decode at the time. A count below the library total
is normal, not a sign of a failed run.

## Numbers that look wrong but are not

**Photos plus videos may not equal the total.** In the example, 151 + 52 = 203
against 204 files. The missing one is a file whose type could not be identified
from its bytes, so it counts toward the total but is neither a photo nor a
video.

**The embedding count is lower than the file count**, and should be. Vectors are
keyed by content, so duplicate copies share one, and `.dng` files are never
embedded at all. A gap here is normal; a gap of thousands means
[`videre embed`](/commands/embed/) has more to do.

**`duration=0ms` is not an error.** Commands reading an already-warm database
genuinely finish in under a millisecond.

**A command can show `success` and still have exited nonzero.** Per-item
problems, a few unreadable files or one corrupt image, do not fail a run. Only
an unhandled error does. `fix-dates` and `faces` both return a count of problems
rather than failing outright.

## Checking on things

Every tracked command reports one of:

| status | meaning |
|---|---|
| `success` | finished, whatever it did or did not find |
| `failed` | returned an error |
| `interrupted` | stopped part-way, usually Ctrl-C or a machine going to sleep |
| `crashed` | claimed to be running, but no live process holds its lock |
| `-` | has never executed against this database, and `last_run` reads `never run` |

`(running now)` is appended to the line, not a status: it means the command's
lock is held by a live process at this moment.

The `faces interrupted` line in the sample above is the ordinary case: a long
run was stopped part-way. **Nothing is lost when that happens.** Every long job
records what it already processed, so running it again resumes rather than
restarting. The status is there to tell you the work is unfinished, not that it
is broken.

:::note[Per-item errors do not make a run `failed`]
`fix-dates` and `faces` can skip individual files and still record `success`,
because one unreadable photo is not a failed run. `failed` means the command
itself returned an error.
:::

Because `(running now)` reflects a live lock, this is the way to see whether a
background [`watch`](/commands/watch/) is actually working:

```bash
videre stats | grep -E 'scan|faces'
```

Tracked commands are `scan`, `faces`, `embed`, `classify`, `dedupe`,
`fix-dates`, `prune` and `locations`, always in that order, always eight lines.
`gallery`, `search`, `mcp` and `config` are deliberately not tracked: they are
interactive or read-only, so "when did it last run" says nothing useful.

## `--check` for unattended runs

Exits nonzero if any tracked command's last run failed or crashed, so cron or
launchd can act without parsing any output:

```bash
videre stats --check || echo "videre needs attention" | mail -s "videre" me@example.com
```

```bash
# crontab: nightly refresh, alert only on failure
0 3 * * * videre scan ~/Photos --retry-incomplete --silent && videre stats --check
```

A cleanly interrupted run (Ctrl-C) counts as `interrupted`, not a problem, so
stopping a long job by hand does not trigger alerts forever.

`crashed` is different from `failed`: it means the last run recorded itself as
still running, but no live process holds its lock. That is what a kill -9, a
power loss, or an OOM kill looks like after the fact.

`--check` changes only the exit code, and composes with both output formats.

## JSON output

```bash
videre stats --json
```

```json
{
  "schema_version": 1,
  "library": {
    "total_files": 204,
    "total_size_bytes": 2038765432,
    "total_photos": 151,
    "total_videos": 52,
    "duplicate_group_count": 3,
    "duplicate_file_count": 7,
    "wasted_bytes": 10921472,
    "faces_detected": 98,
    "people_named": 0,
    "embeddings": [
      { "model_id": "google/siglip-base-patch16-224", "count": 196, "dims": 768, "size_bytes": 409600 }
    ]
  },
  "pipelines": [
    { "command": "scan", "last_run_at": "2026-08-09 22:21:31", "status": "success",
      "duration_ms": 1, "currently_running": false }
  ]
}
```

`pipelines` always has exactly eight entries in a fixed order, with
`last_run_at`, `status` and `duration_ms` all `null` for a command that has
never run. `embeddings` has one entry per model and may be empty.

```bash
videre stats --json | jq -r '.library.wasted_bytes / 1048576 | floor'
videre stats --json | jq -r '.pipelines[] | select(.status != "success") | .command'
```

## Caveats

**Numbers describe the database, not your disk.** They reflect the last
[`videre scan`](/commands/scan/). Files deleted since then are still counted
until [`videre prune`](/commands/prune/) runs, which is the usual reason a
freshly cleaned library still reports duplicates.

**It requires an existing database.** Unlike `embed` or `classify`, pointing
`--db` at a path that does not exist fails cleanly instead of creating an empty
one.

**Sizes are what the files claim**, taken from the database rather than measured
now. `du` on the folder can differ, particularly with sparse files or a
filesystem doing compression.
