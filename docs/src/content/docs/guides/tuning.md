---
title: Tuning
description: The knobs worth turning, what they cost, and the numbers behind the defaults.
---

Every default here was measured, not guessed. **You should not need this page**:
the defaults are the right answer for almost every library, and the commands
that expose these flags work without them. Read on when a run is slow enough
that you want to know why, or when your hardware is unusual.

Each flag is also listed on its own command's page. This is where the reasoning
lives.

## Rule of thumb

| Symptom | Look at |
|---|---|
| scan skips large files on a slow drive | [`read-rate`](#slow-drives-and-large-files) |
| face detection is slower than expected | [`--workers`, `--profile`](#face-detection) |
| an interrupted run loses too much work | [`--chunk`](#how-often-work-is-committed) |
| embedding a big library takes too long | [`--batch`, `VIDERE_EMBED_DTYPE`](#embedding) |

## Slow drives and large files

Reading a file is bounded by a timeout, so a disconnected drive cannot hang a
scan forever. That bound **scales with file size** rather than being constant: a
multi-gigabyte video legitimately takes longer to read than a photo, and a fixed
ceiling cannot tell a large file from a stalled one.

The size itself is read first, under a short separate timeout. That ordering is
the safety property: a dead mount fails there, and the read is never started.

The assumed floor rate is 20 MB/s. On a mount slower than that, healthy files
get reported as unreachable - and because size does not change, the *same* files
fail on every run, which by definition are your longest videos:

```bash
videre config set read-rate 5
```

## Face detection

`--workers` defaults to twice your core count. The oversubscription is
deliberate: HEIC decoding waits on an external subprocess rather than the CPU,
so other workers use the cores meanwhile.

Measured on a 10-core machine: about **3.23x** faster than a single worker, and
raising the HEIC conversion limit from 3 to 6 added another **1.23x**, for
roughly **4.48x** overall. Past 6 the gain is a few percent while per-image
detect time creeps up, so 6 is the default.

`--profile` prints per-stage timings (load, detect, align, embed, db write) and
is the quickest way to see whether a slow run is bound by decoding or inference.

`--eps` and `--min-cluster-size` control how faces are grouped into people, not how
fast detection runs. They are covered on the
[faces page](/commands/faces/#fixing-bad-grouping), since changing them changes
results rather than speed.

## Embedding

`--batch` is how many images go through the model at once. The default is 32 and
the maximum is 96.

:::caution[The cap is not arbitrary]
Above roughly 121, this inference path returns **wrong vectors with no error**:
no crash, no NaN, just embeddings that do not match a one-at-a-time baseline.
Values over 96 are reduced with a warning. Checking output for zeros or NaNs
does not detect it.
:::

Larger batches buy little anyway: 31.0 ms/image at 96 against 39.1 ms at 768.

`VIDERE_EMBED_DTYPE=f16` switches inference to half precision: about 11% faster
on pure JPEG/PNG, 7% on a realistic mix, with no meaningful quality change. It
is opt-in because 7% did not justify perturbing an existing library, and it does
not affect vectors already written.

## How often work is committed

`--chunk` sets how many rows are written per transaction. Larger values are
slightly faster and lose more if the run is interrupted. Both `embed` and
`classify` accept it; the default is 500.

Resumability does not depend on this. Every command records what it has already
*tried*, not just what produced a result, so an interrupted run picks up where
it stopped either way. See [long-running jobs](/guides/long-running-jobs/).

## Concurrency across commands

The HEIC conversion limit is **per process**. Running two videre commands at
once therefore permits twice as many conversions against one shared QuickLook
agent, and they contend: with `faces` and `embed` running together, HEIC load
averaged 16.3s against ~7.6s uncontended, and one file exceeded the 20s timeout
that converted in 0.39s on its own.

The impact is bounded - a skipped file is not marked as done and self-heals on
the next run - but if you use [`videre watch`](/commands/watch/), expect manual
commands run alongside it to be slower than they are on their own.
