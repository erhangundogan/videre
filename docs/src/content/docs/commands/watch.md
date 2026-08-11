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
```

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

## Why `--heic` is worth running

It decodes each HEIC file once and caches the result, including a full-size
copy. [`videre faces`](/commands/faces/) then reads that cache instead of
decoding again: about 108 ms per image against 7.6 s, roughly 70x.

[`videre report --show-faces`](/commands/report/) uses the same cache, so
warming it removes the per-request conversion cost there too.

This cache has a real disk cost at scale, tens of GB for a HEIC-heavy library,
and only [`videre prune`](/commands/prune/) reclaims any of it.

## Running it alongside other commands

`videre watch` and `videre report --show-faces` are designed to run concurrently
against the same database.

Running `watch --faces` at the same time as a manual `videre embed` is a
different matter, since both drive HEIC conversion. See
[cautions](/start/cautions/).

`--prune` runs unattended, so it can override neither of prune's safety guards.

## Supervision

There is no daemon mode and no service unit. Run it in a terminal, a tmux or
screen pane, or your own process supervisor.
