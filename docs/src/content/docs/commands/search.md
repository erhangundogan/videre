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
videre search --date 2019-09               # only that month
videre search --after 2020-01-01           # inclusive lower bound
videre search --before 2020-01-01          # exclusive upper bound
videre search --type video                 # only videos
videre search --ext mov,mp4                # only these extensions
videre search --mime video/quicktime       # an exact type
videre search --path ~/Photos/2024         # only files under a folder
videre search --sort=distance,date         # order, with tie-breaks
```

## What each mode needs

| Mode | Requires |
|---|---|
| Text, `--image` | [`videre embed`](/commands/embed/) |
| `--person` | [`videre faces`](/commands/faces/), then naming via [`gallery`](/commands/gallery/) |
| `--category` | [`videre classify`](/commands/classify/) |
| `--location` | GPS data in your photos |

Matching paths print to stdout, all duplicate paths for each matched file.

:::note[`--person` ignores case, accents and spacing]
`--person alice`, `--person Alice` and `--person ALICE` are the same query, and
`--person "Işıl Özyeğin"` works whether you type the accents or not. Both the
name shown in the labeling UI and the short form in the URL are accepted, so
`--person "Ahmet Arı"` and `--person ahmet_ari` find the same photos.

It still matches a *whole* person, not a part of one: `--person Erhan` will not
find `Erhan Gündoğan`, because those are two different people as far as videre
is concerned.
:::

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
  Use `--person` for people, and the [date view in `gallery`](/commands/gallery/) for
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

## Narrowing by kind, format or folder

Four filters need no metadata beyond the file itself:

| Flag | Selects |
|---|---|
| `--type` | `image` or `video` |
| `--ext` | file extension, e.g. `mov` |
| `--mime` | exact type, e.g. `video/quicktime` |
| `--path` | files under a folder |

`--type`, `--ext` and `--mime` are repeatable and accept comma-separated lists,
so `--ext mov,avi` and `--ext mov --ext avi` are the same request. `--type` is
the broad one: it covers every image or video format rather than a named list.

```bash
videre search "sunset" --type video
videre search "birthday" --ext heic --path ~/Photos/2024
```

These same four work on [`videre scan`](/commands/scan/),
[`watch`](/commands/watch/), [`embed`](/commands/embed/),
[`faces`](/commands/faces/) and [`classify`](/commands/classify/), where they
narrow the *work* rather than the results. See
[scoping a run](/guides/scoping-a-run/).

## Filters compose

`--person`, `--category`, `--location`, `--type`, `--ext`, `--mime`, `--path`
and the date bounds are **filters**: give any combination and they AND together,
each narrowing further.

A text query or `--image` is a **ranker**: at most one, and it orders whatever
the filters left.

```bash
# every axis at once: what it is, who is in it, where and when
videre search "cake" --category photo --person "Alice" \
  --location "Istanbul" --radius 5 --date 2025-05 --type image

# the videos from one trip, newest first
videre search --type video --location "Rome" --after 2024-06-01 \
  --before 2024-07-01 --sort date:desc

# a visual match, restricted to originals rather than exports
videre search --image ~/Desktop/reference.jpg --ext heic --path ~/Photos/originals

# documents photographed in one city, best match first
videre search "receipt" --category document --location "Berlin, Germany" --radius 10

# clips only, from one folder, ignoring everything re-encoded to mp4
videre search "beach" --ext mov --path ~/Photos/2024 -k 50
```

Order does not matter, and neither does how many you give. Filtering happens
before ranking, so a heavily filtered query scores fewer vectors and returns
faster than an unfiltered one.

`-k` truncates the result of all of it, and `--sort` decides the order. See
[compositional searches](/guides/compositional-search/) for the full model,
worked examples and recipes.

## Dates

```bash
videre search --date 2019            # a whole year
videre search --date 2019-09         # a month
videre search --date 2019-09-14      # a day
videre search --after 2020-01-01     # everything since (inclusive)
videre search --before 2020-01-01    # everything before (exclusive)
```

`--date` is shorthand and conflicts with `--after`/`--before`. `--before` is
exclusive so adjacent ranges never both claim the boundary.

Dates match the **capture date when the file has one, otherwise its
modification time**, so screenshots and anything without embedded metadata are
still reachable. That fallback can mix "when taken" with "when last written";
see the [guide](/guides/compositional-search/) for what that means in practice.

Videos carry their own capture date and coordinates, so they match date and
location filters like photos do. See [videos](#videos) below.

## Videos

Since v0.14.0 videre reads each video's capture date, coordinates, duration and
codec, so **videos appear in date and location results alongside photos**. No
separate flag: the same filters cover both.

```bash
videre search --location "Los Angeles, USA" --radius 30 --date 2024-12
```

On a real library that returns 349 photos and 6 videos together. Before v0.14.0
the videos could not have matched either half, because neither field existed for
them.

:::caution[Older libraries need a re-scan]
Video dates and coordinates only appear after the library has been scanned with
v0.14.0 or later. `--retry-incomplete` will not pick them up, since it looks for
files with no recorded type and these have one:

```bash
videre scan ~/Pictures
```
:::

### Capture date, not file date

The date a video matches is when it was **recorded**, not when the file was
written. Those often differ by years: a clip exported or copied last week still
matches the month it was shot.

If a result looks misplaced, compare Finder's "Created" against the capture
date; Finder shows the filesystem timestamp, videre uses the capture time
embedded in the file.

### Not every video has coordinates

A clip recorded with location services off carries none, and no amount of
scanning invents them. On one real library 16 of 260 were in that position.
Those videos are fully searchable by date and text, they simply cannot appear in
`--location` results.

## Sorting

```bash
videre search --date 2019 --sort date:asc            # oldest first
videre search --location "Berlin" --sort=distance,date
```

`--sort` takes a comma-separated `field[:asc|desc]` list over `relevance`,
`distance`, `date` and `size`. Later fields break ties in earlier ones.
Directions are optional: relevance, date and size default to descending,
distance to ascending.

Omitted, it defaults to `relevance` if you gave a query or `--image`, else
`distance` if you gave `--location`, else `date`.

Asking for a sort whose input is missing is an error rather than a silent
fallback:

```
$ videre search --sort distance
error: --sort distance needs --location <place>
```

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
filter that writes to the database, including when it is used through
[`videre mcp`](/commands/mcp/), which exposes it under the same name.

For grouping photos by places already in your library, with no network at all,
use [`videre locations`](/commands/locations/) instead.

### Files without coordinates never match

Every filter follows one rule: **it matches on the best evidence available, and
excludes a file only when there is no evidence at all.**

For location there is no fallback. A file either carries coordinates or it
carries nothing, so a file without them is not a near-miss, it is not a
candidate: asking for a place is asking for files known to have been there, and
an unknown location is not evidence of being anywhere.

Dates are the one axis where a fallback exists, which is why they behave
differently: every file has a modification time, so there is always *some*
evidence. The capture date is preferred and the file's timestamp is used only
when that is all there is. `--person` and `--category` work like location: no
recorded face or classification means no evidence, so the file is excluded.

This surprises people most often with:

- **Screenshots, downloads and received images**, which carry no GPS at all
- **Photos taken with location services off**, which are ordinary photos in
  every other respect
- **Videos scanned before v0.14.0**, which had no coordinates recorded even
  when the file contained them; a re-scan fixes those

So a result count smaller than you expect usually means part of the library has
no coordinates rather than that the radius is too small. To see how much of your
library can participate at all:

```bash
videre stats
```

Widening `--radius` cannot bring in a file that has no coordinates, however
large you make it.

## Caveats

**`-k` now applies to `--person` and `--category`.** They previously returned
every match, unordered. They are now truncated like any other search and
ordered deterministically. Pass a large `-k` for the full set.

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

- [Compositional searches](/guides/compositional-search/) covers combining filters, dates and sorting, with worked examples.

- [Using several search models](/guides/multiple-models/) covers comparing models on your own queries.

## A page you can keep

`--html` writes the results to a browsable file, in the order they were ranked.

```bash
videre search "sunset over water" --html            # writes <db>_search.html
videre search --person "Ahmet" --html ~/ahmet.html  # somewhere specific
```

Matching paths still go to stdout, so piping is unaffected.

:::caution
Place a bare `--html` after the query, or the query is read as its value.
:::
