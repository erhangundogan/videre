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

After running this, name the groups with
[`videre report --faces`](/commands/report/), then find people with
[`videre search --person`](/commands/search/).

The first run downloads about 180 MB, separate from the search model.

## Resumable

Every processed image is recorded, **including images where no face was found**.
That is what makes reruns cheap: a photo with no faces is examined once ever,
not on every run.

Work is committed as the run proceeds, so Ctrl-C loses at most the in-flight
batch. `--limit` processes at most N new images then stops, for chipping away at
a large library in bounded chunks. A limited run skips the final grouping step,
since that is a whole-library pass not worth repeating after every chunk — run
`videre faces --recluster` once scanning is complete.

## Tuning

Only worth touching if grouping looks wrong. `--recluster` re-runs grouping with
new values without re-detecting anything, which makes experimenting cheap.

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

### The quality gate

Before grouping, low-quality faces are held out entirely. They come back as
unassigned singletons, still visible and still nameable by hand.

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

Residual junk that slips through both is best cleared with the labeling UI's
"Dissolve cluster" button, rather than by tightening these further, which starts
removing real faces.

### Why grouping runs in two stages

Average-linkage grouping alone fragments one person into several groups, because
one person's photos legitimately spread wide (pose, lighting, age). A second
pass then merges any two established groups whose averaged fingerprints are at
least `--merge-sim` alike.

Deciding on averages rather than individual faces is what makes this safe: on
real data, confirmed *different* people never exceeded about 0.29, while one
person's fragments ran 0.37 to 0.76. Only established groups take part, never
lone singletons — a single bad crop can resemble a different person, whereas a
whole group's average cannot.

## Performance

Detection runs across `--workers` threads, defaulting to twice your core count.
That oversubscription is deliberate: HEIC decoding waits on an external
subprocess rather than the CPU, so other workers use the cores meanwhile.

Measured on a 10-core machine: about 3.23x faster than a single worker, and
raising the HEIC conversion limit from 3 to 6 added another 1.23x, for roughly
4.48x overall. Raising it past 6 buys only a few more percent, so 6 is the
default.

Running `videre watch --heic` first makes HEIC detection dramatically faster,
about 108 ms per image against 7.6 s, by reusing already-decoded images. See
[cautions](/start/cautions/) on running two heavy commands at once.
