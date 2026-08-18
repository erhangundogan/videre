---
title: videre import
description: Bring photos in from Google Takeout, Apple Photos, or a Lightroom catalog.
---

Brings a library in from somewhere else: a Google Takeout export, an Apple
Photos or iPhoto library, or an Adobe Lightroom catalog. It works out what you
pointed it at, finds the actual files, and corrects the dates those tools left
behind.

```bash
videre import ~/Pictures                    # find whatever is in there
videre import ~/Takeout                     # a Google Takeout export
videre import ~/Pictures/Photos\ Library.photoslibrary
videre import                               # search the usual places
```

The bare path is the main form. You do not need to know whether you have a
`.photoslibrary`, a `.lrcat` or a Takeout dump, and you do not need to know
where inside it the files are kept.

## Where it looks for files

Every source resolves file locations through the same ladder, stopping at the
first step that works:

| Step | What it tries |
|---|---|
| 1 | The provider's own catalog. Only when you pass `--use-library-db`, or for Lightroom, where there is no alternative |
| 2 | Known folder layouts: `originals/`, `Masters/`, `Originals/`, `Google Photos/` |
| 3 | Asks you, via `--originals <dir>` |

**The default never opens a provider database.** Apple, in particular, is read
purely from the filesystem, because Apple's schema changes between macOS
releases and its folder layout does not.

Each run reports which step succeeded, so you can see how your files were found:

```
Located 46,118 file(s) via originals/ (catalog not read).
```

Apple renamed that folder twice, so all three spellings are recognised: early
iPhoto used `Originals/`, iPhoto 9 used `Masters/`, and Photos on Mojave and
later uses lowercase `originals/` with a hex fan-out.

### When it cannot find them

If a vendor changes their structure in a version newer than your videre, you are
not stuck. `--originals` overrides every step:

```bash
videre import --originals ~/somewhere/photos ~/Pictures/Odd.photoslibrary
```

## Options

| Option | Effect |
|---|---|
| `--originals <dir>` | Where the files actually are. Overrides every detection step |
| `--use-library-db` | Also read the provider's catalog to locate files. Off by default |
| `--allow-partial` | Proceed without prompting when an Apple library looks optimised |
| `--dry-run` | Report what would change, modify nothing |
| `-y`, `--yes` | Skip the confirmation prompts |
| `--silent` | No per-file output. Errors always show |
| `--json` | One JSON summary object on stdout |

:::caution[`--into` is accepted but not implemented yet]
Copying into a clean destination tree is designed but not built. Passing
`--into` exits with a message rather than silently ignoring it.
:::

## Import comes before scan

Dates must be corrected before [`videre scan`](/commands/scan/) records them, so
the order matters:

```bash
videre import ~/Takeout      # fix the dates Takeout mangled
videre scan ~/Takeout        # now record them
videre dedupe                # collapse the copies albums created
videre prune                 # tidy up
```

Import deliberately does not scan for you, keeping each command to one job.

## What each source does

### Google Takeout

Takeout puts each photo's real capture date in a `.json` sidecar rather than in
the file, so every photo's timestamp is the day you exported. Import reads those
sidecars and restores the dates.

The sidecar names are the hard part: Google truncates them at around 46
characters, so `photo.jpg.supplemental-metadata.json` can arrive as
`photo.jpg.suppl.json` or even `photo.jpg.s.json`. Import handles those, plus
`(1)` duplicate counters and `-edited` versions.

Point it at whichever level you have: the folder you extracted into, the
`Takeout/` folder, or `Google Photos/` itself all work.

It uses `photoTakenTime`, never `creationTime`. The second one is when the file
was uploaded to Google, often years after the photo was taken, and using it is
the most common way other tools get this wrong.

**When a name is ambiguous, no date is applied.** If a truncated name could
belong to two sidecars, the file is left alone and counted separately. A wrong
date is worse than a missing one.

### Apple Photos and iPhoto

:::caution[Grant Full Disk Access first]
macOS protects a `.photoslibrary`, so by default videre cannot read inside it
at all and the import reports that it could not find your files.

Open **System Settings -> Privacy & Security -> Full Disk Access**, add the
program you run videre from (Terminal, iTerm, your editor), switch it on, then
quit and reopen it. The setting only applies to a newly started program.

videre tells you when this is what happened, rather than blaming a missing
folder.
:::

Reads `originals/` (or `Masters/`, or `Originals/`) directly. Before starting it
prints a short checklist, because two things about the library's state matter
more than anything videre can detect:

1. **Download Originals to this Mac.** If "Optimise Mac Storage" is on, the
   files on disk are smaller stand-ins rather than your originals. This is the
   one worth getting right: once you delete the Apple library, whatever was only
   in iCloud is not coming back.
2. Optionally empty Recently Deleted, since deleted photos are still on disk and
   will be imported. Harmless, as you can delete them again from videre, but it
   saves a pass.

As a safety net, import warns when the library's **median file size** looks too
small to be originals. That is a judgement about the library as a whole, not
per file, because individual small files are perfectly normal.

It also detects a *referenced* library, where Photos links to files elsewhere
rather than copying them in. There the answer is to point videre at your real
folders instead.

If `originals/` turns out to be completely empty, import stops and explains
rather than reporting zero files. That state has two causes which are
indistinguishable on disk, so both are offered: iCloud has evicted your
originals, or the library is a referenced one. The first is checked in
**Photos > Settings > iCloud**, the second in **Photos > Settings > General**.
A referenced library needs no import at all; its files are ordinary photos, so
[`videre scan`](/commands/scan/) is the whole answer.

### Lightroom

Lightroom never owns your files: the catalog is a set of pointers to ordinary
folders you chose. So import reads `.lrcat` to find out **which folders**, which
is the one thing a filesystem scan cannot tell you.

The catalog is copied before reading, never opened in place, since Lightroom
holds it open.

Root folders on drives that are not connected are reported as offline and
skipped, not treated as missing files:

```
Catalog references 4 root folder(s):
  /Users/you/Pictures/2024          online
  /Volumes/Archive/Photos           OFFLINE
```

## Caveats

**Import changes file timestamps.** Like [`fix-dates`](/commands/fix-dates/), it
writes to your files, so it asks before doing so and `--dry-run` shows exactly
what it would do. Only the modification time changes; contents are never
touched.

**It reads other applications' libraries, and only reads.** Nothing is written
back to a Photos library or a Lightroom catalog.

**An ordinary folder of photos needs no import at all.** If nothing importable
is found, it says so and points you at `videre scan`, which is the right answer
for a plain folder.

## More detail

- [Leaving Google Photos](/guides/leaving-google-photos/) is the full path from
  export to a searchable library.
