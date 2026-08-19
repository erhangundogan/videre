---
title: videre report
description: Deprecated. Use videre gallery to browse, or ask dedupe and search for a page you can keep.
---

:::caution[Deprecated in 0.18.0]
`videre report` still works and will be removed in the next release. It is
hidden from `videre --help` and prints a warning naming its replacement.
:::

It did two unrelated jobs, and they have been separated.

## Browsing the library

[`videre gallery`](/commands/gallery/) serves every view from one place, and the
views link to each other:

```bash
videre gallery              # then / , /people , /date
videre gallery --browse     # ...and open a browser
```

| You used to run | Now |
|---|---|
| `videre report --all` | `videre gallery`, at `/` |
| `videre report --faces` | `videre gallery`, at `/people` |
| `videre report --by-date` | `videre gallery`, at `/date` |
| `videre report --show-faces` | `videre gallery`. Faces and place names are always on |

`--heic` and `--heic-original` are gone. The server converts HEIC on demand, so
there is nothing to choose: the eager version existed only because a file
written to disk cannot ask for a thumbnail later.

## Keeping a page

[`videre dedupe --html`](/commands/dedupe/) and
[`videre search --html`](/commands/search/) write a file, which is what plain
`videre report` did:

```bash
videre dedupe --html                  # the duplicate groups
videre search "sunset" --html         # these results
```

| You used to run | Now |
|---|---|
| `videre report` | `videre dedupe --html` |
| `videre report -o out.html` | `videre dedupe --html out.html` |

## Flags it still accepts

Unchanged until removal, and each has an equivalent above.

| Flag | Note |
|---|---|
| `--db <DB>` | Same on every command |
| `--output <PATH>` | `videre dedupe --html <PATH>` |
| `--model <MODEL>` | `videre gallery --model <MODEL>` |
| `--all`, `--faces`, `--by-date`, `--show-faces` | See the table above |
| `--heic`, `--heic-original` | No equivalent; the server converts on demand |

## Why

Serving and writing are different jobs. A file cannot show a face you can click
through to a person, or look up a place name while you read it, because both
need something running to answer. Splitting them let the browsing side gain
links between views, and left the writing side to do the one thing a file is
good at: outliving the command.
