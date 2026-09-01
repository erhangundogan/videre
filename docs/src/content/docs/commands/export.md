---
title: videre export
description: Write videre's labels (face regions, location, categories, ratings) to portable .xmp sidecars.
---

Writes the labels you built in videre into standard `.xmp` sidecar files next to
your photos, so other tools (digiKam, Lightroom, darktable) can read them. This
is how videre stops being an island: the work you invested in naming faces,
rating and labelling travels with your library instead of living only in
`hashes.db`.

Opt-in and non-custodial: it only ever writes a `.xmp` sidecar beside the file,
and never modifies the original.

```bash
videre export --xmp                          # every file with something to write
videre export --xmp --path ~/Photos/2024     # just one folder
videre export --xmp --person "Ayşe"          # just photos of one person
videre export --xmp --dry-run                # list what would be written
```

## What it writes

For each selected photo, videre writes the labels it owns into one sidecar,
using the standard fields the other tools already read:

| videre data | XMP field |
|---|---|
| Named faces (confirmed) | MWG face regions (`mwg-rs:Regions`), name plus box |
| Resolved location name | `Iptc4xmpCore:Location` |
| Category (photo/screenshot/document/meme) | `dc:subject` keyword |
| [Tags](/commands/tag/) | `dc:subject` keywords |
| Star rating | `xmp:Rating` |
| Colour label | `xmp:Label` |

Face regions are written as normalized MWG areas, the format digiKam and
Lightroom use for face tags, so a name you assigned in videre shows up as a named
face region there. Picks and likes have no portable standard and stay in videre's
database.

A file with nothing to write gets no sidecar.

## Merging, not clobbering

If a sidecar already exists (for example one Lightroom wrote), videre **merges**:
it replaces only the fields it owns and preserves everything else in the file
verbatim, including another tool's keywords and develop settings. Re-exporting is
safe and idempotent.

## Choosing what to export

The selection flags narrow the library exactly as [`videre search`](/commands/search/)
does: `--path`, `--person`, `--date`/`--after`/`--before`,
`--location`/`--radius`, `--type`, `--ext`, `--mime`, `--category`, `--has`,
`--missing`. See [scoping a run](/guides/scoping-a-run/). With no selection,
every file with something to write gets a sidecar.

A run prints `N of M`, so a filter matching nothing is distinguishable from an
empty library.

## Options

| Flag | Effect |
|---|---|
| `--xmp` | Write XMP sidecars (currently the only export format; required) |
| `--dry-run` | List the sidecars that would be written, write nothing |
| `--db <path>` | Act on a specific database instead of the [resolved default](/commands/config/) |
| `--silent` | Suppress the summary line |

## Reading it back

videre reads standard XMP on [`videre scan`](/commands/scan/), so labels other
tools wrote can come back into videre too. The `--xmp` precedence rule (which side
wins) is documented on the [scan](/commands/scan/) page. Continuous export while
you work in another tool is available as an opt-in [`watch`](/commands/watch/)
stage.
