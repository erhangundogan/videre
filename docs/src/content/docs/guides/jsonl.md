---
title: JSONL output
description: Scan to a text stream instead of a database, and what you give up.
---

[`videre scan`](/commands/scan/) can write one JSON object per line instead of a
database. It is a way to get scan results into other tools without touching
SQLite.

## Two outputs, two jobs

They are not competing formats, and neither is the lesser one:

| | For |
|---|---|
| **SQLite** (`--db`) | the *library*. Everything that accumulates - embeddings, faces, classifications, location clusters - and everything that reads them: `search`, `report`, `dedupe`, the MCP server. |
| **JSONL** (`--output`) | *composability*. One line per file, straight into `jq`, `awk`, a spreadsheet or another program, with no SQLite dependency and no schema to learn. |

Use SQLite when videre is your library. Use JSONL when videre is one step in a
pipeline you are building. A scan writes one or the other, never both, because
`--output` and `--db` are mutually exclusive.

What JSONL gives up is everything that comes *after* a scan: it holds the facts
about each file and nothing else, so no command reads it back.

```bash
videre scan ~/Photos --output              # writes ~/.videre/hashes.jsonl
videre scan ~/Photos --output out.jsonl    # writes a specific file
```

:::caution[A bare `--output` must come after the folder]
`videre scan --output ~/Photos` treats `~/Photos` as the *filename*, because
`--output` takes an optional value. Put the folder first.
:::

## The format

One JSON object per line, appended:

```json
{"path":"/Photos/IMG_0042.jpg","hash":"5c5254e2...","size_bytes":4823921,"created_at":"2021-06-14T09:12:33","modified_at":"2021-06-14T09:12:33","ext":"jpg","mime":"image/jpeg","exif_date":"2021-06-14T09:12:33","gps_lat":52.5163,"gps_lon":13.3777,"width":4032,"height":3024}
```

The fields match the [database columns](/reference/database/) of the same names.
Absent values, such as EXIF on a PNG, are `null` or omitted.

**Appended, not replaced.** Scanning twice to the same file gives two entries per
path. That suits a log, and it means you should not treat the file as a current
snapshot without deduplicating it yourself.

## Working with it

```bash
# every HEIC file
jq 'select(.ext == "heic")' ~/.videre/hashes.jsonl

# duplicate hashes
jq -r '.hash' hashes.jsonl | sort | uniq -d

# total size in GB
jq -s 'map(.size_bytes) | add / 1073741824' hashes.jsonl

# photos with GPS, as CSV
jq -r 'select(.gps_lat) | [.path, .gps_lat, .gps_lon] | @csv' hashes.jsonl

# largest ten
jq -s 'sort_by(-.size_bytes) | .[:10] | .[] | "\(.size_bytes)\t\(.path)"' -r hashes.jsonl
```

Because it is line-delimited, it also streams:

```bash
videre scan ~/Photos --output /dev/stdout --silent | jq -c 'select(.width > 4000)'
```

## What you give up

JSONL is scan output only. Nothing else reads it:

| Command | Works from JSONL? |
|---|---|
| `dedupe`, `report`, `prune`, `stats`, `locations` | No, they need the database |
| `embed`, `faces`, `classify`, `search` | No |
| `scan --retry-incomplete` | No, it needs a database to consult |

There is also no perceptual fingerprint, no faces, no embeddings, and no
resumability. A JSONL scan is a one-shot description of a folder.

## When to use which

**Use the database** for anything you intend to do with videre itself. It is the
default for good reason, and `sqlite3` queries against it are usually easier
than `jq` over JSONL. See [the database](/reference/database/) for the schema and
example queries.

**Use JSONL** when videre is one step in someone else's pipeline: feeding an
inventory into another tool, producing an audit log per run, or working on a
system where you would rather not keep a database.

If you want structured output from other commands, most support `--json`, which
is a single document rather than a stream:

```bash
videre dedupe --json
videre search "sunset" --json
videre stats --json
videre locations --geojson
```

Those are the better choice for scripting, since they describe results rather
than raw scan rows.
