---
title: Quickstart
description: Scan a folder, then clean up duplicates, search by description, and find people.
sidebar:
  order: 2
---

Start here. Everything else reads from what this creates.

```bash
videre scan ~/Photos
```

That builds a database at `~/.videre/hashes.db` describing what you have. It
does not change your photos.

It also remembers `~/Photos` as your default folder, so later commands can be
run without repeating it. It says so when it happens.

## Clean up duplicates

```bash
videre dedupe                 # list which copies could go
videre report                 # ...or review them visually in a browser first
videre dedupe | xargs trash   # delete them
videre prune                  # tidy the database afterwards
```

`videre dedupe` never deletes anything itself. It prints a list for you to
check. Add `--similar` to also flag photos and videos that merely *look* alike;
those are reported for review only, never included in the delete list.

:::caution
`videre dedupe | xargs trash` deletes immediately. Look before you pipe. See
[cautions](/start/cautions/).
:::

## Search your photos

```bash
videre embed                              # one-time: prepares photos for search
videre search "golden gate bridge at sunset"
videre search --image reference.jpg       # find photos like this one
```

The first `videre embed` downloads about 780 MB of model data (Google's SigLIP,
from Hugging Face; see [what gets downloaded](/start/install/#models-are-not-downloaded-at-install))
and takes a while on a big library. You can stop it at any point and rerun
later, and it picks up where it left off.

## Find people

```bash
videre faces                  # detect and group faces
videre report --faces         # name the groups in your browser
videre search --person "Alice"
```

`videre faces` downloads about 180 MB the first time.

## Other things it can do

```bash
videre classify                        # tag screenshots/documents/memes
videre search --category screenshot

videre locations                       # group photos by place
videre search --location "Berlin"

videre fix-dates                       # set file dates from EXIF
videre report --all                    # browse the whole library
videre stats                           # what's in the library
videre watch ~/Photos                  # keep everything fresh in the background
```

## Working with other tools

`videre dedupe` prints one file path per line, so it pipes into anything:

```bash
videre dedupe | xargs trash
videre dedupe > to-delete.txt
```

`videre search`, `videre dedupe`, `videre stats` and `videre locations` accept
`--json` for scripting, and [`videre mcp`](/commands/mcp/) exposes search and
duplicate review to AI agents over stdio:

```json
{
  "mcpServers": {
    "videre": { "command": "/path/to/videre", "args": ["mcp"] }
  }
}
```

## Good to know

- Long jobs (`embed`, `faces`, `classify`) are resumable. Ctrl-C is safe, and
  rerunning continues where it stopped.
- Two different videre commands can run at once against the same database.
  Running the *same* command twice is refused rather than allowed to corrupt
  anything.
- `videre report --faces` and `--show-faces` start a local web server on
  `localhost:7878`. Nothing leaves your machine.
- The only feature that touches the network is `videre search --location`, which
  looks up a place name once and caches the result.
