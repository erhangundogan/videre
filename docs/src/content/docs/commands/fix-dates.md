---
title: videre fix-dates
description: Set each file's modification time from the date the camera recorded.
---

Sets each file's modification time from its EXIF date, so photos sort by when
they were taken rather than when they were last copied.

:::danger[This is the only command that changes your files]
There is no undo. Once a file's original modification time is overwritten, it is
gone unless you have a backup. It asks for confirmation first, and `--dry-run`
shows exactly what it would do.
:::

```bash
videre fix-dates --dry-run             # show what would change, touch nothing
videre fix-dates                       # apply (asks for confirmation first)
videre fix-dates --yes                 # apply without asking (for scripts)
videre fix-dates --silent              # no per-file output
videre fix-dates --db ~/photos.db      # use a specific database
```

## Why you would want this

Copying, syncing, restoring from backup, or exporting from a photo app all
rewrite a file's modification time to *now*. A folder of holiday photos ends up
all dated the day you copied them, and every tool that sorts by date, from `ls`
to Finder to your file manager, shows them in the wrong order.

The date the camera recorded is still there, in EXIF, untouched by any of that.
This copies it onto the file itself.

```bash
videre scan ~/Photos           # read the EXIF dates
videre fix-dates --dry-run     # see what would change
videre fix-dates               # apply
```

## Exactly what changes

**Changed:** the file's modification time (`mtime`), and only that.

**Not changed:**

| | |
|---|---|
| File contents | Never read or rewritten. Only the timestamp metadata is set |
| Access time (`atime`) | Left as it was |
| Creation / birth time | Not touched. See the platform notes below |
| Filename, path, permissions | Untouched |
| The database | Not updated by this command; run [`videre prune`](/commands/prune/) to re-sync `modified_at` |

Sub-second precision is set to zero, since EXIF dates have one-second
resolution. A file whose mtime was `10:31:07.482` becomes `10:31:07.000`.

## Which files are affected

Only files that actually have an EXIF date. In practice that means camera
photos: `jpg`, `jpeg`, `tiff`, `heic` and `dng`.

Screenshots, PNGs, memes, and most videos have no EXIF date and are left alone
entirely. On a mixed library, expect a good fraction of files to be untouched,
and the count printed before the prompt tells you exactly how many will change.

Both KEEP and REMOVE candidates are included, since duplicates you are about to
delete do not benefit from being skipped.

## Platform notes

### macOS

macOS files have a **birth time** (creation date) that is separate from mtime,
and this command does not set it. There is no portable way to, and doing so
needs a macOS-specific syscall.

The practical consequence: in Finder, **Date Modified** will be correct after
running this, while **Date Created** still shows when the file was copied. If
you sort or browse by Date Created, this command will look like it did nothing.

Finder's default for many views is Date Modified, and `ls -lt`, most file
managers, and most photo tools use mtime, so the fix usually shows up where it
matters.

### Linux

There is no birth time to worry about: most Linux tooling only exposes mtime,
which is what this sets, so the result is what you would expect.

One side effect worth knowing: setting mtime updates the inode change time
(`ctime`) to now, as a matter of how the filesystem works. `ctime` cannot be set
by any program, and almost nothing surfaces it, but backup tools that use it for
change detection will see these files as modified and may re-copy them.

That is worth thinking about before running this across a large library that is
backed up incrementally: a fix-dates pass can trigger a large re-upload.

## Timezones

EXIF dates are **camera-local with no timezone**. A photo taken at 14:30 records
`14:30`, with nothing recording where in the world that was.

videre interprets that as local time on the machine running the command. So a
photo taken at 14:30 in Tokyo, processed on a machine set to Berlin time, gets
an mtime corresponding to 14:30 Berlin.

The consequence is that **running this on machines in different timezones
produces different results** for the same photos. If that matters to you, run it
consistently in one place.

Times that are ambiguous or impossible in the local timezone, which happens
during daylight-saving transitions, are reported as an error and skipped rather
than guessed:

```
Error: /Photos/2021/IMG_2043.jpg: ambiguous local time for 2021-10-31T02:30:00
```

That is one hour a year, so it affects very few photos, and they keep their
existing mtime.

## The confirmation prompt

Before changing anything it prints how many files will be touched and asks
`[y/N]` on stderr. Anything other than `y` or `yes` aborts with no changes,
including end-of-file, so a script with stdin closed aborts safely rather than
proceeding.

`--yes` skips the prompt for scripted use. `--dry-run` never prompts, since it
changes nothing, and the prompt is skipped entirely when there is nothing to do.

## Caveats

**No undo.** Take a backup first if the existing timestamps have any value to
you. `--dry-run` costs nothing and shows the exact before and after.

**The database is not updated.** `file_hashes.modified_at` still holds the old
value until you run [`videre prune`](/commands/prune/), which refreshes
timestamps for files still on disk. Nothing depends on this being current, but
`videre stats` and reports will show the old dates until then.

**Files missing on disk are skipped**, not treated as errors. Deleted duplicates
still recorded in the database fall into this category, and appear in the
summary as skipped.

**Exits nonzero if any file could not be updated**, for example on a read-only
volume or a permissions error. Missing files do not count.

**It trusts the EXIF date.** A camera with a wrong clock produced wrong EXIF, and
this faithfully copies that wrong date onto the file. Dates of `0000-00-00`,
which unset clocks produce, are recognised as invalid and skipped, but a clock
set to the wrong year is indistinguishable from a correct one.
