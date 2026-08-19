---
title: videre dedupe
description: List duplicate copies you could delete, one path per line.
---

Finds duplicates already recorded in the database. Prints one path per line to
stdout, and nothing else, so it pipes cleanly.

```bash
videre dedupe                          # list removable copies (one path per line)
videre dedupe | xargs trash            # ...and delete them
videre dedupe --similar                # also report look-alike groups (review only)
videre dedupe --db ~/photos.db         # use a specific database
videre dedupe --silent                 # suppress the summary; paths still print
videre dedupe --json                   # print one JSON object instead
```

:::danger
This prints the REMOVE side of each duplicate group, so
`videre dedupe | xargs trash` deletes those files immediately. Look before you
pipe.
:::

## The safe way to do it

Review in a browser first. That is what [`videre dedupe --html`](/commands/dedupe/) is
for, and it takes one extra command:

```bash
videre scan ~/Photos           # 1. record what you have
videre dedupe --html                  # 2. review the groups visually
videre dedupe | xargs trash    # 3. delete, once you agree
videre prune                   # 4. tidy the database afterwards
```

Step 2 opens a page showing every duplicate group with thumbnails, KEEP and
REMOVE badges, sizes, dates and paths, sorted by how much space each group
wastes. It is the same grouping and the same KEEP choice `dedupe` will print, so
what you see is what will happen.

If you would rather read the list than look at it:

```bash
videre dedupe > /tmp/remove.txt        # inspect it
wc -l /tmp/remove.txt
xargs trash < /tmp/remove.txt          # then act
```

`trash` moves to the system trash, which is recoverable. `rm` is not. On a
library you care about, prefer `trash` (macOS: `brew install trash`).

Step 4 matters more than it looks: until you prune, the database still lists the
deleted files, and their embeddings and cached thumbnails are still on disk. See
[`videre prune`](/commands/prune/).

## What counts as a duplicate

Exact, byte-for-byte identical content. Files are hashed with BLAKE3 and grouped
by that hash, so two files are duplicates only if their bytes match completely.

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

:::note[It does not matter which copy survives]
Members of a group are byte-identical, so whichever is kept, the file you end up
with is the same file. The only thing that differs is **which path** remains,
which is why reviewing in `videre dedupe --html` is worth it: the KEEP copy may be in a
folder you would not have chosen, especially across
[multiple scanned folders](/reference/paths/#scanning-more-than-one-folder).

If two copies have identical dates, the choice between them is arbitrary. Again,
the bytes are the same.
:::

## `--similar` is review-only

Look-alike groups are deliberately kept out of stdout, so piping into a delete
command can never act on a mere resemblance. They appear in the summary on
stderr and in [`videre dedupe --html`](/commands/dedupe/), where you can look at them.

This needs a prior [`videre scan --similar`](/commands/scan/) to have computed
the fingerprints.

### What counts as alike

Each image is reduced to a 64-bit fingerprint: converted to greyscale, scaled to
9x8, and each pixel compared with its right-hand neighbour to give one bit per
comparison. Two images are treated as similar when at most 10 of those 64 bits
differ, and overlapping pairs are then joined into groups.

In practice that catches resizes, re-compressions, crops that keep the overall
composition, and light edits. It will not catch a photo of the same scene taken
a moment later, since those differ far more than 10 bits.

HEIC files never get a fingerprint. For videos it is computed from a single
poster frame, so it finds re-encodes that keep the opening frame but not a trim
that cuts it.

There is no automatic way to act on these, by design. Review them in the report
and delete by hand.

## Caveats

**It reads the database, not your disk.** Results reflect your last
[`videre scan`](/commands/scan/). Files deleted since then still appear, and
files added since then are missing. Re-scan first if in doubt.

**It spans every folder in the database.** If you scanned several roots into one
database, a group can contain copies from different drives, and the KEEP copy
may be on the one you consider the backup. See
[scanning more than one folder](/reference/paths/#scanning-more-than-one-folder).

**Deleting duplicates does not free everything.** Embeddings and cached
thumbnails for those photos remain until [`videre prune`](/commands/prune/)
removes them. Conversely, deleting one copy of a photo you still have elsewhere
frees nothing derived, because that work is keyed by content and still in use.

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
