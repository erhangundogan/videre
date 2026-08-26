---
title: videre mark
description: Set ratings, picks, colour labels and likes on photos, then find them by mark.
---

Marks a photo: a star rating, a keeper/reject pick, a colour label, or a like
(favourite). Marks are stored per photo (by content hash, so they follow a photo
across duplicates and moves) and compose with every other filter in
[`videre search`](/commands/search/).

```bash
videre mark --person "Alice" --location Berlin --rating 5   # set on a selection
videre mark --category screenshot --pick reject             # cull candidates
videre mark --path ~/Photos/2024 --label red --like
videre search --rating 5 | videre mark --like               # target by piping paths in
```

## Setter flags

At least one is required (otherwise there is nothing to do).

| Flag | Sets | Clear with |
|---|---|---|
| `--rating <0-5>` | star rating (`0` clears) | `--rating 0` |
| `--pick <keep\|reject\|none>` | pick (keeper / reject) | `--pick none` |
| `--label <colour\|none>` | colour label | `--label none` |
| `--like` / `--no-like` | like (favourite) | `--no-like` |

`pick` and `like` are two different axes on purpose: `pick reject` is a **culling**
decision (cull this), `like` is a **positive** favourite. There is no "dislike",
because it would just duplicate `reject`.

## Choosing what to mark

Targets come from either the selection flags or a pipe:

- **Selection flags** narrow the library the same way `search` does:
  `--person`, `--date`/`--after`/`--before`, `--location`/`--radius`, `--type`,
  `--ext`, `--mime`, `--path`. See [scoping a run](/guides/scoping-a-run/).
- **Standard input**: if you pipe paths in (`videre search ... | videre mark ...`),
  those files are marked. This is how you mark by an *existing* mark, since the
  mark flags on `mark` itself always *set* rather than filter.

A run prints `N of M`, so a filter matching nothing is distinguishable from an
empty library. With no selection and no pipe, every file is marked.

## Finding photos by mark

The same names are filters on [`videre search`](/commands/search/) (and in the
gallery), composing with everything else:

```bash
videre search --rating 4 --pick keep --person "Alice" --location Berlin
videre search --pick reject | xargs trash      # cull, then delete
videre search --like --date 2024
```

`--rating 4` means **at least 4** stars. `--pick`, `--label` and `--like` are
exact. A photo with no mark never matches, the same rule every filter follows.

## Portability

Ratings and colour labels are standard XMP (`xmp:Rating`, `xmp:Label`), so
videre reads them from your files on [`videre scan`](/commands/scan/) and can
write them back:

```bash
videre mark --path ~/Photos --export-xmp    # write .xmp sidecars (opt-in)
```

Picks and likes have no portable standard and stay in videre's database. See
[`videre scan`](/commands/scan/) for the `--xmp` precedence rule when importing.

## Other options

| Flag | Effect |
|---|---|
| `--dry-run` | Show what would change (or which sidecars `--export-xmp` would write) without writing anything |
| `--db <path>` | Act on a specific database instead of the [resolved default](/commands/config/) |
| `--silent` | Suppress the `N of M` progress line |

## Caveats

**Marks are database-only until you export.** `--export-xmp` writes one `.xmp`
sidecar per photo, next to the file, and is the only thing here that touches
your folders. Nothing is written without it.

**No undo.** Setting a mark overwrites the previous value; there is no history.
