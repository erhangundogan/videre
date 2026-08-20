---
title: Search models
description: Choosing a model, using more than one, and where the data is kept.
---

Search is powered by a model that runs entirely on your machine. Weights are
downloaded on first use of a command that needs them, never at install.

| Command | Model | Downloaded size |
|---|---|---|
| [`videre embed`](/commands/embed/), [`videre search`](/commands/search/) | SigLIP, default [`google/siglip-base-patch16-224`](https://huggingface.co/google/siglip-base-patch16-224) | about 780 MB |
| [`videre faces`](/commands/faces/) | InsightFace [`WePrompt/buffalo_l`](https://huggingface.co/WePrompt/buffalo_l) (SCRFD detector + ArcFace embedder) | about 180 MB |

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

## Using more than one

Each model keeps its own data, so they never overwrite each other and you can
compare them on the same library:

```bash
videre embed                                                   # the default model
videre embed --model google/siglip2-base-patch16-384           # a second one
videre stats                                                   # see what each has
videre search "sunset" --model google/siglip2-base-patch16-384 # search a specific one
videre config set model google/siglip2-base-patch16-384        # make it the default
```

Preparing a second model does not disturb the first, and switching back and
forth invalidates nothing.

Asking for a model you have not prepared gives an error listing the ones you do
have, rather than silently returning no results. [`videre dedupe --html`](/commands/dedupe/)
is the exception: a missing model disables its in-page similarity search with a
note, rather than failing a report that works fine without it.

Only `videre embed` creates a model's data. Everything else reads.

## Where the data is kept

Not in the main database. Each library and model pair gets its own file:

```
~/.videre/embeddings/<library>-<hash>/<owner>--<model>.db
```

Per library rather than one shared file per model, because
[`videre prune`](/commands/prune/) cannot see another library's contents. A
shared layout would let one library's cleanup delete data another still needs,
and an embedding costs hours to rebuild.

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
