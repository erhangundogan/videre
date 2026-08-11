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

Text mode prints, in order:

- Library totals: files and size, the photo/video split, duplicate groups and
  wasted space, faces detected and people named.
- One line per [search model](/reference/models/) present: id, row count,
  dimensions, and file size.
- One line per tracked command showing its last run, status, and duration.

Tracked commands are `scan`, `faces`, `embed`, `classify`, `dedupe`,
`fix-dates`, `prune` and `locations`. A command that has never run against this
database shows `never run`, and one currently running shows `(running now)`.

`report`, `search`, `mcp` and `config` are deliberately not tracked.

## `--check`

Exits nonzero if any tracked command's last run failed or crashed, so it can
drive cron or launchd failure handling without parsing any output. It composes
with both text and `--json` mode.

A cleanly interrupted run (Ctrl-C) is not treated as a problem.

## What counts as a failure

Per-item errors within a run — a few unreadable files, one corrupted image — do
not mark a run failed. Only an unhandled error does.

So `fix-dates` and `faces` can legitimately exit nonzero while still recording
success, since they return a count of problems rather than failing outright.

## JSON output

`--json` emits `{"schema_version": 1, "library": {...}, "pipelines": [...]}`.
The `pipelines` array always has exactly eight entries in a fixed order, with
null status and timestamps for commands that have never run.
