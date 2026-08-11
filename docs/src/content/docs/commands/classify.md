---
title: videre classify
description: Tag each photo as photo, screenshot, document, or meme.
---

Tags each photo as photo, screenshot, document, or meme. Reuses the work
[`videre embed`](/commands/embed/) already did, so it is quick: no new model, no
re-reading of images.

```bash
videre classify                        # classify everything not done yet
videre classify --reprocess            # redo everything, including already-tagged
videre classify --margin 0.05          # how confident it must be (default 0.05)
videre classify --silent               # no per-image progress
videre classify --db ~/photos.db       # use a specific database
videre classify --model <model-id>     # classify a specific model's data
```

Then find them with [`videre search --category`](/commands/search/):

```bash
videre search --category screenshot
```

Resumable: rerunning only classifies what is not yet done, unless `--reprocess`.

## Confidence

Anything it is not confident about is stored as `unknown` rather than guessed.
`--margin` is the minimum gap between the best and second-best category needed
to accept a result.

The default of 0.05 was chosen against real data: it produced no wrong labels,
at the cost of leaving about 55% unknown. A lower 0.02 caught more but produced
some confidently wrong ones. Raise it for stricter tagging, lower it if you
would rather have a guess than an `unknown`.

Videos are excluded, since none of the four categories fit a video frame.
