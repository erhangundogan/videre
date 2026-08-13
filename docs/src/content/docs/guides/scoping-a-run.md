---
title: Scoping a run
description: Narrow any long-running command to part of your library with the same filter flags.
---

Most videre commands work through your whole library. On a large one that can
mean hours. The same filter flags that narrow a
[search](/guides/compositional-search/) also narrow the *work*, so you can
embed only your videos, detect faces only in last summer's photos, or scan only
one subfolder.

The flags mean the same thing everywhere. What changes is which ones a command
offers, and a command only offers the ones it can actually answer.

```bash
videre embed --type video                      # only videos
videre faces --after 2024-06 --before 2024-09  # only that summer
videre scan ~/Photos --ext heic,mov            # only these two formats
videre classify --location "Berlin, Germany"   # only photos taken near Berlin
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

`--type`, `--ext` and `--mime` are repeatable and accept comma-separated lists:
`--ext mov,avi` and `--ext mov --ext avi` are the same request.

Combining flags narrows further: every condition must hold. `--type video
--after 2024-01-01` means videos *and* taken this year, never either.

## Which commands take which flags

| Command | Flags |
|---|---|
| [`search`](/commands/search/) | all of them |
| [`classify`](/commands/classify/) | all of them |
| [`embed`](/commands/embed/), [`faces`](/commands/faces/) | everything except `--person` and `--category` |
| [`scan`](/commands/scan/), [`watch`](/commands/watch/) | `--type`, `--ext`, `--mime`, `--path` |

The gaps are deliberate rather than unfinished.

`scan` and `watch` walk the filesystem, and a walk has not opened the file yet.
Nothing on disk says when a photo was taken until something reads it, and
reading every file is the expensive work you were trying to narrow. So they
take only the flags answerable from a path.

`embed` and `faces` decline `--person` and `--category`, because both are
derived from the very data those commands produce. Selecting the input by a
label that only exists once the run has finished is circular, so the flag does
not exist rather than quietly matching nothing.

[`videre locations`](/commands/locations/) takes no filters at all. It
recalculates every location cluster from scratch each time, so a scoped run
would not do less work, it would leave everything outside the scope
unclustered.

## A scoped run always tells you

Every scoped command reports what it passed over:

```
Embedding 412 of 70,601 pending file(s) (--type video)
```

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

## Worked examples

**Get a new library searchable in stages.** Embedding everything takes hours;
this gets the recent material usable first and leaves the backlog for overnight.

```bash
videre scan ~/Photos                       # cheap, do it all at once
videre embed --after 2025-01-01            # this year first
videre embed --type video                  # then the videos
videre embed                               # then the rest, skipping both above
```

**Only the part of the disk that changed.** A folder you just imported into,
without rewalking a library of tens of thousands of files.

```bash
videre scan ~/Photos --path ~/Photos/2026-01-import
videre embed --path ~/Photos/2026-01-import
videre faces --path ~/Photos/2026-01-import
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

**Watch only what matters.** An inbox folder, images only, leaving the archive
alone.

```bash
videre watch ~/Photos --path ~/Photos/Inbox --type image
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
