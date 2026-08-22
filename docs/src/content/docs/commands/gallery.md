---
title: videre gallery
description: Browse your whole library in a local web UI, with every file, the people in them, and a date drill-down.
---

Starts a small web server on your own machine and opens your library in a
browser. Nothing is uploaded and nothing is written: it reads the database
`videre scan` built.

```bash
videre gallery                  # http://127.0.0.1:7878
videre gallery --browse         # ...and open it in your browser
videre gallery --port 8080      # if 7878 is taken
```

Stop it with `Ctrl-C`.

## What is on each page

| Path | What you get |
|------|--------------|
| `/` | Every file, with in-page similarity search |
| `/people` | Face groups, and naming them |
| `/date` | A Year / Month / Day drill-down |
| `/map` | Reserved, not built yet |
| `/events` | Reserved, not built yet |
| `/smart` | Reserved, not built yet |

**Files**, **Date** and **People** sit in a strip along the top of every page, so
you switch between them without touching the address bar. The reserved routes are
deliberately not in it; each one appears when it renders something.

They link to each other in smaller ways too, which is the point of serving them
together: a face in
the gallery is clickable through to that person's page, and a photo's location
is resolved to a place name while you look at it. Neither works in a file you
open from disk, because both need something running to answer.

## Options

| Flag | What it does |
|------|--------------|
| `--db <DB>` | SQLite database (default: resolved from `~/.videre`) |
| `--model <MODEL>` | Embedding model backing the in-page similarity search |
| `--port <PORT>` | Port to listen on (default 7878) |
| `--browse` | Open a browser once the server is listening |

## Gallery, or a file you can keep

`gallery` is for looking around, and writes nothing. When you want to keep or
send what a command just found, ask that command for it:

```bash
videre dedupe --html            # the duplicate groups, as a file
videre search "sunset" --html   # these results, as a file
```

Those write a page you can open later without videre running.

:::note
The server binds to `127.0.0.1`, so it is reachable only from this machine.
:::
