---
title: videre gallery
description: Browse your whole library in a local web UI, with every file, duplicate review, a date drill-down, people, a map, and automatic events.
---

Starts a small web server on your own machine and opens your library in a
browser. Nothing is uploaded. Browsing only reads the database `videre scan`
built (previews may populate the derived thumbnail cache); the library changes
only through what you do on the page: naming people, marking and liking photos,
and face-learning answers are saved to the library database, and rotating a
photo updates its orientation tag in the file itself.

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
| `/events` | Substantial travel trips inferred from capture dates and photo locations; click one to see its media |
| `/events/20200312T100000-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` | One trip's media, keyed by its first photo anchor's time and full content hash |
| `/smart` | Reserved, not built yet |

**Library**, **Duplicates**, **Date**, **Events**, **People** and **Map** sit in a strip along
the top of every page, so you switch between them without touching the address
bar. The reserved routes are deliberately not in it; each one appears when it
renders something. At the right of the strip is a **search** box: type a
natural-language query and it ranks the library semantically on the Library page,
the same ranking the **Similar** button uses. It appears only when the library
has embeddings to rank against.

The tall library header (the database path and the scanned-file counts) shows on
the Library page and on a static export; the other sections drop it, since the strip
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

The Events view currently finds travel trips, not ordinary local outings. It
infers home from the place with the most assignable media in your library, then
looks for substantial activity in another place. A short trip needs at least
ten distinct media files in a rolling three-hour window, which may cross
midnight, including three located photos. A multi-day trip needs at least ten
files and two located photos on each of at least two dates. Nearby stops within
one place group can form one trip. A stop outside its 20 km radius starts a
separate candidate and must qualify on its own; a home photo or more than 72
hours between destination photo anchors also ends a trip. A place group's
observed footprint can be compact or reach up to 20 km from its fixed center.
A group overlapping home is
not treated as travel. The ten-item, three-hour, and 20 km values are initial
defaults; a later Gallery configuration will make them editable.

Photos with valid GPS and a full embedded capture date establish the trip.
Dated videos and GPS-less photos can join when nearby photo and location
evidence supports their placement, but they cannot start or extend it. Events
uses the scanner's stored photo capture time or video container creation time,
in wall-clock order, without a filesystem-date fallback. A file with only a
modification date does not join automatically. Trips are recomputed from the
library on demand: neither `videre locations` nor `videre embed` is required.
Each card shows an offline place-based title, date range and file count; click
it to see exactly the included files. If the evidence is too thin, Events
explains why instead of showing one-file cards.

Local outings near home, such as a museum opening, and manual add/remove of
trip files are planned for later iterations. A later iteration will also let
you combine events into a curated event and organize their media together. An
automatic trip can miss files without strong time or place evidence, and a
heavily photographed second routine area can look like a destination until home
correction is available.

They link to each other in smaller ways too, which is the point of serving them
together: a face in
the gallery is clickable through to that person's page, and a photo's location
is resolved to a place name while you look at it. Neither works in a file you
open from disk, because both need something running to answer.

## List and Tile views

The **View** selector on the Library, Date, and Events tabs switches between
**Tile**, the default image-first view without captions, and **List**, which
shows file details. Tile arranges photos and videos into rows using their
stored aspect ratios; files with no recorded dimensions use a square tile.

The choice is saved in the library's [settings](#settings), so it applies to
every tab, survives reloads, and stays with this library whichever port the
gallery runs on. Tile rows adapt when you resize the window or load more files
on the Library tab. Click a tile to open the same lightbox, with the same
previous/next navigation. Thumbnails are served at 480px so tiles stay sharp on
high-DPI displays.

## Settings

The gallery keeps its settings per library, in `.videre/gallery.json` inside
the library folder. The file holds only what differs from the defaults: the
View selector, the People layout toggle and the page you were on save
themselves as you use the gallery, and anything else can be set by editing the
file. A change applies on the next page load, with no restart. Choosing a
default again removes that setting from the file, so a default changed in a
later release still reaches this library.

These are the defaults every library starts from:

<!-- gallery-defaults -->
```json
{
  "resume": { "route": "/" },
  "routes": {
    "files": {
      "view": "tile",
      "pageSize": 200,
      "sort": { "field": "date", "dir": "desc" },
      "tile": { "rowHeight": 280, "colGap": 10, "rowGap": 10 }
    },
    "people": { "align": "right" },
    "map": { "radiusKm": 0 }
  }
}
```

| Setting | Values | What it does |
|---|---|---|
| `resume.route` | a page path | The page the gallery reopens at. Saved as you move around; see below |
| `routes.files.view` | `tile`, `list` | The file view on the Library, Date, Events and Map grids |
| `routes.files.pageSize` | a whole number, 1 to 500 | How many files the Library and Map grids load at a time, and with each **Show more** |
| `routes.files.tile.rowHeight` | 80 to 1000 | Target height of a tile row, in pixels |
| `routes.files.tile.colGap` | 0 to 100 | Space between tiles in a row, in pixels |
| `routes.files.tile.rowGap` | 0 to 100 | Space between tile rows, in pixels |
| `routes.people.align` | `right`, `top` | Where the People list sits on the People page |
| `routes.map.radiusKm` | 0 to 20000 | Radius a map location opens with, in km. `0` uses each place's own radius |

`routes.files.sort` is reserved for the upcoming sort control and is not read
yet.

A value of the wrong type (a word where a number belongs) or out of range is
ignored and the default used instead. Keys the gallery does not know are kept
in the file but have no effect. If the file is not valid JSON, the gallery runs
on the defaults, shows a banner saying why, and saves nothing until you fix or
delete the file, so hand edits are never overwritten.

**Reopening where you left off.** The address `videre gallery` prints, and the
page `--browse` opens, is the last page you visited in that library, such as a
day in the Date view or a place on the map. Opening the bare address still
lands on the Library tab.

**The settings page.** The **...** button at the right end of the navigation
bar opens a menu; **Settings** there leads to a page that shows the file's
location and can:

- **Export settings** to `videre-gallery-settings.json`. Only the `routes`
  section is exported; the page a library reopens at belongs to that library.
- **Import settings** from such a file, into this or any other library.
- **Reset to defaults**, clearing every saved choice except where the library
  reopens.

Copying `.videre/gallery.json` into another library's `.videre` folder works
too.

## Options

| Flag | What it does |
|------|--------------|
| `--library <DIR>` | Select a different library (default: the current directory) |
| `--model <MODEL>` | Embedding model backing similarity search |
| `--port <PORT>` | Port to listen on. Omitted: start at 7878 and advance to the next free port if taken. Given: use exactly that port (fails if busy); `0` lets the OS choose |
| `--browse` | Open a browser once the server is listening, at the page you last visited in this library |

## Face learning

The People page teaches the gallery: naming clusters, moving faces, and
dissolving bad groups write durable teaching evidence where there is a
comparison to record (an action with nothing to compare against records
none), and a background worker turns that evidence into small interpretable
scorers. Once a scorer passes the
shipped gates, the People page asks bounded yes/no identity questions.
Answering Yes names a cluster; No only teaches; Skip does neither.

- Status is machine-readable at `GET /api/face-learning/status`; the page
  shows it as up to date, feedback pending, training, waiting for more
  feedback, or failed. When up to date, it also says whether the last trained
  candidate was promoted to the profile in use or rejected by the quality
  gates.
- Training needs both kinds of feedback: faces that belong together (naming
  people) and groups that are wrong (dissolving a cluster). Until there is
  enough of each, the page shows what it is waiting for, such as *dissolve 2
  more wrong clusters* or *name 1 more person from a group of two or more
  faces*, rather than a failure. A person named from a single face counts as
  a name but teaches nothing yet (there is no second face to compare it
  with), so it does not start a new run; naming a group, or adding a face to
  someone already named, does. After an upgrade, the gallery also tries once
  more when it starts, since a newer videre may train on the same feedback.
- Every action's evidence stays inspectable (per person, and in the teaching
  journal), and no raw embeddings ever appear in a payload.
- Failed runs keep the previous profile. Promotion affects suggestions and
  questions only; grouping itself still comes from the deterministic
  pipeline until a future recluster integration. `videre faces --reset`
  wipes all of it.

## Gallery, or a file you can keep

`gallery` is for looking around and curating while it runs. When you want to
keep or send what a command just found, ask that command for it:

```bash
videre dedupe --html            # the duplicate groups, as a file
videre search "sunset" --html   # these results, as a file
```

Those write a page you can open later without videre running.

:::note
The server binds to `127.0.0.1`, so it is reachable only from this machine.
:::
