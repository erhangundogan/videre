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

### Where a file's date comes from

Every file gets one capture date, the one every date filter, the gallery's date
pages, Events and sorting use. Scan takes the first of these the file has:

| Source | When |
|---|---|
| The photo's EXIF date | the camera recorded one |
| A video's own creation date | the clip carries Apple's date field |
| The Google Takeout sidecar's `photoTakenTime` | the file sits beside a Takeout `.json` and records no date itself |
| A video's container time | a clip with nothing better |
| The file's own modification time | nothing above exists |

Dates are kept as the clock time where the photo was taken, as EXIF writes them.
A source that gives a moment in UTC (a sidecar, a container time, a file time)
is converted to this machine's local time, the same assumption
[`fix-dates`](/commands/fix-dates/#timezones) makes in the other direction. An
EXIF time of `24:03` (written by some Android cameras for three minutes past
midnight) is read as `00:03` the next day.

A photo with no GPS of its own takes its place from the Takeout sidecar too.
[`videre status`](/commands/status/) shows how many dates come from each source.

A library scanned by an older videre has its dates worked out on the next scan,
from what it already recorded, without reading any file again in full.

Scan does not prepare semantic search, near-duplicate fingerprints or faces.
Run [`videre embed`](/commands/embed/) and [`videre faces`](/commands/faces/)
separately for those features.

## Incremental by default

Scan skips a file whose recorded row is already current: the same size and
modification time as the last scan, with its type already identified. New files, and files
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

A file whose bytes were read but whose type could not be identified gets an
explicit sentinel, so it counts as complete and is not re-read on every scan.

## Reading marks from XMP

Scan reads ratings, colour labels, and keywords from an adjacent XMP sidecar and
from the XMP packet embedded in the photo. They are combined field by field: the
sidecar's value wins where it has one, the photo's fills whatever the sidecar
lacks, and keywords from both are kept. So a rating another app writes into the
photo is read even when the photo also has a sidecar, for example one
[`videre export`](/commands/export/) wrote with face regions only. The `--xmp`
option controls how ratings and labels are reconciled with database values:

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

Reading is incremental. A file's XMP is read the first time it is scanned and
again whenever it changes: either the media file itself changes, or its sidecar
changes on its own (you re-rate a photo in another tool while the media file is
untouched). A sidecar change is detected from the sidecar's modification time,
so re-rating in another tool is picked up on the next scan without re-reading
the media. A file whose media and sidecar are both unchanged is skipped
entirely, so a repeat scan reads no metadata at all. `--xmp file` and `--xmp
newest` still reconcile every file, unchanged or not, because they exist to
overwrite the database from the file.

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
shares disk speed usually dominates. Hashing has no total time limit. It
continues as long as bytes arrive, even for a very large or slow file, and
skips a file after 20 seconds without read progress. The initial file stat
has a separate five-second limit. `read-rate` does not control hashing;
it still applies to other size-bounded file reads. See
[tuning](/guides/tuning/#slow-drives-and-large-files) for details.

While a file of 1 GB or more is being hashed, its bytes read so far and the
current rate show beside the progress bar (for several at once, their
totals), so one large video does not leave the counter sitting still for
minutes. A falling rate is the early sign of a stalled drive. Without a
terminal the same line is logged every 30 seconds; `--silent` shows none of
it.
