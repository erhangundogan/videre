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
videre gallery --port 8080      # use exactly this port
```

Stop it with `Ctrl-C`.

Without `--port`, `videre gallery` starts at 7878 and, if that is taken,
advances to the next free port (7879, then 7880, ...), printing the address it
actually bound. So a second `videre gallery` for another library just works,
no ports assigned by hand. An explicit `--port` is used exactly and fails if
that port is busy; `--port 0` lets the operating system pick a free port.

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
not needed to browse to the end. At the item's top-right corner sit up to three
buttons: **Rotate** (photos only) turns the image 90 degrees clockwise by
editing its EXIF orientation in place, and turns the photo's face boxes with it
so face crops stay on their faces; **Fullscreen** blows the whole lightbox up to
the screen, arrows and info panel included (the same button, or **Escape**,
steps back); and **Close** (the &#215; at its right) dismisses it. The close
button, **Escape**, or a click outside the item all close the lightbox.

The preview shown is a downscaled render, so **click the photo** to zoom in and
**drag** to pan: the full-resolution original is loaded on the first zoom, so you
can inspect it up close. Click again to return to fit. While zoomed, the
**scroll wheel** pans the enlarged image; it does not zoom.

In the info panel, the **date** links to that day's view and the **place** links
to the map, which selects the location cluster nearest the photo (the place name
is finer than any cluster, so the map resolves it by the photo's coordinates), so
one click jumps from a photo to everything else taken then or there.

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
| `/map` | Location clusters plotted on a world map, with the full file grid below; click a cluster to see its photos |
| `/map/location/berlin?radius=25` | An addressable location drill-down with a proximity radius in kilometers |
| `/events` | Reserved, not built yet |
| `/smart` | Reserved, not built yet |

**Files**, **Duplicates**, **Date**, **People** and **Map** sit in a strip along
the top of every page, so you switch between them without touching the address
bar. The reserved routes are deliberately not in it; each one appears when it
renders something. At the right of the strip is a **search** box: type a
natural-language query and it ranks the library semantically on the Files page,
the same ranking the **Similar** button uses. It appears only when the library
has embeddings to rank against.

The tall library header (the database path and the scanned-file counts) shows on
the Files page and on a static export; the other sections drop it, since the strip
already says where you are.

The Map view begins with clusters grouped by continent. Zoom in to see each
location cluster, then click one to open an addressable drill-down. Route names
use the normalized place name, such as `berlin`. The radius starts at the value
stored when the cluster was created. Changing it updates the URL and grows or
shrinks the grid to include every GPS-bearing file within that exact distance,
regardless of its original cluster membership. The **Map > Berlin** breadcrumb,
**Clear**, **Escape**, or zooming back to the world returns to the unselected
map and the full grid.

The map renders real coastlines and borders with
[MapLibre GL JS](https://maplibre.org) over an offline vector basemap built from
OpenStreetMap data, downloaded once on first use and stored in the shared cache
(see [Install](/start/install/)); `© OpenStreetMap contributors` is shown on the
map. Your location clusters and every drill-down are drawn from the local
library database, and once the basemap is present the view makes no outbound map
or tile requests. On a machine without working WebGL the map falls back to a
self-drawn plot with the same clusters, drill-down, and grid.

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
| `--port <PORT>` | Port to listen on. Omitted: start at 7878 and advance to the next free port if taken. Given: use exactly that port (fails if busy); `0` lets the OS choose |
| `--browse` | Open a browser once the server is listening |

## Face learning

The People page teaches the gallery: naming clusters, moving faces, and
dissolving bad groups write durable teaching evidence, and a background
worker turns it into small interpretable scorers. Once a scorer passes the
shipped gates, the People page asks bounded yes/no identity questions.
Answering Yes names a cluster; No only teaches; Skip does neither.

- Status is machine-readable at `GET /api/face-learning/status`; the page
  shows it as up to date, feedback pending, training, or failed.
- Every action's evidence stays inspectable (per person, and in the teaching
  journal), and no raw embeddings ever appear in a payload.
- Failed runs keep the previous profile. Promotion affects suggestions and
  questions only; grouping itself still comes from the deterministic
  pipeline until a future recluster integration. `videre faces --reset`
  wipes all of it.

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
