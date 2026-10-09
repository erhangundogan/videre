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
| [`google/siglip2-base-patch16-224`](https://huggingface.co/google/siglip2-base-patch16-224) | ~1.5 GB | 768 | The default |
| [`google/siglip2-base-patch16-384`](https://huggingface.co/google/siglip2-base-patch16-384) | ~1.5 GB | 768 | The same model at 384px: finer detail, about 4x slower to embed |
| [`google/siglip2-so400m-patch14-384`](https://huggingface.co/google/siglip2-so400m-patch14-384) | ~4.5 GB | 1152 | Largest and slowest |

All three are SigLIP 2, which understands queries in many languages, Turkish
included. Searched by their own captions on 300 test photos, the default
found the right photo first for 84% of English queries and 52% of Turkish
ones; the SigLIP 1 model it replaced managed 81% and 37%, at the same speed.

Higher resolution and more parameters generally mean better matching on fine
detail, at proportionally more time per image and more disk. Whether that helps
*your* photos is an empirical question: see
[using several search models](/guides/multiple-models/).

Any other SigLIP or SigLIP 2 model on Hugging Face can be given by id, such as
the older English-only
[`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224).
Two SigLIP 2 kinds do not load: the `-naflex` models, and the `giant-opt`
ones.

A library whose `config.toml` already names a `default_model` keeps it.
videre writes that key when it creates a library, so most libraries made
before SigLIP 2 became the default still use
`google/siglip-base-patch16-224`; `videre config` shows which. To move one
over:

```bash
videre config set model google/siglip2-base-patch16-224
videre embed
```

The old vectors stay on disk until you remove them; see
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

The other models are only fetched if you actually select one:
[`siglip2-base-patch16-384`](https://huggingface.co/google/siglip2-base-patch16-384)
is about 1.5 GB, and
[`siglip2-so400m-patch14-384`](https://huggingface.co/google/siglip2-so400m-patch14-384)
about 4.5 GB.

## One model never disturbs another

Each model keeps its own data, so preparing a second leaves the first untouched
and switching between them invalidates nothing. **Only `videre embed` creates a
model's data; everything else reads it.**

Asking for a model you have not prepared is an error listing the ones you do
have, rather than silently returning nothing.
[`videre gallery`](/commands/gallery/) is the exception: without the model's
data it still serves every page, and leaves out **Similar** with a note.

**For how to actually try, compare, switch and remove one**, see
[using several search models](/guides/multiple-models/).

## Where the data is kept

Not in the main database. Each library and model pair gets its own file:

```
<library>/.videre/embeddings/<owner>--<model>.db
```

Per library rather than one shared file per model, because
[`videre prune`](/commands/prune/) cannot see another library's contents. A
shared layout would let one library's cleanup delete data another still needs,
and an embedding costs hours to rebuild. [Caches](/guides/caches/) keeps
thumbnails per library for the same reason.

Expect roughly 130 MB to 190 MB per model for a 70,000 photo library.
`videre stats` reports the actual figure per model.
