---
title: videre classify
description: Tag each photo as photo, screenshot, document, or meme.
---

Tags each image as `photo`, `screenshot`, `document`, or `meme`, so you can
separate real photographs from the receipts, memes and screenshots that
accumulate in a camera roll.

```bash
videre classify                        # classify everything not done yet
videre classify --reprocess            # redo everything, including already-tagged
videre classify --margin 0.05          # how confident it must be (default 0.05)
videre classify --silent               # no per-image progress
videre classify --db ~/photos.db       # use a specific database
videre classify --model <id>           # classify a specific model's data
videre classify --type image           # only images
videre classify --person Ada           # only photos of a labelled person
```

Classify takes every filter, so a run can be narrowed by anything already known
about a file:

```bash
videre classify --type image --after 2025-01-01     # this year's photos only
videre classify --path ~/Photos/Inbox               # one folder
videre classify --location "Berlin, Germany"        # photos taken near a place
videre classify --person "Alice" --reprocess        # re-label one person's photos
videre classify --ext heic --type image --date 2024 # composed: format, kind, year
```

:::tip
These filters work the same way across commands, and combine. See
[scoping a run](/guides/scoping-a-run/).
:::

## The workflow

It reuses the vectors [`videre embed`](/commands/embed/) already computed, so
there is no new model to download and no image is read from disk again. On a
library that is already embedded it finishes in seconds to minutes rather than
hours.

```bash
videre embed                           # prerequisite, the slow part
videre classify                        # fast, reuses that work
videre search --category screenshot
```

Then use it to clear out the clutter:

```bash
videre search --category screenshot -k 5000 > /tmp/shots.txt
videre search --category meme | xargs -I{} mv {} ~/memes/
videre search --category document        # receipts, tickets, forms
```

Resumable: rerunning only classifies what is not yet done, so adding photos
means `scan`, `embed`, `classify` again and only the new ones are considered.

## Expect a lot of `unknown`

This is the behaviour that surprises people. With the default `--margin 0.05`,
roughly **half a real library comes back `unknown`**, and that is deliberate.

A category is only assigned when the best-matching category beats the
second-best by at least `--margin`. Below that gap, videre stores `unknown`
rather than guessing.

That default was chosen against real data: at 0.05 it produced **no wrong
labels** at the cost of leaving about 55% unlabelled. At 0.02 it caught more
but produced some confidently wrong ones, which is worse: an unknown you can
still find by other means, while a screenshot filed as a document is invisible.

```bash
videre classify --reprocess --margin 0.02   # label more, accept some errors
videre classify --reprocess --margin 0.10   # label less, be surer
```

`--reprocess` is needed when changing `--margin`, since already-classified
images are otherwise skipped.

Note that the underlying scores cluster in a narrow band, so `--margin` is more
sensitive than its scale suggests. Move it in small steps and check the result
before committing to it across a library.

## What each category catches

| Category | Typically |
|---|---|
| `photo` | Anything camera-shaped: people, places, objects, scenes |
| `screenshot` | Phone and desktop captures, app UI, web pages |
| `document` | Receipts, tickets, forms, scans, whiteboards, pages of text |
| `meme` | Image macros, captioned pictures, reaction images |
| `unknown` | The gap between the top two was too small to call |

These are broad visual categories, not content understanding. A photograph *of*
a document tends to land in `document`, which is usually what you want. A
screenshot *of* a photo is genuinely ambiguous and often ends up `unknown`.

## Caveats

**It needs `videre embed` first.** Classification scores existing vectors; there
is nothing to score without them. Images that were not embedded (`.dng`, and
HEIC or video on Linux) are simply absent from the results.

**Videos are excluded entirely**, since none of the four categories describes a
video frame.

**Results are per model.** Classifications are keyed by
[model](/reference/models/) as well as by image, so switching models means
classifying again under the new one. The old results are kept, not overwritten.

**Rerunning after changing `--margin` needs `--reprocess`.** Without it, nothing
happens: every image is already classified and therefore skipped.

**There is no way to correct a label by hand.** Categories are derived, not
edited. If a label is wrong, the levers are `--margin` and choosing a different
model.

## How it works

Each of the four categories has a fixed text description. Those are embedded
once with the same text encoder [`videre search`](/commands/search/) uses, then
every stored image vector is compared against all four by cosine similarity. The
closest category wins, if it wins by more than `--margin`.

That is why it is cheap: it is four text embeddings plus one comparison per
image, with no image decoding at all.

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
| `--person` | only files containing this labeled person, confirmed faces only |
| `--category` | only files `videre classify` gave this category |
| `--has` | only files with this metadata. Supported fields: `gps`, `date` |
| `--missing` | only files missing this metadata. Supported fields: `gps`, `date` |

A scoped run prints `N of M`, so a filter that matches nothing is
distinguishable from an empty library. Full detail, including how missing data
excludes a file, is in [scoping a run](/guides/scoping-a-run/).

## More detail

- [Using several search models](/guides/multiple-models/) covers why classifications do not carry across models.
