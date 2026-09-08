---
title: Scoping a run
description: Narrow any long-running command to part of your library with the same filter flags.
---

Most videre commands work through your whole library. On a large one that can
mean hours. The same filter flags that narrow a
[search](/guides/compositional-search/) also narrow the *work*, so you can
embed only your videos, detect faces only in last summer's photos, or process
only one subfolder.

The flags mean the same thing everywhere. What changes is which ones a command
offers, and a command only offers the ones it can actually answer.

```bash
videre embed --type video                      # only videos
videre faces --after 2024-06 --before 2024-09  # only that summer
videre --library ~/Photos scan --ext heic,mov # record only these two formats
videre classify --location "Berlin, Germany"   # only photos taken near Berlin
videre search --missing gps                     # photos with no coordinates
```

## The flags

| Flag | Selects |
|---|---|
| `--type` | `image` or `video` |
| `--ext` | file extension, e.g. `mov` |
| `--mime` | exact type, e.g. `video/quicktime` |
| `--after`, `--before`, `--date` | when the file was taken |
| `--location`, `--radius` | where it was taken |
| `--person` | who is in it |
| `--category` | how [`videre classify`](/commands/classify/) labelled it |
| `--path` | which folder it is in |
| `--has`, `--missing` | whether metadata exists. Supported fields: `gps`, `date` |
| `--rating` | [rated](/commands/mark/) at least this many stars (0-5) |
| `--pick` | pick state: `keep` or `reject` |
| `--label` | colour label, e.g. `Green` |
| `--like` | only liked (favourite) files |
| `--tag` | carries this [tag](/commands/tag/). Repeatable; all must be present |

`--type`, `--ext`, `--mime`, `--has`, `--missing` and `--tag` are repeatable and
accept comma-separated lists where a list makes sense: `--ext mov,avi` and
`--ext mov --ext avi` are the same request, as are `--missing gps,date` and
`--missing gps --missing date`. Multiple `--tag` values are ANDed: every named
tag must be present.

The last five - `--rating`, `--pick`, `--label`, `--like`, `--tag` - filter on
the marks and tags you set with [`videre mark`](/commands/mark/) and
[`videre tag`](/commands/tag/). They compose with everything above.

Combining flags narrows further: every condition must hold. `--type video
--after 2024-01-01` means videos *and* taken this year, never either.

## Which commands take which flags

| Command | Flags |
|---|---|
| [`search`](/commands/search/), [`classify`](/commands/classify/), [`export`](/commands/export/) | all of them |
| [`tag`](/commands/tag/) | all of them (its own `--add`/`--remove` are the setters) |
| [`embed`](/commands/embed/), [`faces`](/commands/faces/) | everything except `--person` and `--category` |
| [`mark`](/commands/mark/) | all except the mark-value filters; the only mark/tag filter it takes is `--tag` |
| [`scan`](/commands/scan/), [`watch`](/commands/watch/) | `--type`, `--ext`, `--mime` |

The gaps are deliberate rather than unfinished.

`scan` and `watch` walk the whole selected library, and a walk has not opened
the file yet. Nothing on disk says when a photo was taken until something reads
it, and reading every file is the expensive work you were trying to narrow. So
they take only the flags answerable from a filename (`--type`, `--ext`,
`--mime`), and no `--path`: their scope is the library you select, so to walk a
different subset you point them at a different library.

`embed` and `faces` decline `--person` and `--category`, because both are
derived from the very data those commands produce. Selecting the input by a
label that only exists once the run has finished is circular, so the flag does
not exist rather than quietly matching nothing.

`mark` is the one command where `--rating`, `--pick`, `--label` and `--like` are
*setters*, not filters: `videre mark --rating 5` gives files five stars. So on
`mark` those four cannot also filter, and the only mark/tag filter it accepts is
`--tag` (narrowing which files get the mark, e.g. `videre mark --tag vacation
--rating 5`). `tag` has the opposite arrangement: its setters are the separate
`--add`/`--remove` flags, so every filter above - including `--tag` - is free to
narrow which files it (un)tags.

[`videre locations`](/commands/locations/) takes no filters at all. It
recalculates every location cluster from scratch each time, so a scoped run
would not do less work, it would leave everything outside the scope
unclustered.

## A scoped run always tells you

Every scoped command reports what it passed over:

```
Embedding 412 of 70,601 pending item(s) (--type video)
```

`embed`, `classify` and `faces` all word this the same way, differing only in
the verb.

Filtering narrows an existing set, it does not redefine it. `videre embed
--type video` still only considers files that were pending anyway, so the
number on the right is the unfiltered work, not your library size.

This matters because a filter that matches nothing is not an error. If you
expected thousands and see `0 of 70,601`, the filter is wrong, not the library.

## Missing data excludes a file

If a filter needs information a file does not have, that file does not match.
A photo with no GPS never appears in a `--location` search, and one with no
date never appears in a `--date` one. It is not treated as "unknown, so maybe":
you asked for a place, and a file with no place is not it.

For dates specifically there is a fallback before that rule applies: videre
uses the EXIF date when there is one, and the file's modification time when
there is not. Only a file with neither is excluded.

This is the same rule everywhere, and it means you can widen a run in stages.
If `--location "Berlin, Germany"` covers fewer files than you expected, the
missing ones are the ones without coordinates; scan them or fill in their
metadata rather than loosening the radius.

Use `--missing gps` or `--missing date` when the missing metadata is itself the
thing you want to fix or inspect. `--has gps` means both latitude and longitude
are present; a file missing either coordinate matches `--missing gps`.

## Worked examples

**Get a new library searchable in stages.** Embedding everything takes hours;
this gets the recent material usable first and leaves the backlog for overnight.

```bash
cd ~/Photos
videre scan                                # cheap, do it all at once
videre embed --after 2025-01-01            # this year first
videre embed --type video                  # then the videos
videre embed                               # then the rest, skipping both above
```

**Only the part of the library that changed.** `scan` always walks the whole
library, but it is cheap on unchanged files, and the expensive stages take
`--path`, so you bound *those* to the subfolder you just imported into.

```bash
videre scan                                    # refresh the whole library (cheap)
videre embed --path ~/Photos/2026-01-import    # embed only the new subfolder
videre faces --path ~/Photos/2026-01-import    # detect only there
```

**Faces from one event.** Detection is the expensive stage, so bounding it by
date and place is the difference between minutes and an afternoon.

```bash
videre faces --location "Rome" --radius 10 --after 2024-06-01 --before 2024-07-01
```

**Re-label one slice after changing your mind about categories.**

```bash
videre classify --type image --path ~/Photos/screenshots --reprocess
```

**Skip formats that are slow and rarely worth it.** HEIC decoding goes through
QuickLook and dominates a run; this does the cheap formats first.

```bash
videre embed --ext jpg,png,mp4             # fast formats now
videre embed --ext heic                    # the slow ones separately
```

**Watch only what matters.** An inbox that is its own library, images only,
leaving the archive alone.

```bash
videre --library ~/Photos/Inbox watch --type image
```

**Find it afterwards, with the same vocabulary.**

```bash
videre search "birthday" --person "Alice" --type image \
  --location "Berlin, Germany" --date 2025-05
```

## Narrowing a job you have already started

Scoping composes with resumability rather than replacing it. Commands already
skip work they have finished, so a scoped run is "the part I want, minus what
is already done":

```bash
videre embed --type video      # get the videos done first
videre embed                   # then everything else, videos already skipped
```

The second command does not redo the first. See
[long-running jobs](/guides/long-running-jobs/) for stopping and resuming.
