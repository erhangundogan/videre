---
title: Query syntax
description: One search string for filters and text, in the style of Gmail and GitHub search.
---

[`videre search`](/commands/search/) takes a query: words to search for, and
filters, in one string.

```bash
videre search 'person:özgür "gün batımı" -tag:screenshot (tag:deniz OR tag:plaj) rating:>=4'
```

Put the whole query in single quotes. That keeps double quotes and
parentheses inside it in bash, zsh and fish alike; in fish, parentheses
outside quotes run a command. A query that starts with `-` would read as an
option, so put `--` before it, or start with `NOT`:

```bash
videre search -- '-tag:ekran date:2023'
videre search 'NOT tag:ekran date:2023'
```

## Words and filters

- **Words without a key are the text to search for**, ranked by meaning as a
  plain `videre search gün batımı` always was. Quote a phrase to keep it
  together, or to include a `:`, `OR` or `-` in it.
- **`key:value` is a filter.** Quote a value with spaces:
  `person:"Erhan Gündoğan"`, `place:"Kadıköy, İstanbul"`.
- Terms side by side must all match (AND). `OR` between two terms matches
  either. `-` or `NOT` in front of a term excludes it. Parentheses group:
  `tag:deniz (person:özgür OR person:ayşe)`. Without them, every term joined
  by `OR` becomes one alternative and the rest must all match, so
  `person:özgür tag:deniz OR tag:plaj` means Özgür, at deniz or plaj; use
  parentheses whenever that is not what you mean.
- The text searches, it does not filter, so it cannot go inside `OR`, `NOT`
  or parentheses. Use a key instead: `tag:deniz OR tag:plaj`.

## Keys

| Key | Matches | Example |
|---|---|---|
| `person:` | photos of a named person, exactly as [`--person`](/commands/search/) | `person:özgür` |
| `tag:` | files with this tag | `tag:deniz` |
| `category:` | files [classified](/commands/classify/) as this | `category:document` |
| `place:` | files at one of the library's own places, by name, offline | `place:kadıköy` |
| `date:` | a year, month or day | `date:2023`, `date:2023-07` |
| `after:` `before:` | on or after, or before, a date | `after:2023-05-01` |
| `rating:` | at least this many stars, or a range | `rating:4`, `rating:>=4`, `rating:[2 TO 3]` |
| `pick:` | `keep` or `reject` | `pick:keep` |
| `label:` | a colour label | `label:Red` |
| `is:liked` | liked files | `is:liked` |
| `type:` | `image` or `video` | `type:video` |
| `ext:` `mime:` | a file extension, or an exact type | `ext:heic`, `mime:video/quicktime` |
| `path:` | files under a folder | `path:Tatil/2023` |
| `has:` `missing:` | `gps` or `date` present, or not | `missing:gps` |

`place:` matches a place name the library already knows, whole words, ignoring
case and accents: `place:kadikoy` finds "Kadıköy, İstanbul". It never looks a
place up online; for photos near any place on the map, use
[`--location`](/commands/search/), which does.

A file with no date never matches `date:`, `after:` or `before:`, and one with
no GPS never matches `place:`. `missing:date` and `missing:gps` find them.

Wildcards (`den*`), boosts (`^2`) and patterns are not supported and are
refused rather than ignored.

## With flags

The flags still work and narrow together with the query:

```bash
videre search 'tag:deniz OR tag:plaj' --date 2023 -k 50
videre search 'tag:deniz' --image örnek.jpg    # filters with an example image
```

An example image and words in the query both rank the results, so one search
takes one or the other.

## In other commands

`embed`, `faces`, `classify`, `tag`, `mark` and `export` take a query as
`--query`, filters only, to narrow what they work on. See
[Scoping a run](/guides/scoping-a-run/). The MCP server's `search` tool takes
the same language in its `query` parameter.
