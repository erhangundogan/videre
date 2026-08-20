---
title: Using several search models
description: Try a bigger model without losing the work you already did.
---

Search quality depends on the model, and the only way to know whether a larger
one helps *your* photos is to try it. videre keeps each model's work separate,
so trying one costs time but risks nothing.

The three search models, their sizes and dimensions, are listed in
[search models](/reference/models/). This guide is about deciding between them
without losing the work you already did.

## Trying one

```bash
videre embed --model google/siglip2-base-patch16-384
```

That is a **full second pass** over every image, taking about as long as the
first, plus its own download. The existing vectors are untouched throughout.

Then compare on queries you care about:

```bash
videre search "kids playing in snow" --scores
videre search "kids playing in snow" --scores --model google/siglip2-base-patch16-384
```

Use your own hard queries, not easy ones. Anything finds a sunset; the
difference shows up on specific requests such as "a red bicycle against a brick
wall".

Scores are **not comparable between models**. Each has its own scale. Compare
the ordering and which photos appear, not the numbers.

## Switching

```bash
videre config set model google/siglip2-base-patch16-384
videre search "sunset"                    # now uses it by default
```

Switching invalidates nothing. The previous model's vectors stay on disk and
stay queryable:

```bash
videre search "sunset" --model google/siglip-base-patch16-224
```

Switching back is instant, because nothing was deleted.

## Seeing what you have

```bash
videre stats
```

```
Embeddings:
  google/siglip-base-patch16-224              196   768-dim     400 KB
  google/siglip2-base-patch16-384             196   768-dim     400 KB
```

One line per prepared model, with dimensions read from the stored data rather
than a hardcoded table, so an unfamiliar model reports honestly.

Asking for a model you have not prepared gives an error listing the ones you do
have, rather than silently returning nothing.
[`videre dedupe --html`](/commands/dedupe/) is the exception: a missing model disables
its similarity button with a note rather than failing a report that is otherwise
fine.

## Classification is per model too

[`videre classify`](/commands/classify/) scores a specific model's vectors, and
results are stored per model:

```bash
videre classify --model google/siglip2-base-patch16-384
videre search --category screenshot --model google/siglip2-base-patch16-384
```

Preparing a new model does **not** carry classifications across. Run `classify`
again under it, which is quick since it reuses the vectors.

## Removing one

Each model's data is one file:

```
~/.videre/embeddings/<library>-<hash>/<owner>--<model>.db
```

```bash
ls -la ~/.videre/embeddings/*/
rm ~/.videre/embeddings/hashes-3f9a1c04e7b25d68/google--siglip2-base-patch16-384.db
```

Deleting it removes only that model's vectors. Rerun `videre embed --model ...`
to rebuild.

Also remove the weights if you are not keeping the model:

```bash
rm -rf ~/.cache/huggingface/hub/models--google--siglip2-base-patch16-384
```

## Caveats

**Budget the disk.** Each model is roughly 130 MB to 190 MB of vectors per
70,000 photos, plus its download. Three models is a few GB before any photos.
They are stored [per library and per model](/reference/models/#where-the-data-is-kept),
so a second library repeats the cost.

**Do not run two `embed` passes at once.** They both convert HEIC, and
contending for that makes both dramatically slower. See
[long-running jobs](/guides/long-running-jobs/).

**`--batch` is capped at 96 regardless of model.** Above roughly 121 the batched
path silently produces wrong vectors, so higher values are reduced with a
warning. See [`videre embed`](/commands/embed/).

**Libraries from before 0.10** report their model as missing. See
[upgrading](/reference/models/#upgrading-from-before-010); nothing is deleted.
