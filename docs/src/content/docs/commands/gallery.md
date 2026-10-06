---
title: videre gallery
description: Browse your whole library in a local web UI, with every file, duplicate review, a date drill-down, people, a map, and automatic events.
---

Starts a small web server on your own machine and opens your library in a
browser. Nothing is uploaded. Browsing only reads the database `videre scan`
built (previews may populate the derived thumbnail cache); the library changes
only through what you do on the page: naming people, marking and liking photos,
and face-learning answers are saved to the library database, and rotating a
photo updates its orientation in the file itself.

```bash
videre gallery                  # http://127.0.0.1:7878
videre gallery --browse         # ...and open it in your browser
videre gallery --port 8080      # use exactly this port
```

Stop it with `Ctrl-C`.

## It keeps the library current

The gallery starts [`videre watch`](/commands/watch/) beside it, so files you
add, change or remove while browsing are scanned and processed without a
second terminal. [`videre status`](/commands/status/) shows what it is doing,
and it logs to `.videre/logs/watch.log`. The watch stops when the gallery
does, and if the gallery is killed without the chance to stop it, the watch
notices within a second or two and stops itself.

When a watch is already running for the library, one you started yourself for
example, the gallery says so and uses it, and leaves it running when the
gallery stops.

The watch is a normal `videre watch`, so it does that command's work,
including fetching the face model once if it has never been downloaded. On a
library that was never processed, opening the gallery therefore starts the
whole scan, faces and embed pipeline in the background. To browse without it:

```bash
videre config set gallery-starts-watch false
```

Nothing depends on the watch being there: every change you make on the page
is complete when it returns, and anything a watch would redo is left for the
next [`videre embed`](/commands/embed/), [`videre faces`](/commands/faces/)
or `videre watch`.

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
buttons: **Rotate** (photos only) turns the image 90 degrees by editing its
orientation in place, never re-encoding the pixels, and turns the photo's face
boxes with it so face crops stay on their faces; **Fullscreen** blows the whole lightbox up to
the screen, arrows and info panel included (the same button, or **Escape**,
steps back); and **Close** (the &#215; at its right) dismisses it. The close
button, **Escape**, or a click outside the item all close the lightbox.

JPEG, PNG, TIFF and WebP rotate by their EXIF orientation. A HEIC rotates by
its rotation property, the one iPhones write and Photos and Preview display
by, and its EXIF orientation is kept in step. A HEIC saved without that
property cannot be rotated here, so the request is refused and the file is
left untouched. A rotation changes what the photo shows but not its identity,
so everything videre worked out from the old orientation is dropped at once:
cached previews are rendered again, and the photo's search embedding and
category are redone by the next [`videre embed`](/commands/embed/) or
[`videre watch`](/commands/watch/). Nothing needs clearing by hand.

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
the filename, size, and date. The button beside the filename copies it. It
stays visible even when a file has none of the people or location data yet.

## What is on each page

| Path | What you get |
|------|--------------|
| `/` | Every file, with a **Similar** button on each card once the library has embeddings (the grid under the map has it too) |
| `/search?like=<hash>`, `/search?q=...` | A ranking as a page of its own: files like one, or matching words, within the query's filters. **Open as a page** on a Similar or text-results strip leads here, and a bookmark reopens it. **Show more** continues the ranking |
| `/duplicates` | Duplicate groups, the same review `dedupe --html` writes, plus Google Takeout edits beside their originals ([`dedupe --edited`](/commands/dedupe/#google-photos-edits---edited)), counted in the header apart from exact copies |
| `/people` | Face groups, and naming them. Singletons load as you scroll; naming one keeps your place |
| `/date` | A Year / Month / Day drill-down |
| `/date/2024`, `/date/2024/09`, `/date/2024/09/26` | The media of that year, month, or day, each with its item count; **Show more** loads the next page |
| `/map` | Location clusters plotted on a world map, with the full file grid below; click a cluster to see its photos |
| `/map/location/berlin?radius=25` | An addressable location drill-down with a proximity radius in kilometers |
| `/events` | Substantial travel trips inferred from capture dates and photo locations; click one to see its media |
| `/events/20200312T100000-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa` | One trip's media, keyed by its first photo anchor's time and full content hash |
| `/smart` | Reserved, not built yet |

**Library**, **Duplicates**, **Date**, **Events**, **People** and **Map** sit in a strip along
the top of every page, so you switch between them without touching the address
bar. The reserved routes are deliberately not in it; each one appears when it
renders something.

Each section's toolbar sits under the strip and stays there as you scroll. It
reads, left to right: **View**, **Sort**, then a **search and filter** box
with its **Filter** button, then the section's own controls, each set apart by
a divider, and **Select** at the right end. The box takes a
[query](/reference/query-syntax/) and applies it to that section (below). On
a page with no toolbar, such as a person's page or Settings, the box sits at
the right of the strip instead and opens the Library page with the query:

- **Filters narrow the grid.** `tag:deniz OR tag:plaj`, `person:özgür
  date:2023` or `-tag:ekran` leave only the matching files, with a line above
  the grid saying how many of the library's files match, and a **clear** link.
- **Words rank.** Words without a key rank the files the filters left, by
  meaning, in the results strip above the grid: the same ranking the
  **Similar** button uses, so they need a library with embeddings. Without
  embeddings the box is offered as a filter.
- **A query that cannot run says so.** The line above the grid shows the error
  and the character it failed at, instead of an unfiltered grid.

The query stays in the address bar as `?q=...`, so a reload or a bookmark
opens the same view. Other pages take it too:

- **Date**: years, months and days count only the matching files, a period
  with none is left out, and its links keep the query.
- **Map**: only the places holding a matching file, counted by those files;
  the grid below lists the matching files.
- **Events**: trips are still found over the whole library, so a query never
  splits one; it shows the trips with a matching file, and a trip's page shows
  only those files.
- **Duplicates**: the groups with a matching copy, shown whole, since the page
  is for choosing among a group's copies.
- **People**: the people, clusters and faces seen in a matching file.

On these pages the box applies the query where you are, and the strip's links
to them carry it. Words rank only on the Library page, so a query with words,
or one typed on a person's or a cluster's own page, opens Library.

In the box, each filter is a **chip**, and words stay as text:

- **Suggestions** open as you type: keys, then the library's own values for
  the key (people with their face, tags, places, categories, labels, types),
  most used first, matching case and accents loosely, so `gündo` finds Erhan
  Gündoğan. Arrow keys move, Enter or Tab takes one. A value becomes a chip;
  Enter on its own runs the query.
- **A second value of the same key joins its chip.** For `person:` and `tag:`
  it starts as **all** (both in the photo); for keys a file has one of, such
  as `type:` or `place:`, as **any**. The chip's any/all button switches it.
- **not** on a chip excludes it, **×** removes it, and both apply at once.
  Clicking a chip's text, or Backspace in an empty box, takes it back into
  the box to edit.
- A part of the query that chips cannot show, such as `tag:deniz OR
  person:ayşe`, stays one chip with its text unchanged.
- **Filter** beside the box opens the options: a column for each kind of filter
  (people, a date range, places, tags, rating and marks, type and extension,
  category, and what a file has or is missing), filled with this library's
  values and their counts. Choosing a value writes the same chip typing it
  would, choosing it again takes it out, and **Apply** runs the query.
  `rating:4` means four stars or more.
- The **×** at the end of the box clears every chip and the words at once.

The tall library header (the database path and the scanned-file counts) shows on
the Library page and on a static export; the other sections drop it, since the strip
already says where you are. The Duplicates page keeps a header of its own while it
has duplicates: the number of duplicate groups, the files in them, and the space
the extra copies take.

A liked file wears a red heart at the bottom left of its thumbnail. It only
shows the state: the heart in the lightbox is the one that likes and unlikes.

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
  "faces": { "learning": false, "learningUpdates": false },
  "routes": {
    "files": {
      "view": "tile",
      "pageSize": 200,
      "sort": { "field": "date", "dir": "desc" },
      "tile": { "rowHeight": 280, "colGap": 10, "rowGap": 10 }
    },
    "date": { "pageSize": 200 },
    "search": { "pageSize": 96 },
    "events": { "sort": { "field": "date", "dir": "desc" } },
    "people": { "align": "right", "reclusterOpen": false, "pageSize": 200 },
    "duplicates": { "pageSize": 100 },
    "map": { "radiusKm": 0, "sort": { "field": "date", "dir": "desc" } }
  }
}
```

| Setting | Values | What it does |
|---|---|---|
| `resume.route` | a page path | The page the gallery reopens at. Saved as you move around; see below |
| `faces.learning` | `true`, `false` | Whether face learning runs for this library; see [Face learning](#face-learning) |
| `faces.learningUpdates` | `true`, `false` | Whether the People page shows face learning's status strip and teaching notes, while learning is on |
| `routes.files.view` | `tile`, `list` | The file view on the Library, Date, Events and Map grids |
| `routes.files.pageSize` | a whole number, 1 to 500 | How many files the Library and Map grids load at a time, and with each **Show more** |
| `routes.date.pageSize` | a whole number, 1 to 500 | The same for a day, month or range in the Date view |
| `routes.search.pageSize` | a whole number, 1 to 200 | How many results a search page shows at a time, and with each **Show more** |
| `routes.people.pageSize` | a whole number, 1 to 1000 | How many faces the People page's singletons and a person's page load at a time, as you scroll |
| `routes.duplicates.pageSize` | a whole number, 1 to 1000 | How many duplicate groups the Duplicates page shows at a time, and with each **Show more** |
| `routes.files.tile.rowHeight` | 80 to 1000 | Target height of a tile row, in pixels |
| `routes.files.tile.colGap` | 0 to 100 | Space between tiles in a row, in pixels |
| `routes.files.tile.rowGap` | 0 to 100 | Space between tile rows, in pixels |
| `routes.people.align` | `right`, `top` | Where the People list sits on the People page |
| `routes.people.reclusterOpen` | `true`, `false` | Whether the People page's Recluster row is open |
| `routes.map.radiusKm` | 0 to 20000 | Radius a map location opens with, in km. `0` uses each place's own radius |
| `routes.files.sort.field` | `date`, `name`, `size`, `rating`, `liked`, `type` | What the Library and Date grids and an event's files are ordered by; see [Sorting](#sorting) |
| `routes.files.sort.dir` | `asc`, `desc` | Which way that order runs |
| `routes.map.sort.field`, `routes.map.sort.dir` | as `routes.files.sort` | The same for the Map grid |
| `routes.events.sort.field` | `date`, `files`, `length`, `name` | What the Events overview orders trips by |
| `routes.events.sort.dir` | `asc`, `desc` | Which way that order runs |

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
bar opens a menu: **Help** opens this page in a new tab, and **Settings** leads
to a page that shows the file's
location, has a **Page sizes** box for each paged view (a value outside its
range is refused and not saved), and can:

- **Export settings** to `videre-gallery-settings.json`. Only the `routes`
  section is exported; the page a library reopens at belongs to that library.
- **Import settings** from such a file, into this or any other library.
- **Reset to defaults**, clearing every saved choice except where the library
  reopens.

Copying `.videre/gallery.json` into another library's `.videre` folder works
too.

## Selecting

**Select**, at the right end of the toolbar of the Library, Date, Events and Map
pages, turns on select mode ("Select enabled", beside it, so nothing else in the
toolbar moves). While it is on:

- a click on a photo or video selects it instead of opening it;
- **Shift-click** selects everything between the last click and this one;
- **Escape** clears the selection; **Select** again leaves select mode.

As soon as something is selected, a bar along the bottom (the same one the
People page uses for singletons) shows how many, and acts on all of them:

| Control | Does |
|---|---|
| **Heart** | likes every selected item; when all are already liked, unlikes them |
| **Star** | 1 to 5 stars, or clear the rating |
| **Colour** | a colour label, or none |
| **Tag** | adds or removes the tag typed in the field, which suggests the library's tags; Enter adds |
| **Flag** | Keep or Reject; choosing one every item already has clears it, and Clear removes either |
| **Rotate left / right** | photos only; videos and RAW files are skipped |
| **More** (⋯) | Copy paths: the selected files' paths, one per line |
| **Delete** (trash, last) | moves the files to the system Trash, after a confirmation |

Each action's result appears briefly above the bar. Escape closes an open menu
first, then clears the selection.

Marks and tags belong to the content, so every copy of a photo changes with
it, exactly as with [`videre mark`](/commands/mark/) and
[`videre tag`](/commands/tag/). Delete moves **every copy** of each selected
item to the Trash (the confirmation counts them), each with its XMP sidecar,
and the library stops
listing them at once; their marks, tags and faces stay until
[`videre prune`](/commands/prune/). A Delete is undone with
[`videre dedupe --undo`](/commands/dedupe/#undoing---trash), which puts back
the most recent trash run, from the gallery or from `dedupe --trash`. Prune also withdraws face-learning evidence
that depended on those faces, while keeping the historical journal and person
names. It waits until no other videre command or
`watch` stage is working on the library.

Select mode needs the running gallery: a static export has no Select button.

## Sorting

In the toolbar on the Library, Date and Map tabs and on an event's page, next
to View, the **Sort** selector orders the files: Date (the default, newest
first), Name, Size, Rating, Liked, or Type. The arrow button beside it flips
the direction: triangle up is ascending, and while it reads pressed it points
down for descending. Files with no date and unrated files always sort last.
The choice is saved in the library's [settings](#settings) and applies to both
list and tile views, so the lightbox's previous/next follows the same order.
The Map keeps a choice of its own.

On the Events overview, Sort orders the trips instead: Date (when the trip
started, the default, newest first), Files, Length, or Name. The duplicates
page keeps its own Sort by control.

## Options

| Flag | What it does |
|------|--------------|
| `--library <DIR>` | Select a different library (default: the current directory) |
| `--model <MODEL>` | Embedding model backing similarity search |
| `--port <PORT>` | Port to listen on. Omitted: start at 7878 and advance to the next free port if taken. Given: use exactly that port (fails if busy); `0` lets the OS choose |
| `--browse` | Open a browser once the server is listening, at the page you last visited in this library |

## Face learning

Face learning is **off by default**. Turn it on for a library in
`.videre/gallery.json`:

```json
{ "faces": { "learning": true } }
```

Off, naming, moving and dissolving work exactly the same but record no
teaching evidence, no training runs, and the People page asks no identity
questions. Evidence recorded while it was on is kept, and training resumes
from it when it is turned back on. The change applies without restarting the
gallery.

When on, the People page teaches the gallery: naming clusters, moving faces, and
dissolving bad groups write durable teaching evidence where there is a
comparison to record (an action with nothing to compare against records
none), and a background worker turns that evidence into small interpretable
scorers. Once a scorer passes the
shipped gates, the People page asks bounded yes/no identity questions.
Answering Yes names a cluster; No only teaches; Skip does neither.

- The status strip and the teaching notes after each action are hidden by
  default: they describe the process, not a result, so they stay out of the
  way. Set `"learningUpdates": true` beside `"learning"` to see them. The
  status is also machine-readable at
  `GET /api/face-learning/status`, and training outcomes (promoted,
  rejected with the check it missed, waiting for feedback) go to the gallery
  log at the `info` level. Identity questions show either way: they are the
  result.
- Training runs in the background on its own database connection, ten
  seconds after the last teaching action, so browsing and naming never wait
  for it.
- The Recluster row (below) says in one line what learning contributes right
  now: which profile suggests names, or that it is not used yet and why.
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
  questions only; grouping comes from the clustering values below.
  `videre faces --reset` wipes all of it.

### Recluster

**Recluster** in the People toolbar opens a form with the grouping values of
[`videre faces`](/commands/faces/#clustering-parameters), one per line:
`eps`, merge, attach and minimum group size, with the quality gates (minimum
face size, sharpness, and the two legacy gates) under **More**. Each has an
**i** after its name that says what it does, which way to move it, and its
default; hover, focus or tap it. The form starts from the values this library
uses now, and a blank or out-of-range value is refused with a message before
anything runs.

- **Preview** computes the grouping those values would produce and changes
  nothing: how many groups, from how many unnamed faces, how many stay single
  and how many the quality gates held out, next to the current numbers.
- **Apply** regroups the unnamed faces and refreshes the identity questions,
  so faces that now sit in a group can be asked about. Named faces never
  move. It saves the values that differ from the defaults to
  `.videre/gallery.json`, and from then on `videre faces` and `videre watch`
  group this library with them (a `videre faces` flag still wins for that
  run).
- **Defaults** fills in the built-in values; apply them to drop the saved ones.

Apply refuses while a `videre faces` or `videre watch` run is working on the
library, and records its run like one, so [`videre status`](/commands/status/)
shows it.

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
