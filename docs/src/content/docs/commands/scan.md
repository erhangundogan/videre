---
title: videre scan
description: Read a folder recursively and record every media file in the database.
---

Reads a folder recursively and records every
[supported media file](/reference/file-types/) in the database. Run this first:
everything else reads from what it creates.

```bash
videre scan ~/Photos                              # scan into the default database
videre scan                                       # same, using the folder from `videre config set path`
videre scan ~/Photos --similar                    # also fingerprint images/videos for near-duplicate detection
videre scan ~/Photos --db ~/photos.db             # write to a specific database instead
videre scan ~/Photos --output                     # write JSONL to ~/.videre/hashes.jsonl instead of SQLite
videre scan ~/Photos --output out.jsonl           # write JSONL to a specific file
videre scan ~/Photos --retry-incomplete           # only files an earlier scan didn't finish
videre scan ~/Photos --silent                     # no progress output
videre scan ~/Photos --json                       # print one JSON summary object instead
```

Re-running is safe and idempotent, since existing entries are updated in place.

:::note
`--output` and `--db` cannot be combined. A bare `--output` must come *after*
the folder, or it swallows the folder as its value.

`--output-sqlite` still works as an alias for `--db`. It was the original name,
from when JSONL and SQLite were peer output *formats* rather than one
destination and one opt-out. Existing scripts do not need changing.
:::

## What it records

For every file: its content hash, size, timestamps, extension, detected type,
and EXIF where present (date taken, GPS, dimensions). Nothing is opened for
decoding unless you pass `--similar`.

It does **not** prepare search or detect faces. Those are
[`videre embed`](/commands/embed/) and [`videre faces`](/commands/faces/), run
separately and much slower.

## `--retry-incomplete`

A normal scan re-reads every byte of every file, which on a large library is the
slow part by far: about 10 minutes and 460 GB of reading for 70,000 files, of
which walking the folder is under two seconds.

`--retry-incomplete` still walks the folder but opens only files that have no
entry yet, or whose entry a previous run left unfinished, such as an interrupted
scan or a file that timed out on a slow drive. On an already-complete library of
that size it finishes in about a second, having opened nothing. New files are
picked up too, since they have no entry yet.

```bash
videre scan ~/Photos                      # the full pass, occasionally
videre scan ~/Photos --retry-incomplete   # the quick pass, routinely
```

It needs a database to consult, so it cannot be combined with `--output`, which
writes JSONL.

## `--similar`

Also computes a perceptual fingerprint, which is what lets
[`videre dedupe --similar`](/commands/dedupe/) find photos that merely *look*
alike rather than being byte-identical.

This decodes every image, so it is substantially slower than a plain scan. It is
worth doing once when you intend to hunt near-duplicates, not routinely.

For `.mov` and `.mp4` this needs macOS, since the frame is extracted with
QuickLook. Elsewhere those files simply get no fingerprint, the same graceful
skip as any other undecodable file. HEIC files never get one.

## Caveats

**Rows are keyed by path, so a moved file looks like a new one.** Re-scanning
after reorganising folders adds rows at the new paths and leaves the old ones
behind, pointing at files that no longer exist. Run
[`videre prune`](/commands/prune/) afterwards to clear them. Nothing derived is
lost in the meantime, because faces and embeddings are keyed by content, not
path.

**Scanning several folders puts them all in one database.** That is often what
you want, but it means `dedupe` and `prune` then act across all of them, and a
bare `videre scan` with no argument only refreshes the *first* folder you ever
scanned. See
[scanning more than one folder](/reference/paths/#scanning-more-than-one-folder)
for the full picture and how to keep collections separate.

**The first folder you scan becomes your default.** It is adopted automatically
so later commands work with no arguments, it prints a note when it happens, and
it never overwrites a folder you configured yourself. Change it with
`videre config set path <dir>`.

**Unreadable files are skipped, not fatal.** A permissions error or a file that
times out on a slow or disconnected drive leaves that file unrecorded and the
scan continues. `--retry-incomplete` is how you pick them up later once the
cause is fixed.

**A full scan reads every byte.** On an external drive or a network share that
is the dominant cost, and it is why `--retry-incomplete` exists. Nothing is
written to your files at any point.

## More detail

- [JSONL output](/guides/jsonl/) covers `--output` and what it gives up.
- [Keeping libraries separate](/guides/multiple-libraries/) covers scanning collections that should not see each other.


## HEIC and near-duplicates

`--similar` hashes HEIC as well as JPEG, PNG, GIF, WebP, BMP, TIFF and video.
HEIC cannot be decoded directly, so it converts through QuickLook exactly as
[`embed`](/commands/embed/) and [`faces`](/commands/faces/) do, which means
HEIC near-duplicate detection is macOS-only.

The conversion is cheap per file, because the hash only needs a 9x8 grid, so
videre asks QuickLook for a 64px rendition rather than a full-size one. It is
not cheap in bulk: HEIC and video both pay a conversion, so on a library made
mostly of those, `--similar` is a long job.

Measured on 700 real files (300 HEIC, 300 JPEG, 60 MOV, 40 PNG, 2.3 GB) on a
10-core machine, with the files already in the page cache so the figure
reflects decoding rather than disk:

| | Time |
|---|---|
| before parallel hashing (0.13.0) | 108s |
| after (0.13.1) | 15s |

Expect less than that 7.2x on an external drive, where reading the files, not
decoding them, becomes the limit. Progress is shown throughout, so a long run
is visibly a long run rather than an apparent hang.

:::note[Matching survives resizing]
The perceptual hash compares images by shape, not bytes, so a photo matches its
own downscaled copy. Measured on a real library, a HEIC original and its
768x1024 preview differed by 0 to 5 bits out of 64, and 29,258 real rendition
pairs were within 4 bits 99.1% of the time. Exact equality only held for 53.4%
of them, which is why near-duplicate grouping uses a distance rather than an
equality test.
:::

## Very large files

Reading a file is bounded by a timeout, so a disconnected drive cannot hang a
scan forever. That bound scales with the size of the file rather than being a
constant: a multi-gigabyte video legitimately takes longer to read than a
photo, and a fixed ceiling cannot tell a large file from a stalled one.

The size itself is read first under a short, separate timeout. That is what
keeps a dead mount failing quickly: it stops there, and the read is never
started.

If you scan a mount slower than about 20 MB/s and see files reported as
unreachable, lower the assumed floor rate:

```bash
videre config set read-rate 5
```

## Video metadata

Scanning reads each video's own metadata: capture date, GPS coordinates,
dimensions, duration and codec. Those feed the same features photos already
use, so [`videre search`](/commands/search/) date and location filters,
`--near`, and [`videre locations`](/commands/locations/) all cover video.

Dates are stored as local wall-clock time, exactly as photo dates are, so the
two sort and filter together rather than drifting by a timezone.

:::caution[Libraries scanned before v0.14.0 need re-scanning]
`--retry-incomplete` only revisits files with no recorded type, which is not the
same as "scanned before video metadata existed". Run `videre scan` over the
library again to fill these in:

```bash
videre scan ~/Pictures
```
:::

Not every video carries coordinates - a clip recorded with location services
off has none, and on one real library 16 of 260 were in that position. Those
files are still scanned and searchable; they simply do not appear in
location-filtered results.
