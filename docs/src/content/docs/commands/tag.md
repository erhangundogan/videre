---
title: videre tag
description: Add or remove free-form tags on photos, then find them with search --tag.
---

Tags a photo with any word or phrase you like. Tags are stored per photo (by
content hash, so they follow a photo across duplicates and moves) and compose
with every other filter in [`videre search`](/commands/search/).

```bash
videre tag --add beach --add summer --path ~/Photos/2024   # add on a selection
videre tag --add "to print" --person "Ayşe" --rating 5      # target by any filter
videre tag --remove beach --path ~/Photos/2024/blurry       # remove
```

## Setting tags

Give at least one `--add` or `--remove` (both are repeatable, and you may pass
both in one run: removals apply first, then additions).

| Flag | Effect |
|---|---|
| `--add <tag>` | Add this tag to every file in the selection |
| `--remove <tag>` | Remove this tag from every file in the selection |

## Choosing what to tag

The selection flags narrow the library the same way `search` does: `--person`,
`--category`, `--date`/`--after`/`--before`, `--location`/`--radius`, `--type`,
`--ext`, `--mime`, `--path`, `--has`, `--missing`. See
[scoping a run](/guides/scoping-a-run/). A run prints `N of M`. With no
selection, every file is tagged.

There is no `--tag` filter here: on this command a tag means *set*, not filter.
To retag by an existing tag, select the files another way.

## Finding photos by tag

`--tag` is a filter on [`videre search`](/commands/search/), composing with
everything else. It is repeatable and ANDed: every named tag must be present.

```bash
videre search --tag beach
videre search --tag beach --tag summer --person "Ayşe" --rating 4
```

## Portability

Tags round-trip through standard XMP `dc:subject` keywords: a scan reads keywords
from a photo's sidecar or embedded packet and stores them as tags, and
[`videre export --xmp`](/commands/export/) writes your tags back out as
`dc:subject`, so digiKam, Lightroom and darktable see them. Hierarchical
keywords (`lr:hierarchicalSubject`) are not modelled yet; a tag is one flat
string, and a `/`-separated value is stored verbatim.

## Other options

| Flag | Effect |
|---|---|
| `--db <path>` | Act on a specific database instead of the [resolved default](/commands/config/) |
| `--silent` | Suppress the `N of M` line |
