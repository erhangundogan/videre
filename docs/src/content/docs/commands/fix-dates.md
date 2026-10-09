---
title: videre fix-dates
description: Set each file's modification time from the date the camera recorded.
---

Sets each file's modification time from its capture date: the EXIF date the
camera recorded, a video's own date, or the date in its Google Takeout sidecar.
Photos then sort by when they were taken rather than when they were last
copied, in any tool.

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
videre --library ~/Photos fix-dates    # select a different library
```

## Why you would want this

Copying, syncing, restoring from backup, or exporting from a photo app all
rewrite a file's modification time to *now*. A folder of holiday photos ends up
all dated the day you copied them, and every tool that sorts by date, from `ls`
to Finder to your file manager, shows them in the wrong order.

The date the camera recorded is still there, in EXIF, untouched by any of that.
This copies it onto the file itself.

```bash
videre --library ~/Photos scan           # read the EXIF dates
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
| The database | Only `modified_at`, which is set to the new time so the next scan does not treat the file as changed |

Sub-second precision is set to zero, since EXIF dates have one-second
resolution. A file whose mtime was `10:31:07.482` becomes `10:31:07.000`.

## Which files are affected

Files whose capture time something recorded (see
[where a file's date comes from](/commands/scan/#where-a-files-date-comes-from)):
camera photos with an EXIF date, videos with their own creation date, and, in a
Google Takeout export, photos with no EXIF date whose sidecar says when they
were taken. A file dated only by its own modification time is left alone, since
there is nothing better to set.

A file whose modification time already equals its capture date is left alone too,
so running this a second time changes nothing. The count in the prompt is the
number of files whose time will actually change, the same number
[`videre status`](/commands/status/) suggests, and the summary reports the rest
as already correct:

```
876 file(s) with a capture date, 8 updated, 868 already correct, 0 error(s).
```

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

The hours a daylight-saving change makes awkward are dated too. The hour the
clocks go back happens twice, and a photo from it takes the first pass, before
the change. The hour the clocks go forward never happens, so a camera that
showed it had not been changed yet, and the photo is read as the clock after
the jump, one hour later.

A date fix-dates cannot read at all is reported as an error and the file keeps
its mtime; [`videre status`](/commands/status/) counts it as skipped.

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

**Files missing on disk are skipped**, not treated as errors. Deleted duplicates
still recorded in the database fall into this category, and appear in the
summary as skipped. When there are any, fix-dates says so and points at
[`videre prune`](/commands/prune/), which drops their rows from the library.

**Exits nonzero if any file could not be updated**, for example on a read-only
volume or a permissions error. Missing files do not count.

**It is the only command that writes file times.** `videre import` used to set
Takeout dates itself, in UTC, while this command used local time, and the two
undid each other by hours. Now scan reads the sidecar and this command writes
every date, one way.

**It trusts the EXIF date.** A camera with a wrong clock produced wrong EXIF, and
this faithfully copies that wrong date onto the file. Dates of `0000-00-00`,
which unset clocks produce, are recognised as invalid and skipped, but a clock
set to the wrong year is indistinguishable from a correct one.
