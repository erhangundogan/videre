---
title: videre faces
description: Detect faces and group them, so you name a person once instead of tagging each photo.
---

Detects faces, then groups them so you can name a person once instead of tagging
each photo.

```bash
videre faces                           # detect, group, and store (resumable)
videre faces --limit 500               # only process 500 new images, then stop
videre faces --recluster               # regroup existing faces without re-detecting
videre faces --reprocess               # start over: re-detect everything
videre faces --dry-run                 # detect but write nothing
videre faces --profile                 # print per-stage timing when finished
videre faces --silent                  # no per-image progress
videre faces --db ~/photos.db          # use a specific database
videre faces --ext heic                # only HEIC photos
videre faces --date 2024-07            # only that month
```

:::tip
These filters work the same way across commands, and combine. See
[scoping a run](/guides/scoping-a-run/).
:::

The first run downloads about 180 MB, separate from the search model.

## The whole workflow

Detection and naming are separate steps. The first is slow and automatic, the
second is fast and manual.

```bash
videre faces                  # 1. find faces and group them (slow, resumable)
videre gallery         # 2. name the groups in your browser
videre search --person "Alice"
```

Step 2 opens `localhost:7878`; naming happens on its **People** tab
(`/people`), which has three sections: **People** you have named, **Unassigned
Clusters** (groups it is confident about but has no name for), and **Singletons**
(faces it could not group). Drag a cluster onto a person to assign it, or create
a new person from it.

Clicking a cluster or a person opens its own page, at `/people/cluster/<id>` and
`/people/person/<name>`.

The payoff is the ratio: one drag can name forty photos.

### Working through a large library

Detection on tens of thousands of photos takes hours. `--limit` lets you do it
in sittings:

```bash
videre faces --limit 2000     # a chunk, then stop
videre faces --limit 2000     # continue where it left off
videre faces --recluster      # once, after the last chunk
```

A limited run **skips the grouping step**, because grouping is a whole-library
pass that is not worth repeating after every chunk. Run `--recluster` once at
the end, or grouping never happens at all.

Plain `videre faces` with no `--limit` does group at the end, so if you are
happy to let it run to completion you never need `--recluster`.

## Fixing bad grouping

Grouping is the part that needs judgement, and `--recluster` makes experimenting
cheap: it reuses existing detections, so it takes minutes rather than hours.

```bash
videre faces --recluster --eps 0.55            # stricter: fewer, tighter groups
videre faces --recluster --merge-sim 0.30      # more willing to merge groups
videre faces --recluster --min-cluster-size 2  # allow smaller groups
```

| Symptom | Try |
|---|---|
| One person split across several groups | Lower `--merge-sim` toward 0.30 |
| Two people merged into one group | Raise `--merge-sim`, or lower `--eps` |
| Too many singletons, few groups | Lower `--min-cluster-size` to 2 |
| A group full of unrelated faces | Raise `--min-face-size`, lower `--max-generic-sim` |

:::note[Two people should not merge in the first place]
Merging two groups now requires more than similar averages: they must also be
close *overall*, not merely pointing the same way. Two tight groups that sit far
apart are refused however similar their averages look.

This was added after two different people ended up in one group on a real
library, at an average similarity of 0.370 against a threshold of 0.35. If you
hit that on an older version, `--merge-sim` is still the knob - raise it - but
you should need it far less often now.
:::

Change one value at a time and look at the result. These interact, and a
combination that fixes one library often overfits it.

For a single wrongly-merged group, the labeling UI's **Dissolve cluster** button
is better than retuning: it breaks that group back into singletons without
touching anything else. Faces are not deleted.

### All tuning options

```bash
videre faces --eps 0.6                 # how alike faces must be to group (default 0.6)
videre faces --min-cluster-size 3      # fewest faces that can form a group (default 3)
videre faces --merge-sim 0.35          # how readily two groups merge (default 0.35)
videre faces --min-face-size 80        # ignore faces smaller than this in pixels (default 80)
videre faces --min-blur 100            # ignore faces too soft to read (default 100)
videre faces --max-landmark-error 7    # ignore faces the detector mislocated (default 7)
videre faces --max-generic-sim 0.4     # legacy fallback, see below (default 0.4)
videre faces --attach-sim 1            # second pass, off by default (default 1)
videre faces --batch 8                 # images per batch (default 8)
videre faces --workers 8               # parallel workers (default: 2x your CPU cores)
videre faces --qlmanage-concurrency 6  # simultaneous HEIC conversions (default 6)
```

## Why some faces are ignored

Before grouping, low-quality faces are held out. They come back as unassigned
singletons, still visible and still nameable by hand, just not grouped.

This exists because low-quality faces produce near-identical featureless
fingerprints regardless of who they are, so grouping them piles unrelated people
into one large mixed cluster. A face is held out if it fails any check:

- **Size** (`--min-face-size`, default 80px). Tiny crops, such as distant faces
  in group shots, are mostly blur once scaled up.
- **Sharpness** (`--min-blur`, default 100). The variance of the Laplacian of the
  aligned crop, which is the image the model is actually given. Measured on a
  1,063-face library: faces that grouped had a median of 894, faces left ungrouped
  182.
- **Alignment** (`--max-landmark-error`, default 7). The model never sees your
  photo, only a small square built by warping it so the detected eyes, nose and
  mouth land on fixed positions. When the detector mislocates those points the
  square is a mangled image, and the fingerprint describes the mangling rather
  than the person, so such faces resemble *each other*. This measures how far the
  five points are from being a face shape at all.

Each is disabled by setting it to 0, except `--max-landmark-error`, which is
disabled by setting it high.

:warning: **`--max-generic-sim` (default 0.4) is a fallback, and only applies to
faces recorded before sharpness was measured.** It compares a face against the
average of every face in the library, which sounds reasonable and is not: on a
personal library that average largely *is* the most photographed person, so the
check discards the very faces most worth grouping. Measured on a labelled
library, it blocked 15 photos of the owner, several of which matched a face
already in their group almost exactly. Re-run `videre faces --reprocess` to
record sharpness and leave it behind.

### Recovering more of one person

`--attach-sim` (default 1, off) runs a second pass after grouping: an ungrouped
face joins the group holding its **nearest** face, if that face is at least this
similar. Grouping normally asks a face to resemble the *average* of a whole
group, which someone photographed over many years can fail even when three
members match them almost exactly. `--attach-sim 0.5` recovers some of those.

It runs after grouping and can never merge two groups, which is what keeps it
safe.

## Why grouping runs in two stages

Average-linkage grouping alone fragments one person into several groups, because
one person's photos legitimately spread wide (pose, lighting, age). A second
pass then merges any two established groups whose averaged fingerprints are at
least `--merge-sim` alike.

Deciding on averages rather than individual faces is what makes this safe: on
real data, confirmed *different* people never exceeded about 0.29, while one
person's fragments ran 0.37 to 0.76. Only established groups take part, never
lone singletons: a single bad crop can resemble a different person, whereas a
whole group's average cannot.

## Caveats

:::caution[Do not run two heavy commands at once]
`faces`, [`embed`](/commands/embed/), and [`watch`](/commands/watch/) all convert
HEIC through the same macOS service, and each limits itself independently, so
together they overwhelm it.

Measured with `faces` and `embed` running together: a HEIC load averaged over 16
seconds against about 7.6 uncontended, and one file exceeded the timeout that
converted in 0.39 s on its own. Nothing is lost, since a skipped file is not
marked as done and retries next run, but it is much slower than running them one
after the other.
:::

**Warm the HEIC cache first** if your library is HEIC-heavy. Detection reads
already-decoded images when they exist, about 108 ms against 7.6 s:

```bash
videre watch ~/Photos --heic     # then Ctrl-C once it settles
videre faces
```

See the [thumbnail cache](/reference/paths/#thumbnail-cache) for what that
stores and how much space it takes.

**Names live on faces, not on groups.** Group numbers are reassigned on every
`--recluster`, but the names you assigned are kept, because they are stored per
face. Retuning does not lose your work.

**An interrupt can cost more than one image.** Each worker batches, so a Ctrl-C
can lose up to `workers x batch` images of progress, which is 160 with the
defaults on a 10-core machine. Everything already committed is safe and the
rerun continues correctly; it just redoes a little.

**Detection is not perfect.** Faces in profile, heavily shadowed, or very small
are often missed entirely, and no amount of retuning brings them back, since
tuning only affects grouping of faces that were already found. `--reprocess`
re-detects from scratch, which only helps after a videre upgrade changes
detection itself.

**Nothing is uploaded.** Detection, grouping, and the naming UI all run locally.
The server on `localhost:7878` is reachable only from your machine.

## Performance

| Flag | Does |
|---|---|
| `--workers` | detection threads (default: twice your core count) |
| `--profile` | print per-stage timings: load, detect, align, embed, db write |

`--profile` is the quickest way to see whether a slow run is bound by decoding
or by inference. Why the default oversubscribes your cores, and the measured
4.48x it is worth, are in [tuning](/guides/tuning/#face-detection).

## Scoping the run

Every flag below narrows an existing set, never widens it, and they combine:
each condition must hold.

| Flag | Selects |
|---|---|
| `--type` | `image` or `video`. Repeatable, or comma-separated |
| `--ext` | file extension, e.g. `mov`. Repeatable, or comma-separated |
| `--mime` | exact type, e.g. `video/quicktime`. Repeatable, or comma-separated |
| `--after` | date on or after this (inclusive) |
| `--before` | date before this (exclusive) |
| `--date` | a whole year, month or day: `YYYY`, `YYYY-MM`, `YYYY-MM-DD` |
| `--location` | within `--radius` km of a place, e.g. `"Berlin, Germany"` |
| `--radius` | radius in km for `--location` (default 20) |
| `--path` | only files under this directory. Repeatable |
| `--has` | only files with this metadata. Supported fields: `gps`, `date` |
| `--missing` | only files missing this metadata. Supported fields: `gps`, `date` |

`--person` and `--category` are deliberately absent: both are derived from data
this command produces, so selecting its input by one would be circular.

A scoped run prints `N of M`, so a filter that matches nothing is
distinguishable from an empty library. Full detail, including how missing data
excludes a file, is in [scoping a run](/guides/scoping-a-run/).

## Importing names from XMP

If a photo has an `.xmp` sidecar (or embedded XMP) with named face regions, for
example one digiKam or Lightroom wrote, `videre faces` reads those regions and
assigns the names to the faces it detects, matching each region to a face by
where it sits in the frame. This is the read side of
[`videre export --xmp`](/commands/export/): a name you gave a face in another
tool imports into videre.

Names are imported while faces are being detected, so the region has a detected
face to attach to. To import into a library whose faces were detected earlier,
re-run with `--reprocess`.

The `--xmp` flag decides who wins when both carry a name:

| `--xmp` | Effect |
|---|---|
| `db` (default) | The database wins: imported names fill only faces you have not already named |
| `file` | The sidecar wins: an imported name replaces an existing one |
| `newest` | Reserved; currently behaves as `db` |

The default comes from the `xmp_precedence` config key (see
[`videre config`](/commands/config/)), the same setting `scan` uses for ratings.

## More detail

- [Long-running jobs](/guides/long-running-jobs/) covers running this alongside other commands, and resuming.
- [Caches and disk use](/guides/caches/) covers the decode cache this reads from.
- [Backing up](/guides/backup/) covers why the names you assign are the one thing that cannot be recomputed.
