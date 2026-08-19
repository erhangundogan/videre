---
title: Using several search models
description: Try a bigger model without losing the work you already did.
---

Search quality depends on the model, and the only way to know whether a larger
one helps *your* photos is to try it. videre keeps each model's work separate,
so trying one costs time but risks nothing.

## The models

| Model | Download | Notes |
|---|---|---|
| [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224) | ~780 MB | The default |
| [`google/siglip2-base-patch16-384`](https://huggingface.co/google/siglip2-base-patch16-384) | ~1.4 GB | Newer, higher resolution |
| [`google/siglip-so400m-patch14-384`](https://huggingface.co/google/siglip-so400m-patch14-384) | ~3.3 GB | Largest |

Each links to its model card on Hugging Face, where the training data, intended
use, and limitations are documented by the people who built it.

Higher resolution and more parameters generally mean better matching on fine
detail, at proportionally more time per image and more disk.

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

## Why they are stored per library

`~/.videre/embeddings/<library>-<hash>/` is keyed by library as well as model,
even though content hashes would technically allow sharing between libraries.

[`videre prune`](/commands/prune/) cannot see another library's files, so a
shared layout would let one library's cleanup delete vectors another still
needed. A thumbnail lost that way costs milliseconds; an embedding costs hours.
See [caches](/guides/caches/) for the same tradeoff decided the other way.

The library part of the path includes a hash of its canonical path, so two
databases both named `photos.db` in different folders never collide.

## Caveats

**Budget the disk.** Each model is roughly 130 MB to 190 MB of vectors per
70,000 photos, plus its download. Three models is a few GB before any photos.

**Do not run two `embed` passes at once.** They both convert HEIC, and
contending for that makes both dramatically slower. See
[long-running jobs](/guides/long-running-jobs/).

**`--batch` is capped at 96 regardless of model.** Above roughly 121 the batched
path silently produces wrong vectors, so higher values are reduced with a
warning. See [`videre embed`](/commands/embed/).

**Libraries from before 0.10** kept vectors in the main database. That fallback
was removed in 0.11, so such a library reports the model as missing. Nothing is
deleted; rerun `videre embed` to rebuild in the current layout.
