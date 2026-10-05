---
title: videre dedupe
description: Find duplicate copies and move them to the trash, safely.
---

Finds duplicates already recorded in the database. It can list them (one path
per line), or remove the copies itself: `--trash` moves them to the system
trash, `--delete` deletes them permanently.

```bash
videre dedupe                          # list removable copies (one path per line)
videre dedupe --trash --dry-run        # show what --trash would move
videre dedupe --trash                  # move the copies to the trash (asks first)
videre dedupe --trash --yes            # ...without the confirmation prompt
videre dedupe --delete                 # delete the copies permanently (asks first)
videre dedupe --undo --dry-run         # show what the last --trash run would put back
videre dedupe --undo                   # put back the last --trash run (asks first)
videre dedupe --undo                   # again: the run before that
videre dedupe --similar                # also report look-alike groups (review only)
videre dedupe --edited --trash         # Google Takeout: trash the -edited copies, keep the originals
videre --library ~/Photos dedupe       # select a different library
videre dedupe --json                   # print one JSON object instead
```

**Prefer `--trash` over piping.** videre holds the real path strings, so
`--trash` handles paths containing spaces correctly; a shell pipe like
`videre dedupe | xargs trash` splits every path on its spaces and mishandles
any that contain one (common in Google Takeout exports). Both `--trash` and
`--delete` ask before removing anything unless `--yes`, preview with
`--dry-run`, show their progress, and refuse an implausibly large removal
unless `--force`. They remove only **exact** duplicates; `--similar` groups are
review-only. A copy's XMP sidecar (`<file>.<ext>.xmp`) goes with it, and
`--dry-run` lists which sidecars would.

:::danger
`--trash` and `--delete` (without `--dry-run`) remove copies immediately once
you confirm. A `--trash` run can be put back with
[`videre dedupe --undo`](#undoing---trash) until the trash is emptied;
`--delete` cannot be undone. Review first with
[`videre dedupe --html`](/commands/dedupe/) or `--trash --dry-run`.
:::

## Where removed copies go

`--trash` moves copies to the operating system's trash, exactly as if you had
dragged them there yourself. `--delete` deletes them outright, which is faster
on a large set, and is the one to use where a volume has no trash at all.

- **macOS:** the Trash of the copy's volume, through the system's file
  manager service rather than by scripting Finder, so no Finder permission
  prompt appears and thousands of files take seconds. Finder's **Put Back**
  may not be offered for them; use [`videre dedupe --undo`](#undoing---trash),
  or drag a copy out of the Trash.
- **Linux:** the freedesktop.org trash. The file moves to a trash folder on
  the same volume when one can be created, otherwise to your home trash, so
  Restore still works. If no trash is available (a read-only volume, say),
  `--trash` refuses that file rather than falling back to a hard delete; use
  `--delete` if deleting is what you want.

To put a whole run back, use [`videre dedupe --undo`](#undoing---trash). To
recover a single copy by hand, put it back where it was and run `videre scan`
(or let `watch` pick it up).

### `--trash` or `--delete`

| | `--trash` | `--delete` |
|---|---|---|
| Undo | Yes: `videre dedupe --undo`, or drag the copy out of the Trash | No: the copy is gone |
| Speed | About 400 files a second on macOS | Thousands of files a second |
| Disk space | Freed only once you empty the Trash | Freed at once |
| Needs | A trash on the copy's volume, and permission to move files into it | Permission to delete files in the folder |

Measured on a Mac with an internal SSD: `--trash` moved about 400 files a
second, so 3,000 copies take several seconds, while `--delete` removed 3,000
in half a second. A USB or network drive is slower for both.

**`--trash` may need extra permissions.** Moving files to the trash is done on
behalf of the app you run videre in (Terminal, iTerm2, your editor), so macOS
applies that app's privacy settings. On a USB drive, an external disk, or a
protected folder such as Desktop, Documents or Downloads, macOS may ask
whether the app can access that location, or refuse until it is allowed under
**System Settings > Privacy & Security** (Files and Folders, or Full Disk
Access). If a copy cannot be moved, videre reports it, keeps it in the
library, and carries on with the rest; re-run once access is granted, or use
`--delete` if you do not need the copies back. On Linux, a volume where no
trash folder can be created (read-only, or without write access at its root)
has the same effect.

`--delete` needs nothing beyond the ordinary right to delete files in the
folder, which is why it also works where there is no trash at all.

### The database is cleaned up too

Each copy's row is dropped from the library the moment the copy leaves, so a
run you stop halfway leaves the library listing exactly what is still on disk.
A copy that is already gone, from an earlier run that was stopped say, is
counted as `already gone`, not as a failure, and its row is dropped too.

After the removal, videre runs the same pass
[`videre prune`](/commands/prune/) would: the removed copies' rows, and any
embeddings or cached thumbnails only they were using, are dropped at once, so
the gallery never shows a ghost of a deleted copy. The pass appears as its own
`prune` entry in [`videre status`](/commands/status/); if it cannot start
(a `watch` cycle holding the library, say), videre says so and you can run
`videre prune` by hand.

## Undoing `--trash`

`videre dedupe --undo` puts back what the most recent `--trash` run moved to
the trash. Run it again to put back the run before that, and so on: the newest
run comes back first, one run per call.

```bash
videre dedupe --undo --dry-run   # list what would come back, change nothing
videre dedupe --undo             # put it back (asks first; --yes skips the prompt)
videre dedupe --undo --json      # one JSON object with each file's outcome
```

**What is recorded.** Every `--trash` run, and every Delete in
[`videre gallery`](/commands/gallery/), writes a small record of what it moved
to `.videre/trash/` in the library, a line per file as each one goes. So a run
you stopped halfway can be undone too, exactly as far as it got. `--undo` puts
back whichever run was most recent, from either. `--delete` records nothing:
there is nothing to put back.

**How a file is found.** Each record holds where the file landed in the trash,
which is not always its own name: a trash that already holds a file of that
name gives the newcomer another one. A file is put back only if its content
still matches what was trashed, so a different file of the same name is never
taken. No extra permission is needed, Full Disk Access included.

**It never overwrites.** If something already sits at a file's original path,
that file is skipped and reported, and the run stays undoable: move the other
file away and run `--undo` again.

**Back in the library.** Restored files are scanned back in before `--undo`
returns. Their embeddings and faces, dropped by the cleanup after the
removal, are rebuilt by the next [`embed`](/commands/embed/),
[`faces`](/commands/faces/) or [`pipeline`](/commands/pipeline/), or by a
running [`watch`](/commands/watch/); [`videre status`](/commands/status/) lists
them as outstanding until then.

```
Restored 412 file(s) from the run of 2026-10-05 10:04 UTC (3 not in the trash). 1 earlier run(s) can still be undone.
```

**Limits.**

- A file emptied from the trash cannot come back. It is reported as
  `not in the trash`, and the run is dropped from the record.
- `--delete` runs cannot be undone.
- Runs made with videre 0.52 or older have no record. Those went through
  Finder, so Finder's **Put Back** works for them.

## The safe way to do it

Review in a browser first. That is what [`videre dedupe --html`](/commands/dedupe/) is
for, and it takes one extra command:

```bash
videre --library ~/Photos scan           # 1. record what you have
videre dedupe --html                  # 2. review the groups visually
videre dedupe --trash                 # 3. move the copies to the trash, once you agree
videre prune                          # 4. tidy the database afterwards
```

Step 2 opens a page showing every duplicate group with thumbnails, KEEP and
REMOVE badges, sizes, dates and paths, sorted by how much space each group
wastes. It is the same grouping and the same KEEP choice `dedupe` will print, so
what you see is what will happen.

If you would rather read the list, or drive your own tool, `dedupe` still prints
the removable paths. Use `--print0` so a path with a space survives the pipe:

```bash
videre dedupe > /tmp/remove.txt              # inspect it
wc -l /tmp/remove.txt
videre dedupe --print0 | xargs -0 trash      # or pipe it, space-safe
```

`--print0` writes the paths NUL-delimited; a plain `videre dedupe | xargs trash`
splits on spaces and is unsafe. For anything but scripting, `--trash` is
simpler and safer.

Step 4 matters more than it looks: until you prune, the database still lists the
deleted files, and their embeddings and cached thumbnails are still on disk. See
[`videre prune`](/commands/prune/).

## What counts as a duplicate

Exact, identical image or video content. Each file is hashed with BLAKE3 with
its metadata left out (EXIF, XMP, comments, a video's creation date), and files
are grouped by that hash. So a copy whose date was fixed, or that was rotated
in the gallery, is still a duplicate of the original, and
[`--trash`](#which-copy-is-kept) keeps the oldest-dated copy.

A re-saved, re-compressed, resized or cropped copy is **not** a duplicate here,
however similar it looks. That is what `--similar` is for.

Filenames, folders and timestamps are irrelevant to the grouping. `IMG_0042.jpg`
and `holiday-best.jpg` are one group if their contents match.

## Which copy is kept

Within each group, files are sorted oldest first and **the first is kept**,
everything after it is printed for removal.

The sort key is:

1. `exif_date`, the date the camera recorded, if present
2. otherwise the earlier of the file's created and modified timestamps
3. otherwise nothing, which sorts first

EXIF dates of `0000-00-00T00:00:00`, produced by cameras with an unset clock,
are treated as absent and fall through to step 2.

The intent is to keep the copy closest to the original: an edited or re-saved
copy usually has a later filesystem date, while the EXIF date survives copying.

:::caution[The kept copy decides which metadata survives]
Members of a group have the same pixels or media data, but their metadata can
differ: one copy may carry a date or location you corrected in another app. Only
the KEEP copy's metadata survives `--trash`, and because the oldest date wins,
a copy whose date you moved later is the one removed.

Review in `videre dedupe --html` before removing: check the dates, and check
**which path** remains, since the KEEP copy may be in a folder you would not have
chosen, especially across [multiple scanned folders](/guides/multiple-libraries/).

If two copies have identical dates, the choice between them is arbitrary.
:::

## Google Photos edits: `--edited`

Google Takeout exports every photo you edited in Google Photos twice: the
original, `IMG_1.jpg`, and Google's render of the edit, `IMG_1-edited.jpg`,
side by side in the same folder. The bytes differ, so they are not exact
duplicates, and a crop or filter can put them past `--similar` too. In one real
export a fifth of all files were such edits.

`--edited` pairs them by name: a file named `<name>-edited.<ext>` (or
`<name>-edited(1).<ext>`, with Google's counter) whose original
`<name>.<ext>` is in the same folder. The **original is kept** and the edit is
listed for removal, since the original is the file the camera wrote and the
edit is Google's re-encoded copy of it.

```bash
videre dedupe --edited --html             # review the pairs side by side
videre dedupe --edited --trash --dry-run  # list the edits that would go
videre dedupe --edited --trash            # trash them (asks first)
```

With `--trash` or `--delete`, the edits go the same way exact duplicates do,
after a confirmation, followed by the database clean-up. They do not count
toward the implausibly-large-deletion check, so no `--force` is needed: a pair
exists only when both files are in the library, so a large number of them is
what a Takeout export looks like, not a sign of a mistake. Faces named on an
edit are not moved to its original; name them again there if you need to.

`--json --edited` adds `edited_pairs`, each `{"kept": ..., "removed": ...}`.

The pairs come from the last scan, and so do `--json`, `--html` and the
gallery's Duplicates page. Removal checks the disk again: an edit whose
original has gone since that scan is kept, since it is now the only copy.

## `--similar` is review-only

Look-alike groups are deliberately kept out of stdout, so piping into a delete
command can never act on a mere resemblance. They appear in the summary on
stderr and in [`videre dedupe --html`](/commands/dedupe/), where you can look at them.

This needs a prior [`videre embed`](/commands/embed/), which computes the
fingerprints alongside the embeddings.

### What counts as alike

Each image is reduced to a 64-bit fingerprint: converted to greyscale, scaled to
9x8, and each pixel compared with its right-hand neighbour to give one bit per
comparison. Two images are treated as similar when at most 10 of those 64 bits
differ, and overlapping pairs are then joined into groups.

In practice that catches resizes, re-compressions, crops that keep the overall
composition, and light edits. It will not catch a photo of the same scene taken
a moment later, since those differ far more than 10 bits.

Fingerprints with almost no set bits (or almost nothing but set bits) are
excluded from grouping entirely. A flat or near-flat frame (a fade-in, a
letterboxed opener, a solid title card) produces exactly such a fingerprint,
and any two of them collide no matter what the rest of the clip shows. Exact
copies of those files are still caught by exact-duplicate detection, which
compares content, not the fingerprint. The exclusion also reaches genuinely
related files whose shared frame is very low-detail, such as two re-encodes
of a mostly blank page; grouping is review-only and errs toward precision.

HEIC files never get a fingerprint. For videos it is computed from a single
poster frame, so it finds re-encodes that keep the opening frame but not a trim
that cuts it. File size plays no part in the comparison: a genuine re-encode
routinely halves the file, so a size window would drop the very matches the
feature exists to find.

There is no automatic way to act on these, by design. Review them in the report
and delete by hand.

## Caveats

**It reads the database, not your disk.** Results reflect your last
[`videre scan`](/commands/scan/). Files deleted since then still appear, and
files added since then are missing. Re-scan first if in doubt.

**It spans every folder in the database.** If you scanned several roots into one
database, a group can contain copies from different drives, and the KEEP copy
may be on the one you consider the backup. See
[scanning more than one folder](/guides/multiple-libraries/).

**Deleting duplicates by hand does not free everything.** If you delete copies
yourself in Finder or a file manager, their embeddings and cached thumbnails
remain until [`videre prune`](/commands/prune/) removes them. `videre dedupe
--trash` and `--delete` run the prune pass for you, so its removals are clean in one step.
Either way, deleting one copy of a photo you still have elsewhere frees nothing
derived, because that work is keyed by content and still in use.

**Output order is by content hash**, which is effectively arbitrary. Use
`videre dedupe --html` if you want groups ordered by wasted space.

**Empty output means no exact duplicates**, not an error. Try `--similar` to see
whether you have near-duplicates instead.

## Output streams

| Stream | Contents |
|---|---|
| stdout | REMOVE candidate paths, one per line |
| stderr | Progress and summary, suppressed by `--silent` |

With `--json`, stdout is instead a single JSON object, always, including an
error object plus a nonzero exit code on failure. That makes it safe to script
against without parsing the human-readable summary.

## More detail

- [Backing up](/guides/backup/) covers what to keep before deleting in bulk.

## A page you can keep

`--html` writes the same duplicate groups to a browsable file, with thumbnails
and the group structure, so you can review them away from the terminal or keep
the list after the run.

```bash
videre dedupe --html                    # writes <db>_duplicates.html
videre dedupe --html ~/dupes.html       # somewhere specific
```

The paths still go to stdout, so piping is unaffected.

For browsing the whole library rather than one result set, use
[`videre gallery`](/commands/gallery/), which serves it live instead of writing
a file.
