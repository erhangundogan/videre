---
title: Search models
description: Which models exist, how one is selected, and where their data is kept.
---

Search is powered by a model that runs entirely on your machine. Weights are
downloaded on first use of a command that needs them, never at install.

### Search models

Selected with `--model`. Each link goes to the model card on Hugging Face, where
training data, intended use and limitations are documented by the people who
built it.

| Model | Download | Dimensions | Notes |
|---|---|---|---|
| [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224) | ~780 MB | 768 | The default |
| [`google/siglip2-base-patch16-384`](https://huggingface.co/google/siglip2-base-patch16-384) | ~1.4 GB | 768 | Newer, higher resolution |
| [`google/siglip-so400m-patch14-384`](https://huggingface.co/google/siglip-so400m-patch14-384) | ~3.3 GB | 1152 | Largest |

Higher resolution and more parameters generally mean better matching on fine
detail, at proportionally more time per image and more disk. Whether that helps
*your* photos is an empirical question: see
[using several search models](/guides/multiple-models/).

### Face model

Not selectable. [`videre faces`](/commands/faces/) always uses InsightFace
[`WePrompt/buffalo_l`](https://huggingface.co/WePrompt/buffalo_l), about 180 MB,
an SCRFD detector plus an ArcFace embedder.

Both live in the shared Hugging Face cache at `~/.cache/huggingface/hub/`,
overridable with `HF_HOME`. See
[what gets downloaded](/start/install/#models-are-not-downloaded-at-install).

`scan`, `dedupe`, `fix-dates`, `prune`, `stats`, and `locations`
without similarity search need **no model at all**.

## Choosing one

`embed`, `search`, `classify`, `gallery` and `mcp` all take `--model <id>`,
resolved as `--model` first, then `default_model` in your config, then the
built-in default.

```bash
videre config set model google/siglip2-base-patch16-384   # lasting default
videre embed --model google/siglip2-base-patch16-384      # just this once
videre config                                             # show what resolves
```

Non-default models are larger and are only fetched if you actually select one:
[`siglip2-base-patch16-384`](https://huggingface.co/google/siglip2-base-patch16-384)
is about 1.4 GB, and
[`siglip-so400m-patch14-384`](https://huggingface.co/google/siglip-so400m-patch14-384)
about 3.3 GB.

## One model never disturbs another

Each model keeps its own data, so preparing a second leaves the first untouched
and switching between them invalidates nothing. **Only `videre embed` creates a
model's data; everything else reads it.**

Asking for a model you have not prepared is an error listing the ones you do
have, rather than silently returning nothing.
[`videre dedupe --html`](/commands/dedupe/) is the exception: a missing model
disables its in-page similarity search with a note, rather than failing a page
that works without it.

**For how to actually try, compare, switch and remove one**, see
[using several search models](/guides/multiple-models/).

## Where the data is kept

Not in the main database. Each library and model pair gets its own file:

```
~/.videre/embeddings/<library>-<hash>/<owner>--<model>.db
```

Per library rather than one shared file per model, because
[`videre prune`](/commands/prune/) cannot see another library's contents. A
shared layout would let one library's cleanup delete data another still needs,
and an embedding costs hours to rebuild. [Caches](/guides/caches/) shows the same
tradeoff decided the other way, for thumbnails.

The library part of the path includes a hash of its canonicalised path, so two
libraries both called `photos.db` in different folders never collide.

Expect roughly 130 MB to 190 MB per model for a 70,000 photo library.
`videre stats` reports the actual figure per model.

## Upgrading from before 0.10

Data written by 0.9.x lived in the main database. That fallback was removed in
0.11.0, so such a library now reports the same clear error as any other missing
model.

Nothing is deleted: the old data sits untouched and can be dropped by hand.
Rerun `videre embed` to rebuild in the current layout.
