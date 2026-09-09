---
title: videre scan
description: Scan the current library and record every supported media file.
---

Scans the selected library recursively and records every
[supported media file](/reference/file-types/) in `.videre/hashes.db` inside
that library. Run this first. Other commands read the database it creates.

```bash
cd ~/Photos
videre scan                    # scan the current directory (only new and changed files)
videre scan --similar          # also prepare near-duplicate matching
videre scan --force            # re-read and re-hash every file
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

The database location is fixed at `.videre/hashes.db` inside the library. `scan`
takes no directory operand, no path filter, and no database or output-location
selector: the library is chosen the same way for every command. To scan another
collection, run the command from that directory or select it with `--library`.

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

## Incremental by default

Scan skips a file whose recorded row is already current: the same size and
modification time as the last scan, with its type already identified (and a
perceptual hash already present when `--similar` is used). New files, and files
that changed since the last scan, are processed; everything else costs a cheap
`stat`, not a re-read. So re-scanning a large library that has barely changed is
fast, and re-running is always safe.

```bash
videre scan          # only new and changed files
videre scan --force  # re-read and re-hash every file
```

`--force` ignores the skip and re-reads every file. Reach for it
after restoring from a backup, or to re-examine files whose contents changed
without their modification time changing (rare, but some tools preserve mtime).
`--retry-incomplete` is still accepted as a deprecated alias of the default and
no longer changes what is processed.

A file whose bytes were read but whose type could not be identified gets an
explicit sentinel, so it counts as complete and is not re-read on every scan.

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

Because scan is incremental, a change to a sidecar **alone** (you re-rate a
photo in another tool but the media file itself is untouched) is not noticed on
a normal scan, since the media file looks unchanged. Run `videre scan --force`
to re-read files and re-import marks from sidecars that changed on their own.

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
are reported without stopping the rest of the scan. A file with no recorded row
is always retried, so just run `videre scan` again after fixing the underlying
problem.

**Reading a file's bytes is the expensive part.** A first scan, a changed
file, or a `--force` pass reads every byte; on external drives and network
shares disk speed usually dominates. Set a lower local read-rate assumption for
a slower mount:

```bash
videre config set read-rate 5
```

See [tuning](/guides/tuning/#slow-drives-and-large-files) for details.
