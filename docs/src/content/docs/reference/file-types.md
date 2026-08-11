---
title: Supported files
description: Which file types videre reads, and what it can do with each.
---

`.jpg` `.jpeg` `.png` `.gif` `.webp` `.bmp` `.tiff` `.heic` `.mov` `.mp4` `.dng`

## What works with what

| Type | Scan & dedupe | EXIF | Search | Faces | Near-duplicate |
|---|---|---|---|---|---|
| jpg, jpeg, tiff | yes | yes | yes | yes | yes |
| png, gif, webp, bmp | yes | — | yes | yes | yes |
| heic | yes | yes | macOS only | macOS only | no |
| mov, mp4 | yes | — | macOS only | — | macOS only |
| dng | yes | yes | no | no | no |

Everything is scanned, hashed and exactly de-duplicated regardless. The gaps
above are about what can be *decoded*, not what is recorded.

`.dng` is skipped for search because no DNG decoder is available. Its EXIF
metadata is still read.

See [platform support](/reference/platforms/) for why HEIC and video need macOS.

## File type detection

Types are identified by the file's actual leading bytes, not its name, so a
mislabeled file is still handled correctly. This costs nothing extra, since the
bytes are already being read to hash the file.

A `.png` that is really a JPEG is treated as a JPEG. A `.dng` reports as TIFF,
which it genuinely is, so DNG is excluded from search by extension rather than
by type.

When the bytes match nothing recognised, the file is recorded as
`application/octet-stream` rather than left empty. That is a real answer, not a
failure: it records that the question was asked and settled, which is what stops
[`scan --retry-incomplete`](/commands/scan/) reopening the same file on every
run. Such a file is still processed, falling back to its extension.

Files scanned before type detection existed have no recorded type until you
re-scan; those also fall back to the extension.

## EXIF fields

EXIF is read from `jpg`, `jpeg`, `tiff`, `heic` and `dng`. Fields are empty when
the file carries no EXIF data.

| Field | Notes |
|-------|-------|
| Date taken | `DateTimeOriginal`, camera-local with no timezone |
| GPS latitude | Decimal degrees, negative is South |
| GPS longitude | Decimal degrees, negative is West |
| Width, height | Pixel dimensions |

Dates of `0000-00-00`, which cameras with an unset clock produce, are discarded
rather than stored.

## Near-duplicate detection for video

For `.mov` and `.mp4`, the fingerprint comes from a single poster frame, not the
video content. So it catches re-encodes and trims that keep the opening frame,
but not a trim that cuts it.

That output is review-only and never reaches
[`videre dedupe`](/commands/dedupe/)'s pipeable output, so a false match costs
you a noisy group in the report, never a wrong deletion.
