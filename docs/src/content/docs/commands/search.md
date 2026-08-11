---
title: videre search
description: Find photos by description, example image, person, category, or place.
---

```bash
videre search "sunset over water"          # search by description
videre search --image photo.jpg            # find photos like this one
videre search --person "Alice"             # photos of a named person
videre search --category screenshot        # photo / screenshot / document / meme / unknown
videre search --location "Berlin, Germany" # photos taken near a place
videre search "a dog" -k 50                # more results, default 20 (--top-k works too)
videre search "a dog" --scores             # show how well each result matched
videre search "a dog" --json               # print one JSON object instead
videre search --location "Rome" --radius 5 # tighter radius in km (default 20)
videre search "a dog" --db ~/photos.db     # use a specific database
videre search "a dog" --model <model-id>   # search a specific model's data
```

## What each mode needs

| Mode | Requires |
|---|---|
| Text, `--image` | [`videre embed`](/commands/embed/) |
| `--person` | [`videre faces`](/commands/faces/), then naming via [`report --faces`](/commands/report/) |
| `--category` | [`videre classify`](/commands/classify/) |
| `--location` | GPS data in your photos |

Matching paths print to stdout, all duplicate paths for each matched file.

## How the modes differ

Text and image search are ranked by similarity, and `-k` limits the results.

`--person` and `--category` are set membership, not ranked queries, so they
ignore `-k` and have no score.

`--location` is a "k nearest" query: results are sorted by distance ascending
and truncated to `-k`. With `--scores` it prepends the distance in km rather
than a similarity score.

## `--location` is the one mode that uses the network

It looks up an arbitrary place name — not limited to places already in your
library — via the Nominatim (OpenStreetMap) public geocoding API. This is the
only network call videre ever makes.

The result is cached locally, keyed by the query string, so repeating a query
never repeats the lookup. That cache write also makes `--location` the only
search mode that writes to the database, and the only one not exposed through
[`videre mcp`](/commands/mcp/).

For grouping photos by places already in your library, with no network at all,
use [`videre locations`](/commands/locations/) instead.

## JSON output

`--json` emits a single document with `schema_version`, `query`, `count`, and
`results`. Fields per hit vary by mode:

| Mode | Fields |
|---|---|
| Text, `--image` | `path`, `hash`, `score` |
| `--person` | `path` |
| `--category` | `path`, `hash` |
| `--location` | `path`, `hash`, `distance_km` |

`--scores` is a no-op under `--json`, since the score is always included.
