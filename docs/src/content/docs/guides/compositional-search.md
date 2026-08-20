---
title: Compositional searches
description: Combine filters and dates in one query, and control the order of results.
---

Every filter on [`videre search`](/commands/search/) composes. Give several and
they AND together, each one narrowing further.

## The filters you can combine

| Flag | Selects |
|---|---|
| `--person` | who is in it |
| `--category` | how `videre classify` labelled it |
| `--location`, `--radius` | where it was taken |
| `--after`, `--before`, `--date` | when it was taken |
| `--type`, `--ext`, `--mime` | what kind of file it is |
| `--path` | which folder it is in |

Every condition must hold, so adding a flag can only narrow the result.

The same vocabulary narrows the *work* on the long-running commands, not just
the results of a search: see [scoping a run](/guides/scoping-a-run/).

## The one idea

**Filters narrow. A ranker orders what survives.**

- **Filters:** `--person`, `--category`, `--location`, and the date bounds.
  Any number, in any combination.
- **Rankers:** a text query, or `--image`. At most one, and optional.

If you give no ranker, results come back ordered by date. That is the whole
model; the rest of this page is detail.

## Watch it narrow

Each flag cuts the set down. Real counts from a small test library:

```bash
videre search --category photo                        # 61 results
videre search --category photo --after 2020-01-01     # 17
videre search --category photo --date 2019            #  1
```

Add a ranker and the survivors get ordered by how well they match:

```bash
videre search "birthday cake" --category photo --after 2020-01-01
```

That reads as: of the photos taken since 2020, the ones most like a birthday
cake.

## Dates

Three ways to express a range:

```bash
videre search --date 2019            # the whole year
videre search --date 2019-09         # that month
videre search --date 2019-09-14      # that day
videre search --after 2020-01-01     # everything since
videre search --before 2020-01-01    # everything before
videre search --after 2019-06-01 --before 2019-09-01   # a custom span
```

`--date` is shorthand and cannot be combined with `--after` or `--before`.

| `--date` | Matches |
|---|---|
| `2025` | `>= 2025-01-01`, `< 2026-01-01` |
| `2025-05` | `>= 2025-05-01`, `< 2025-06-01` |
| `2025-05-14` | `>= 2025-05-14`, `< 2025-05-15` |

`--before` is **exclusive**, so adjacent ranges tile without both claiming the
boundary. `--date 2025-05` and `--date 2025-06` never return the same file.

### Which date it matches

The **EXIF capture date** when the file has one, otherwise the file's
**modification time**.

That matters because screenshots, PNGs and most videos carry no EXIF at all. A
strict EXIF-only filter would make them unreachable by date, including the
screenshots you are most likely to want to find.

:::caution[The fallback can mislead]
A photo taken in 2019, copied to a new machine in 2026, with no EXIF date, has a
2026 modification time and will match `--date 2026`.

Results therefore mix "when it was taken" with "when the file was last written".
Running [`videre fix-dates`](/commands/fix-dates/) sets modification times from
EXIF where it exists, which makes the two agree and the fallback more accurate.
:::

## Sorting

```bash
--sort <field>[:asc|desc][,<field>[:asc|desc]]...
```

| Field | Default direction | Needs |
|---|---|---|
| `relevance` | `desc` | a text query or `--image` |
| `distance` | `asc` | `--location` |
| `date` | `desc` | nothing |
| `size` | `desc` | nothing |

Directions are optional because each field already defaults to what you
probably meant: best match first, nearest first, newest first, largest first.

```bash
videre search --date 2019 --sort date:asc          # oldest first
videre search --category photo --sort size         # largest first
```

### Multiple fields break ties

Fields apply left to right, each breaking ties in the one before:

```bash
videre search --location "Berlin" --sort=distance,date
```

Nearest first, and among photos at the same spot, newest first. That tie-break
is not hypothetical: a burst of shots taken standing in one place all share a
GPS fix, so `distance` alone would leave their order arbitrary.

When `--sort` is omitted the default is the first that applies: `relevance` if
you gave a ranker, else `distance` if you gave `--location`, else `date`.

Asking for a sort whose input is missing is an error rather than a silent
fallback:

```
$ videre search --sort distance
error: --sort distance needs --location <place>
```

## What does not compose

**Two rankers.** A text query and `--image` both order results, so at most one:

```bash
videre search "a dog" --image photo.jpg    # error
```

**OR and NOT.** Filters only ever AND. There is no way to ask for "screenshots
or documents", or "anything except memes". Run two searches and combine the
output yourself:

```bash
{ videre search --category screenshot; videre search --category document; } | sort -u
```

**A bare search.** With no ranker and no filter there is nothing to narrow, so
videre asks for at least one rather than returning your whole library. Use
[`videre gallery`](/commands/gallery/) to browse everything.

## Recipes

Clear out old screenshots:

```bash
videre search --category screenshot --before 2024-01-01 -k 1000 > /tmp/old.txt
wc -l /tmp/old.txt          # look before acting
```

One person on one trip:

```bash
videre search --person "Alice" --location "Lisbon" --radius 25 --date 2024-08
```

The biggest videos from a year:

```bash
videre search --date 2023 --sort size -k 20 --scores
```

Everything from one trip, videos included:

```bash
videre search --location "Los Angeles, USA" --radius 30 --date 2024-12 -k 500
```

Since v0.14.0 videos carry their own capture date and coordinates, so a query
like that returns photos and clips together rather than quietly dropping the
video. On one real library it returns 349 photos and 6 videos.

Narrowing the same trip to a single day, then ordering by when things happened:

```bash
videre search --location "Los Angeles, USA" --radius 30 \
  --date 2024-12-12 --sort date:asc
```

Photos of a person, ranked by how well they match a description:

```bash
videre search "at the beach" --person "Alice" --after 2020-01-01
```

Feed a filtered set to another tool:

```bash
videre search --category document --date 2025 --json \
  | jq -r '.results[] | .path'
```

## Caveats

### An empty result is usually correct

Composing filters narrows fast, and zero results normally means the combination
genuinely has nothing, not that something is broken. Two real examples from the
same library:

```bash
videre search --location "Los Angeles, USA" --radius 30 --date 2024-11   # 0
videre search --location "Los Angeles, USA" --radius 30 --date 2024-12   # 355
```

All that library's Los Angeles material was shot in December. Before assuming a
bug, widen one axis at a time: drop the date, or raise `--radius`, and see which
one was doing the excluding.

### A location filter excludes files with no coordinates

`--location` narrows to files that have GPS and fall inside the radius. Files
with no coordinates drop out entirely, so adding a location to a query can cut
the result far more than the geography suggests - screenshots, received images
and photos taken with location services off carry none.

That is correct behaviour rather than a gap: asking for a place is asking for
files known to have been there.

The general rule across every filter is that it matches on the best evidence
available and excludes a file only when there is none. Location, `--person` and
`--category` have no fallback, so files lacking that data drop out. Dates do
have one - every file has a modification time - so nothing is excluded for
want of a capture date; the weaker evidence is used instead.

### Video dates are capture dates

A video matches the month it was **recorded**, not when its file was written.
Copying or re-exporting a clip changes the file timestamp Finder shows while
leaving the capture date alone, so a video shot in December 2024 still answers
to `--date 2024-12` even if its file was created last week.

**Filters make search faster, not slower.** Narrowing happens before ranking, so
the model scores only the survivors. A composed query does less work than an
unfiltered one.

**`-k` applies to everything now.** `--person` and `--category` previously
returned every match; they are now truncated like any other search, and ordered
deterministically. Pass a large `-k` if you want the full set.

**Each filter has its own prerequisite.** `--person` needs
[`videre faces`](/commands/faces/) plus naming, `--category` needs
[`videre classify`](/commands/classify/), a text query needs
[`videre embed`](/commands/embed/), and `--location` needs GPS in the files.
Missing one gives no results rather than an error.

**`--location` reaches the network.** It geocodes the place name once and caches
the answer. It is the only search filter that does.

**Sub-second date precision varies.** Some stored dates carry a timezone offset
and some do not, depending on the source. Comparison is textual, which is exact
at day granularity and what every `--date` form uses. Only hand-written
`--after`/`--before` bounds with a time component can land on the difference.

## Available to agents too

Everything here is exposed through [`videre mcp`](/commands/mcp/) under the same
names, so an assistant can compose the same query. The CLI and the MCP server run
the identical code path, so results cannot diverge between them.
