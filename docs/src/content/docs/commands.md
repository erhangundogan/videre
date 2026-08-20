---
title: Commands
description: Every videre subcommand, what it does, and where to read more.
---

| Command | What it does |
|---------|--------------|
| [`videre import`](/commands/import/) | Bring photos in from Google Takeout, Apple Photos, or Lightroom |
| [`videre scan`](/commands/scan/) | Read a folder and record what's in it. Run this first. |
| [`videre dedupe`](/commands/dedupe/) | List duplicate copies you could delete |
| [`videre gallery`](/commands/gallery/) | Browse the library in a local web UI: files, people, dates |
| [`videre search`](/commands/search/) | Find photos by description, example image, person, category, or place |
| [`videre embed`](/commands/embed/) | Prepare photos for search (one-time, resumable) |
| [`videre faces`](/commands/faces/) | Detect and group faces |
| [`videre classify`](/commands/classify/) | Tag photos as photo/screenshot/document/meme |
| [`videre locations`](/commands/locations/) | Group photos by where they were taken |
| [`videre fix-dates`](/commands/fix-dates/) | Set each file's date from its EXIF shoot date |
| [`videre prune`](/commands/prune/) | Remove database entries for files that no longer exist |
| [`videre watch`](/commands/watch/) | Background loop keeping everything current |
| [`videre stats`](/commands/stats/) | Library totals and what has run recently |
| [`videre config`](/commands/config/) | Show or change defaults |
| [`videre mcp`](/commands/mcp/) | Expose search to AI agents |

Every command also takes `--help`.

## Common options

Most commands share these:

| Option | Effect |
|---|---|
| `--db <path>` | Use a specific database instead of the [resolved default](/reference/paths/) |
| `--silent` | Suppress progress output on stderr |
| `--json` | Print one JSON object on stdout instead of human-readable text |
| `--model <id>` | Use a specific [search model](/reference/models/) (`embed`, `search`, `classify`, `gallery`, `mcp`) |

## What needs what

| To run | You first need |
|---|---|
| `dedupe`, `gallery`, `fix-dates`, `prune`, `stats`, `locations` | `scan` |
| `scan`, when coming from another tool | `import` first, so dates are fixed before they are recorded |
| `search` (text or image) | `embed` |
| `search --person` | `faces`, then naming via `videre gallery` |
| `search --category` | `classify` (which needs `embed`) |
| `search --location` | GPS data in your photos |
| `search --date` / `--after` / `--before` | nothing beyond `scan` |

[Workflows](/start/workflows/) has the full dependency map, what to run
*afterwards* (`prune` is the one people forget), recipes for the common jobs,
and rough costs for the long ones.
