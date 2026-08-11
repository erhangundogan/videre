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
Library: 204 file(s) (1.9 GB), 151 photo(s), 52 video(s)
Duplicates: 3 group(s), 7 file(s), 10.4 MB wasted
Faces: 98 detected, 0 people named

Embeddings:
  google/siglip-base-patch16-224              196   768-dim     400 KB

Pipeline status:
  scan       success      last_run=2026-08-09 22:21:31  duration=1ms
  faces      success      last_run=2026-08-09 22:16:25  duration=34596ms
  embed      success      last_run=2026-08-09 22:17:59  duration=12692ms
  classify   success      last_run=2026-08-09 22:18:33  duration=921ms
  dedupe     success      last_run=2026-08-09 22:21:31  duration=0ms
  fix-dates  success      last_run=2026-08-09 22:16:04  duration=2ms
  prune      success      last_run=2026-08-09 22:17:39  duration=2ms
  locations  success      last_run=2026-08-09 22:18:38  duration=66ms
```

**Duplicates** counts exact, byte-identical copies only. "Wasted" is what you
would reclaim by deleting all but one of each group, which is what
[`videre dedupe`](/commands/dedupe/) proposes.

**Faces detected** counts individual faces, not photos, so one group shot
contributes several. **People named** stays at 0 until you assign names in
[`report --faces`](/commands/report/); detection alone never produces a name.

**Embeddings** lists one line per [model](/reference/models/) you have prepared,
with dimensions derived from the stored data rather than a hardcoded table, so
an unfamiliar model still reports honestly.

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

`never run` marks a command that has not executed against this database, and
`(running now)` appears when its lock is currently held by a live process. That
makes this the way to see whether a background [`watch`](/commands/watch/) is
actually working:

```bash
videre stats | grep -E 'scan|faces'
```

Tracked commands are `scan`, `faces`, `embed`, `classify`, `dedupe`,
`fix-dates`, `prune` and `locations`, always in that order, always eight lines.
`report`, `search`, `mcp` and `config` are deliberately not tracked: they are
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
