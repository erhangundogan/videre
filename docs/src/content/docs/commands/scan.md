---
title: videre scan
description: Scan the current library and record every supported media file.
---

Scans the selected library recursively and records every
[supported media file](/reference/file-types/) in `.videre/hashes.db` inside
that library. Run this first. Other commands read the database it creates.

```bash
cd ~/Photos
videre scan                    # scan the current directory
videre scan --similar          # also prepare near-duplicate matching
videre scan --retry-incomplete # process new or unfinished files only
videre scan --silent           # suppress progress output
videre scan --json             # print one JSON summary object
```

Use `--library` to select a different library without changing directory:

```bash
videre --library ~/Photos scan
videre scan --library ~/Photos
```

`--library` may appear before or after the command, but only once. A relative
value is resolved from the directory where videre was invoked.

## Local state

The first scan creates this state inside the selected library:

```text
.videre/
├── config.toml
├── hashes.db
└── locks/
```

The database location is fixed. `scan` has no directory operand, `--path`,
`--db`, `--output`, or `--output-sqlite` option. To scan another collection,
run the command from that directory or select it with `--library`.

Re-running is safe. Existing rows are updated by path while annotations and
location fields owned by later processing are preserved.

## What it records

For every file, scan stores its content hash, size, timestamps, extension,
detected type, and available media metadata. Photo metadata includes capture
date, GPS coordinates, and dimensions. Video metadata can also include capture
date, GPS coordinates, dimensions, duration, and codec.

Scan does not prepare semantic search or detect faces. Run
[`videre embed`](/commands/embed/) and [`videre faces`](/commands/faces/)
separately for those features.

## `--retry-incomplete`

A normal scan reads every byte of every supported file. This is the expensive
part of scanning a large library.

`--retry-incomplete` still walks the library, but opens only files with no row
or no recorded media type. It also picks up files added since the previous
scan. A file whose bytes were read but whose type could not be identified gets
an explicit sentinel, so later retry runs do not repeatedly read it.

```bash
videre scan                    # full refresh
videre scan --retry-incomplete # quick incremental pass
```

## `--similar`

`--similar` computes a perceptual fingerprint used by
[`videre dedupe --similar`](/commands/dedupe/) to find media that looks alike
without being byte-identical.

This requires image decoding, so it is slower than a normal scan. HEIC and
video poster frames use QuickLook and are available on macOS. Files that cannot
be decoded still keep their normal scan record without a perceptual hash.

## Reading marks from XMP

Scan reads ratings, colour labels, and keywords from an adjacent XMP sidecar or
an embedded XMP packet. The `--xmp` option controls how ratings and labels are
reconciled with database values:

| Value | Behaviour |
|---|---|
| `db` | Database values win. XMP fills missing values |
| `file` | XMP values win |
| `newest` | Reserved for timestamp comparison; currently behaves as `db` with a warning |

The local default is configured with:

```bash
videre config set xmp db
```

Keywords are imported as additive [tags](/commands/tag/), independently of
the rating and label precedence.

## Nested libraries

A parent scan includes media in nested directories, even when one of those
directories is also used as a separate library. It skips every `.videre`
directory before descending, so nested databases, config files, caches, and
other state are never scanned as media or merged into the parent library.

For example, scanning `~/Photos/x` and later scanning `~/Photos` creates two
independent databases. The parent sees media under `x`; it does not adopt or
modify `x/.videre`.

## Caveats

**Rows are keyed by path.** Moving a file creates a new path on the next scan
and leaves the old row until [`videre prune`](/commands/prune/) removes it.
Faces, embeddings, marks, and tags are keyed by content hash.

**Unreadable files are skipped.** Permission failures and files that time out
are reported without stopping the rest of the scan. Use
`--retry-incomplete` after fixing the underlying problem.

**A full scan reads every byte.** On external drives and network shares, disk
speed usually dominates. Set a lower local read-rate assumption for a slower
mount:

```bash
videre config set read-rate 5
```

See [tuning](/guides/tuning/#slow-drives-and-large-files) for details.
