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

## Writing queries that work

The model matches images to *descriptions of what is visible*. Plain descriptive
phrases work best:

```bash
videre search "a dog on a beach"
videre search "snow covered mountains"
videre search "birthday cake with candles"
videre search "close up of a red flower"
videre search "people sitting around a dinner table"
```

Things it is good at: subjects, scenes, colours, weather, obvious activities,
broad settings such as indoors, city street, forest.

Things it is **not**:

- **Reading text.** It is not OCR. "the receipt from the hardware store" will
  not work; try [`--category document`](/commands/classify/) to narrow to
  documents instead.
- **Counting.** "three cats" is treated much like "cats".
- **Boolean logic.** There is no AND, OR or NOT. "dog not cat" is just a phrase,
  and the negation is ignored.
- **Names and dates.** It has no idea who Alice is or when a photo was taken.
  Use `--person` for people, and the [by-date report](/commands/report/) for
  time.

Every result is a ranked match, so something always comes back even for a query
nothing fits. Use `--scores` to see whether a match is real:

```bash
videre search "a dog on a beach" --scores
0.284  /Photos/2019/beach-day-12.jpg
0.271  /Photos/2019/beach-day-08.jpg
0.118  /Photos/2021/garden.jpg
```

Scores are cosine similarities, and only comparable *within* one query. There is
no universal cutoff, but a sharp drop down the list is usually where genuine
matches stop.

## Finding photos like one you have

```bash
videre search --image ~/Desktop/reference.jpg -k 40
```

The query image does not need to be in your library. This is often the fastest
way to find a series: pick one frame you remember and pull the rest.

## Using the results

Paths print one per line, so this pipes like any other tool:

```bash
videre search "screenshots of code" -k 100 > /tmp/found.txt
videre search "blurry photo" -k 50 | xargs -I{} mv {} ~/to-review/
open $(videre search "golden gate bridge" -k 5)
```

With `--json` you get structured output for scripting:

```bash
videre search "sunset" --json | jq -r '.results[] | select(.score > 0.25) | .path'
```

That threshold trick is the usual way to turn a ranked list into a filtered one,
since `-k` limits count rather than quality.

## Searching a specific model

If you have prepared more than one [model](/reference/models/), each is searched
separately:

```bash
videre search "sunset over water" --model google/siglip2-base-patch16-384
```

Comparing the same query across two models on your own library is the practical
way to decide whether a larger one is worth its cost:

```bash
videre search "kids playing in snow" --scores
videre search "kids playing in snow" --scores --model google/siglip2-base-patch16-384
```

Asking for a model you have not prepared gives an error naming the ones you do
have, rather than silently returning nothing.

## How the modes differ

Text and image search are ranked by similarity, and `-k` limits the results.

`--person` and `--category` are set membership, not ranked queries, so they
ignore `-k` and have no score.

`--location` is a "k nearest" query: results are sorted by distance ascending
and truncated to `-k`. With `--scores` it prepends the distance in km rather
than a similarity score.

## `--location` is the one mode that uses the network

It looks up an arbitrary place name, not limited to places already in your
library, via the Nominatim (OpenStreetMap) public geocoding API. This is the
only network call videre ever makes.

```bash
videre search --location "Kreuzberg, Berlin" --radius 3
videre search --location "Iceland" --radius 300 -k 200
```

Radius matters more than it looks: a city name with the default 20 km will
include its suburbs, while a neighbourhood needs a few km to be meaningful.

The result is cached locally, keyed by the query string, so repeating a query
never repeats the lookup. That cache write makes `--location` the only search
mode that writes to the database, and the only one not exposed through
[`videre mcp`](/commands/mcp/).

For grouping photos by places already in your library, with no network at all,
use [`videre locations`](/commands/locations/) instead.

## Caveats

**Video results are weaker.** Videos are embedded from a single frame, so a clip
only matches if its opening frame shows the subject.

**Results are as fresh as your last `embed`.** New photos are not searchable
until [`videre embed`](/commands/embed/) has covered them. Running
[`videre watch`](/commands/watch/) keeps that current for you.

**Search never reads your files.** It compares stored vectors, so it works with
the drive unplugged; the paths it prints just will not resolve.

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

## More detail

- [Using several search models](/guides/multiple-models/) covers comparing models on your own queries.
