---
title: videre dedupe
description: Find duplicate copies by kind, review them, and move them to the trash, safely.
---

Finds duplicates already recorded in the database. Each action is its own
subcommand: `list` them (the default, one path per line), write a `review`
page, `trash` the copies, `delete` them permanently, or `undo` the last trash
run.

```bash
videre dedupe                                # list removable exact copies (one path per line)
videre dedupe --kind resized                 # copies of a picture at another size
videre dedupe review                         # write a page to review the groups
videre dedupe trash --dry-run                # show what trash would move
videre dedupe trash                          # move the copies to the trash (asks first)
videre dedupe trash --yes                    # ...without the confirmation prompt
videre dedupe delete                         # delete the copies permanently (asks first)
videre dedupe undo --dry-run                 # show what the last trash run would put back
videre dedupe undo                           # put back the last trash run (asks first)
videre dedupe trash --kind creation          # Google Takeout: trash the creations, keep the originals
videre dedupe trash --query 'date:2015'      # only groups with a copy from 2015
videre dedupe --kind similar                 # look-alike groups (review only)
videre --library ~/Photos dedupe             # select a different library
videre dedupe --json                         # print one JSON object instead
```

## Kinds

`--kind` chooses what counts as a duplicate. It takes one or more kinds,
comma-separated or repeated (`--kind exact,resized`), and defaults to `exact`.
Each kind is a fixed rule with its own keeper:

| Kind | What | Kept | Removable |
|---|---|---|---|
| `exact` | identical image or video content | the oldest copy | yes |
| `resized` | the same picture at another pixel size | the largest copy | yes |
| `creation` | a Google Photos `-edited`, `-EFFECTS` or `-SMILE` file beside its original | the original | yes |
| `similar` | look-alike pictures | nothing chosen | no, review only |

`trash` and `delete` refuse `similar`: see those groups with
`videre dedupe review --kind similar` and delete by hand.

## Choosing groups: `--query`, `--path`, `--type`, `--ext`, `--mime`

Every action but `undo` takes a [query](/reference/query-syntax/) and the path
and media filters. A group is chosen when **any** of its files matches, and is
then taken whole: its copies are judged by the kind's keeper rule, even when
only the keeper matched. So `videre dedupe trash --query 'date:2015'` never
keeps a copy because it fell outside the query while its keeper went.

```bash
videre dedupe --query 'tag:tatil'            # groups with a file tagged tatil
videre dedupe trash --path ~/Photos/WhatsApp # groups with a copy under that folder
videre dedupe --kind resized --type image    # resized copies among images
videre dedupe --mime image/heic             # groups with a HEIC copy
```

The summary says how many groups matched (`3 of 41 group(s) match.`).

## Removing: `trash` and `delete`

**Remove with `trash` or `delete`, not a pipe.** videre holds the real path
strings, so both handle paths containing spaces correctly; piping the list
into another tool splits every path on its spaces and mishandles any that
contain one (common in Google Takeout exports). Both ask before removing
anything unless `--yes`, preview with `--dry-run`, show their progress, and
refuse an implausibly large removal unless `--force`. A copy's XMP sidecar
(`<file>.<ext>.xmp`) goes with it, and `--dry-run` lists which sidecars would.
With `--json`, either prints one object listing what was (or with `--dry-run`,
would be) removed.

:::danger
`trash` and `delete` (without `--dry-run`) remove copies immediately once you
confirm. A trash run can be put back with
[`videre dedupe undo`](#undoing-a-trash-run) until the trash is emptied;
`delete` cannot be undone. Review first with
[`videre dedupe review`](#a-page-you-can-keep) or `trash --dry-run`.
:::

## Where removed copies go

`trash` moves copies to the operating system's trash, exactly as if you had
dragged them there yourself. `delete` deletes them outright, which is faster
on a large set, and is the one to use where a volume has no trash at all.

- **macOS:** the Trash of the copy's volume, through the system's file
  manager service rather than by scripting Finder, so no Finder permission
  prompt appears and thousands of files take seconds. Finder's **Put Back**
  may not be offered for them; use [`videre dedupe undo`](#undoing-a-trash-run),
  or drag a copy out of the Trash.
- **Linux:** the freedesktop.org trash. The file moves to a trash folder on
  the same volume when one can be created, otherwise to your home trash, so
  Restore still works. If no trash is available (a read-only volume, say),
  `trash` refuses that file rather than falling back to a hard delete; use
  `delete` if deleting is what you want.

To put a whole run back, use [`videre dedupe undo`](#undoing-a-trash-run). To
recover a single copy by hand, put it back where it was and run `videre scan`
(or let `watch` pick it up).

### `trash` or `delete`

| | `trash` | `delete` |
|---|---|---|
| Undo | Yes: `videre dedupe undo`, or drag the copy out of the Trash | No: the copy is gone |
| Speed | About 400 files a second on macOS | Thousands of files a second |
| Disk space | Freed only once you empty the Trash | Freed at once |
| Needs | A trash on the copy's volume, and permission to move files into it | Permission to delete files in the folder |

Measured on a Mac with an internal SSD: `trash` moved about 400 files a
second, so 3,000 copies take several seconds, while `delete` removed 3,000
in half a second. A USB or network drive is slower for both.

**`trash` may need extra permissions.** Moving files to the trash is done on
behalf of the app you run videre in (Terminal, iTerm2, your editor), so macOS
applies that app's privacy settings. On a USB drive, an external disk, or a
protected folder such as Desktop, Documents or Downloads, macOS may ask
whether the app can access that location, or refuse until it is allowed under
**System Settings > Privacy & Security** (Files and Folders, or Full Disk
Access). If a copy cannot be moved, videre reports it, keeps it in the
library, and carries on with the rest; re-run once access is granted, or use
`delete` if you do not need the copies back. On Linux, a volume where no
trash folder can be created (read-only, or without write access at its root)
has the same effect.

`delete` needs nothing beyond the ordinary right to delete files in the
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

## Undoing a trash run

`videre dedupe undo` puts back what the most recent trash run moved to the
trash. Run it again to put back the run before that, and so on: the newest
run comes back first, one run per call. It takes no kind or query: it undoes
the run, whatever chose it.

```bash
videre dedupe undo --dry-run   # list what would come back, change nothing
videre dedupe undo             # put it back (asks first; --yes skips the prompt)
videre dedupe undo --json      # one JSON object with each file's outcome
```

**What is recorded.** Every `dedupe trash` run, and every Delete in
[`videre gallery`](/commands/gallery/), writes a small record of what it moved
to `.videre/trash/` in the library, a line per file as each one goes. So a run
you stopped halfway can be undone too, exactly as far as it got. `undo` puts
back whichever run was most recent, from either. `delete` records nothing:
there is nothing to put back.

**How a file is found.** Each record holds where the file landed in the trash,
which is not always its own name: a trash that already holds a file of that
name gives the newcomer another one. A file is put back only if its content
still matches what was trashed, so a different file of the same name is never
taken. No extra permission is needed, Full Disk Access included.

**It never overwrites.** If something already sits at a file's original path,
that file is skipped and reported, and the run stays undoable: move the other
file away and run `undo` again.

**Back in the library.** Restored files are scanned back in before `undo`
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
- `delete` runs cannot be undone.
- Runs made with videre 0.52 or older have no record. Those went through
  Finder, so Finder's **Put Back** works for them.

## The safe way to do it

Review first. That is what [`videre dedupe review`](#a-page-you-can-keep) and
the gallery's Duplicates page are for, and it takes one extra command:

```bash
videre --library ~/Photos scan        # 1. record what you have
videre dedupe review                  # 2. review the groups visually
videre dedupe trash                   # 3. move the copies to the trash, once you agree
```

Step 2 writes a page showing every duplicate group with thumbnails, KEEP and
REMOVE badges, sizes, dates and paths. It is the same grouping and the same
KEEP choice `dedupe` will act on, so what you see is what will happen.

If you would rather read the list first, `dedupe` prints the removable paths,
then `trash` or `delete` removes exactly those:

```bash
videre dedupe > /tmp/remove.txt              # inspect it
wc -l /tmp/remove.txt
videre dedupe trash                          # to the trash; undo puts it back
videre dedupe delete                         # or permanently, faster, no undo
```

For a script that has to read the list itself, `--print0` writes the paths
NUL-delimited so a path with a space survives (`videre dedupe --print0 |
xargs -0 ...`); a plain newline-delimited pipe splits on spaces and is unsafe.

## Exact copies

Identical image or video content. Each file is hashed with BLAKE3 with its
metadata left out (EXIF, XMP, comments, a video's creation date), and files
are grouped by that hash. So a copy whose date was fixed, or that was rotated
in the gallery, is still a duplicate of the original.

Filenames, folders and timestamps are irrelevant to the grouping. `IMG_0042.jpg`
and `holiday-best.jpg` are one group if their contents match.

### Which copy is kept

Within each group, files are sorted oldest first and **the first is kept**,
everything after it is listed for removal.

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
the KEEP copy's metadata survives `trash`, and because the oldest date wins,
a copy whose date you moved later is the one removed.

Review in `videre dedupe review` before removing: check the dates, and check
**which path** remains, since the KEEP copy may be in a folder you would not have
chosen, especially across [multiple scanned folders](/guides/multiple-libraries/).

If two copies have identical dates, the choice between them is arbitrary.
:::

## Resized copies

The same picture saved at another pixel size: a messenger copy (WhatsApp,
Viber), an export for the web, a phone's reduced copy. The bytes differ, so
they are not exact duplicates. Two files are one picture when all of these
hold:

- their pixel sizes differ;
- their shapes (width over height, upright) are within 1%;
- their fingerprints (as for `similar`, below) are at most 4 bits apart;
- reduced to 64x64 greyscale, their pixels differ by at most 0.5 on average,
  on a scale of 0 to 255.

The **largest is kept**: most pixels, then the larger file, then the older
date. Each copy is compared with the keeper itself, so a chain of near
pictures never joins two different ones.

Two files of the **same size** are never resized copies, however close they
look. At one size a burst of shots taken a moment apart is as close as a
re-saved copy, and which frame to keep is a choice, not a rule.

The last test needs the pixels, so the first run decodes each candidate (about
1% of a library) and keeps the result in the library, so later runs, the
review page and the gallery are instant. A photo rotated in the gallery is
checked again. Fingerprints come from [`videre embed`](/commands/embed/), so
run it first.

Measured on a 14,000-image Google Takeout library: every copy the owner
confirmed was within 4 bits and at 0.56 or less, and every other
different-size pair at 0.59 or more; 0.5 leaves the closest non-copies a
margin.

## Google Photos creations

Google Takeout exports what Google Photos made from a photo beside the photo
itself, in the same folder: `IMG_1-edited.jpg` (your edit), `IMG_1-EFFECTS.jpg`
(a filter or crop it suggested) and `IMG_1-SMILE.jpg` (a face pasted smiling).
Their bytes and pixels differ from `IMG_1.jpg`, often by far, so no other kind
pairs them. In one real export a fifth of all files were such creations.

`--kind creation` pairs them by name: `<name>-edited.<ext>`, `<name>-EFFECTS.<ext>`
or `<name>-SMILE.<ext>` (or with Google's counter, `<name>-edited(1).<ext>`)
whose original `<name>.<ext>` is in the same folder. The **original is kept**
and the creations are listed for removal, since the original is the file the
camera wrote.

```bash
videre dedupe review --kind creation         # review them side by side
videre dedupe trash --kind creation --dry-run
videre dedupe trash --kind creation          # trash them (asks first)
```

They do not count toward the implausibly-large-deletion check, so no `--force`
is needed: a pair exists only when both files are in the library, so a large
number of them is what a Takeout export looks like, not a sign of a mistake.
Faces named on a creation are not moved to its original; name them again
there if you need to. The query term `is:creation` finds them anywhere.

The pairs come from the last scan, and so do `--json`, `review` and the
gallery's Duplicates page. Removal checks the disk again: a creation whose
original has gone since that scan is kept, since it is now the only copy.

## Look-alike pictures

Review only. `list` prints nothing for them, so piping into a delete command
can never act on a mere resemblance; they appear in the summary on stderr, in
`--json` as flat `files` groups, and in `videre dedupe review --kind similar`.

This needs a prior [`videre embed`](/commands/embed/), which computes the
fingerprints alongside the embeddings.

### What counts as alike

Each image is reduced to a 64-bit fingerprint: converted to greyscale, scaled to
9x8, and each pixel compared with its right-hand neighbour to give one bit per
comparison. Two images are treated as similar when at most 10 of those 64 bits
differ, and overlapping pairs are then joined into groups.

In practice that catches resizes, re-compressions, crops that keep the overall
composition, light edits, and often burst shots taken a moment apart.

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
trash` and `delete` run the prune pass for you, so their removals are clean in
one step. Either way, deleting one copy of a photo you still have elsewhere
frees nothing derived, because that work is keyed by content and still in use.

**Empty output means no duplicates of the chosen kinds**, not an error. Try
`--kind resized` or `--kind similar` to see whether you have other kinds.

## Output streams

| Stream | Contents |
|---|---|
| stdout | REMOVE candidate paths, one per line |
| stderr | Progress and a summary per kind, suppressed by `--silent` |

With `--json`, stdout is instead a single JSON object, always, including an
error object plus a nonzero exit code on failure. That makes it safe to script
against without parsing the human-readable summary.

`list --json` is the document the [MCP](/commands/mcp/) `find_duplicates` tool
returns, `schema_version` 2:

```json
{
  "schema_version": 2,
  "total_files": 14471,
  "kinds": ["exact", "resized"],
  "unchecked": 0,
  "groups": [
    { "kind": "resized", "keep": { "path": "...", ... }, "remove": [{ "path": "...", ... }] },
    { "kind": "similar", "files": [{ "path": "...", ... }, ...] }
  ]
}
```

A removable kind has `keep` and `remove`; `similar` has `files`. `unchecked`
counts resized candidates not compared yet, which only the MCP tool leaves
(it never decodes).

## A page you can keep

`review` writes the chosen groups to a browsable file, with thumbnails and the
group structure, so you can review them away from the terminal or keep the
list after the run.

```bash
videre dedupe review                         # writes <db>_duplicates.html
videre dedupe review ~/dupes.html            # somewhere specific
videre dedupe review --kind exact,creation   # the kinds to show
```

For the live view, with buttons to act, use the Duplicates page in
[`videre gallery`](/commands/gallery/).

## More detail

- [Backing up](/guides/backup/) covers what to keep before deleting in bulk.
