---
title: videre gallery
description: Browse your whole library in a local web UI, with every file, the people in them, and a date drill-down.
---

Starts a small web server on your own machine and opens your library in a
browser. Nothing is uploaded, and your media files and library database are not
modified. Gallery previews may populate the derived thumbnail cache while it
reads the database `videre scan` built.

```bash
videre gallery                  # http://127.0.0.1:7878
videre gallery --browse         # ...and open it in your browser
videre gallery --port 8080      # if 7878 is taken
```

Stop it with `Ctrl-C`.

JPEG and other browser-raster previews are resized and cached on first use.
Their embedded EXIF orientation is applied before resizing, so the grid and
lightbox match Finder and other metadata-aware viewers. Later requests, including
files fetched through **Show more**, read the cached preview without reopening or
decoding the original.

On macOS, video grid tiles are a still poster frame (extracted upright through
QuickLook, so a clip that carries rotation is never shown sideways), marked with
a small play badge so a video is distinguishable from a photo; the video plays in
the lightbox when you click it. The poster is cached like the other previews. On
other platforms, where videre has no video frame extraction, tiles remain the
plain inline video, also badged.

Once the lightbox is open, move through the items without closing it: the
on-screen arrows at the left and right edges, or the **Left** and **Right**
arrow keys, step to the previous and next item. Stepping past the last loaded
item pulls in the next page automatically and keeps going, so **Show more** is
not needed to browse to the end. The **Fullscreen** button in the top corner of
the item blows the whole lightbox up to the screen, arrows and info panel
included; the same button (or **Escape**) steps back. **Escape** or a click
outside the item closes the lightbox.

An info panel sits directly under the media: the labeled people in the photo
(each links to that person's page), the place name when the file has GPS, and
the filename, size, and date. It stays visible even when a file has none of the
people or location data yet.

## What is on each page

| Path | What you get |
|------|--------------|
| `/` | Every file, with a **Similar** button on each card once the library has embeddings |
| `/duplicates` | Duplicate groups, the same review `dedupe --html` writes |
| `/people` | Face groups, and naming them |
| `/date` | A Year / Month / Day drill-down |
| `/date/2024`, `/date/2024/09`, `/date/2024/09/26` | The media of that year, month, or day, each with its item count |
| `/map` | Reserved, not built yet |
| `/events` | Reserved, not built yet |
| `/smart` | Reserved, not built yet |

**Files**, **Duplicates**, **Date** and **People** sit in a strip along the top
of every page, so you switch between them without touching the address bar. The reserved routes are
deliberately not in it; each one appears when it renders something.

They link to each other in smaller ways too, which is the point of serving them
together: a face in
the gallery is clickable through to that person's page, and a photo's location
is resolved to a place name while you look at it. Neither works in a file you
open from disk, because both need something running to answer.

## List and Tile views

The **View** selector on the Files and Date tabs switches between **List**, the
default view with file details, and **Tile**, an image-first view without
captions. Tile arranges photos and videos into rows using their stored aspect
ratios; files with no recorded dimensions use a square tile.

Your browser remembers the choice across both tabs and page reloads. Tile rows
adapt when you resize the window or load more files on the Files tab. Click a
tile to open the same lightbox, with the same previous/next navigation.
Thumbnails are served at 480px so tiles stay sharp on high-DPI displays.

## Options

| Flag | What it does |
|------|--------------|
| `--library <DIR>` | Select a different library (default: the current directory) |
| `--model <MODEL>` | Embedding model backing similarity search |
| `--port <PORT>` | Port to listen on (default 7878) |
| `--browse` | Open a browser once the server is listening |

## Gallery, or a file you can keep

`gallery` is for looking around: it does not modify your media or library
database, though it may cache derived previews. When you want to keep or send
what a command just found, ask that command for it:

```bash
videre dedupe --html            # the duplicate groups, as a file
videre search "sunset" --html   # these results, as a file
```

Those write a page you can open later without videre running.

:::note
The server binds to `127.0.0.1`, so it is reachable only from this machine.
:::
