---
title: videre pipeline
description: One command that brings the library fully current, in dependency order.
---

One command that brings your library up to date. It runs the stages in the
right order, each one incremental, then exits. It is the action counterpart to
[`videre status`](/commands/status/), which reports the same picture without
doing any work.

```bash
videre pipeline                        # bring everything current (asks before the slow stages)
videre pipeline --dry-run              # show the plan and rough cost, do nothing
videre pipeline --yes                  # do not ask, run the slow stages too
videre pipeline --skip embed,classify  # run only the cheap stages this time
videre pipeline --json                 # one JSON object describing what ran
videre --library ~/Photos pipeline     # select a different library
```

## What it runs

In dependency order:

```
scan -> faces -> embed -> classify -> locations
```

`scan` runs first because it detects what changed and every later stage reads
its rows. `embed` runs before `classify` because classify reads embeddings.
Each stage only touches outstanding work, so on an unchanged library the whole
run is a cheap no-op: a stage with nothing to do is skipped.

Everything on by default is additive: it fills in derived data that is missing
and never rewrites what is already there. Two stages are opt-in:

- `--fix-dates` also corrects file dates from EXIF or a sidecar. It is opt-in
  because it rewrites existing date metadata.
- `--export` also writes `.xmp` sidecars beside your photos at the end.

`prune` (which deletes stale rows) is never run by the pipeline; run
[`videre prune`](/commands/prune/) yourself when you want it.

## The cost gate

`embed` and `classify` are the expensive stages: on a large library they can
take hours. Before the first one with real outstanding work, the pipeline shows
what it is about to do and roughly how long, and asks:

```
About to embed 12,431 files (~1.6h). Proceed? [y/N]
```

Answer no and it skips the slow stages and finishes the rest. Pass `--yes` to
skip the question, or `--skip embed,classify` to leave them out entirely.

## Selecting a library

Like `scan` and `watch`, `pipeline` operates on the library you select with
`--library` (or the current directory). It has no `--path` or other
walk-narrowing filter: it brings the whole selected library current.

## Options

| flag | meaning |
|---|---|
| `--skip <stage>[,<stage>...]` | leave stages out; repeatable and comma-separated |
| `--fix-dates` | also run fix-dates (rewrites dates from EXIF/sidecar) |
| `--export` | also write `.xmp` sidecars at the end |
| `--yes`, `-y` | run the expensive stages without asking |
| `--dry-run` | print the plan and cost, then exit without doing work |
| `--json` | emit a single JSON object instead of the checklist |
| `--silent` | suppress the checklist and per-stage progress |

## See also

- [`videre status`](/commands/status/) reports what is stale and what to run,
  without changing anything. `videre pipeline` is the command that closes those
  gaps.
- [`videre watch`](/commands/watch/) keeps a library fresh continuously in the
  background.
