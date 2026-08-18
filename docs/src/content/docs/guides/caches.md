---
title: Caches and disk use
description: Everything videre stores on disk, how big it gets, and what clearing it costs.
---

videre keeps three caches. All are derived, so all can be deleted, but they cost
very different amounts to rebuild.

| Cache | Where | Typical size | Cost to lose |
|---|---|---|---|
| Thumbnails and decodes | `~/.cache/videre/thumbnails/` | **tens of GB** | Seconds each, re-decoded on demand |
| Model weights | `~/.cache/huggingface/hub/` | ~960 MB | A download |
| Geocoded place names | inside the database | tiny | One network lookup each |

Embeddings are **not** a cache. They are hours of computation and are covered
under [backing up](/guides/backup/).

## Thumbnail cache

The big one, and the only one that grows without limit.

```
~/.cache/videre/thumbnails/          # normally
<VIDERE_HOME>/cache/thumbnails/      # when VIDERE_HOME is set
```

Files are named by content hash:

| File | What |
|---|---|
| `<hash>_240.jpg` | Grid thumbnail |
| `<hash>_1200.jpg` | Lightbox size |
| `<hash>_original.jpg` | **Full-resolution decode** |
| `<hash>_face<id>_<size>.jpg` | Cropped face |

The full-resolution copies are what make it large. They exist because
[`videre faces`](/commands/faces/) needs full resolution to place face boxes,
and reusing one is roughly 70x faster than decoding again (~108 ms against
~7.6 s).

### Managing it

```bash
videre stats                                # how big, alongside everything else
rm -rf ~/.cache/videre/thumbnails/          # safe, regenerates on demand
```

[`videre stats`](/commands/stats/) has a **Disk use** section listing every store
largest first and marking which are rebuildable, so you can see whether the cache
is actually the thing worth clearing before clearing it. It also knows where the
cache is, which `du` needs telling - the path differs depending on whether
`VIDERE_HOME` is set.

Deleting it is genuinely safe. Every file is derived, and the only cost is
re-conversion the next time something needs the image.

There is **no** size limit, age-based expiry, or eviction. The only automatic
cleanup is [`videre prune`](/commands/prune/), and it only removes entries for
photos that are no longer in the database. Cache for photos you still own is
never reclaimed.

### It is shared between databases

Keyed by content and stored in one directory, so the same photo in two libraries
is converted once. The consequence is that **one library's `prune` can delete
another's entries**, since prune can only see its own database.

That is accepted deliberately: a thumbnail costs milliseconds to rebuild, so the
same flaw that would be unacceptable for embeddings is unimportant here.

Because the location follows `VIDERE_HOME`, switching homes means starting from
an empty cache and leaving the old one behind. See
[keeping libraries separate](/guides/multiple-libraries/).

### Warming it deliberately

```bash
videre watch ~/Photos --heic     # decode and cache everything, then Ctrl-C
videre faces                     # now reads the cache instead of decoding
```

Worth doing before a long face-detection run on a HEIC-heavy library. Do not run
both at once; see [long-running jobs](/guides/long-running-jobs/).

## Model weights

Downloaded on first use, never at install:

```
~/.cache/huggingface/hub/
  models--google--siglip-base-patch16-224/     ~780 MB
  models--WePrompt--buffalo_l/                 ~180 MB
```

Set `HF_HOME` to move it. It is the standard Hugging Face location, so other
tools on your machine may share it.

```bash
du -sh ~/.cache/huggingface/hub/
rm -rf ~/.cache/huggingface/hub/models--google--siglip-base-patch16-224
```

Deleting means the next [`embed`](/commands/embed/) or
[`search`](/commands/search/) downloads it again. Your embeddings are unaffected:
they live elsewhere and stay queryable.

Selecting a larger [model](/reference/models/) downloads it in addition, not
instead. Unused ones sit there until removed by hand.

## Geocode cache

A table inside the database, filled by
[`videre search --location`](/commands/search/) so a repeated place-name query
never repeats the network request.

```sql
SELECT query, lat, lon, resolved_at FROM geocode_cache ORDER BY resolved_at DESC;
DELETE FROM geocode_cache;              -- safe; just re-looks-up next time
```

Tiny, and the only reason to clear it is if a place resolved to somewhere wrong.

Separate from the `location_name` column that
[`watch --location`](/commands/watch/) fills, which is a reverse lookup done
offline.

## Where disk actually goes

```bash
du -sh ~/.videre/                     # database, config, embeddings
du -sh ~/.videre/embeddings/          # ~130-190 MB per model per 70k photos
du -sh ~/.cache/videre/thumbnails/    # usually the largest
du -sh ~/.cache/huggingface/hub/      # ~960 MB with defaults
videre stats                          # per-model embedding sizes
```

On a large HEIC library the thumbnail cache usually dwarfs everything else, and
it is also the safest thing to delete. Work through it in that order:

1. `rm -rf` the thumbnail cache. Free, regenerates.
2. Remove model weights you no longer use. Free, re-downloads.
3. `videre prune` to drop derived data for photos that are gone.
4. Delete an unused model's embeddings directory, if you tried one and moved on.

Nothing in that list touches your photos.
