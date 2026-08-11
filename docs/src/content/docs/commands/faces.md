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
```

The first run downloads about 180 MB, separate from the search model.

## The whole workflow

Detection and naming are separate steps. The first is slow and automatic, the
second is fast and manual.

```bash
videre faces                  # 1. find faces and group them (slow, resumable)
videre report --faces         # 2. name the groups in your browser
videre search --person "Alice"
```

Step 2 opens a page on `localhost:7878` with three sections: **People** you have
named, **Unassigned Clusters** (groups it is confident about but has no name
for), and **Singletons** (faces it could not group). Drag a cluster onto a
person to assign it, or create a new person from it.

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
videre faces --max-generic-sim 0.4     # drop blurry/featureless faces (default 0.4)
videre faces --batch 8                 # images per batch (default 8)
videre faces --workers 8               # parallel workers (default: 2x your CPU cores)
videre faces --qlmanage-concurrency 6  # simultaneous HEIC conversions (default 6)
```

## Why some faces are ignored

Before grouping, low-quality faces are held out. They come back as unassigned
singletons, still visible and still nameable by hand, just not grouped.

This exists because low-quality faces produce near-identical featureless
fingerprints regardless of who they are, so grouping them piles unrelated people
into one large mixed cluster. A face is held out if it fails either check:

- **Size** (`--min-face-size`, default 80px). Tiny crops, such as distant faces
  in group shots, are mostly blur once scaled up. On real data, genuine person
  groups were essentially all above 100px per side, while a junk cluster sat
  around 60px.
- **Distinctiveness** (`--max-generic-sim`, default 0.4). Sunglasses, masks,
  profiles, blur, and false positives such as a carved statue face carry little
  identity information. On real data, 0.4 removed about 78% of a mixed junk
  cluster while touching none of the confirmed real-person clusters.

`--min-face-size 0` and `--max-generic-sim 1` each disable their check.

A well-photographed person survives this even in their bad shots, because their
many good photos anchor the group far from the generic average.

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

Detection runs across `--workers` threads, defaulting to twice your core count.
That oversubscription is deliberate: HEIC decoding waits on an external
subprocess rather than the CPU, so other workers use the cores meanwhile.

Measured on a 10-core machine: about 3.23x faster than a single worker, and
raising the HEIC conversion limit from 3 to 6 added another 1.23x, for roughly
4.48x overall. Raising it past 6 buys only a few percent while per-image detect
time creeps up, so 6 is the default.

`--profile` prints per-stage timings (load, detect, align, embed, db write),
which is the quickest way to see whether a slow run is bound by decoding or by
inference.

## More detail

- [Long-running jobs](/guides/long-running-jobs/) covers running this alongside other commands, and resuming.
- [Caches and disk use](/guides/caches/) covers the decode cache this reads from.
- [Backing up](/guides/backup/) covers why the names you assign are the one thing that cannot be recomputed.
