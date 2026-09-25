---
title: Gallery HTTP interface
description: The local HTTP routes served while videre gallery is running.
---

`videre gallery` starts a small HTTP server on your own machine. The gallery
pages use that server to fetch rows, thumbnails, face data, marks and people
updates while you browse.

The interface is local. It binds to `127.0.0.1`, exists only while
`videre gallery` is running, and reads or writes the database chosen when the
server started. You can call it from your own local tools, but it is not a
hosted service and should not be exposed to other machines.

```bash
videre gallery --browse
videre gallery --port 8080
```

The examples below use `http://127.0.0.1:7878`, the default address. Without
`--port` a busy 7878 advances to the next free port, so check the startup line
before assuming `7878`; an explicit `--port` is used exactly.

:::note
Static HTML exports from commands such as `videre dedupe --html` and
`videre search --html` are separate files. They do not keep this server running
and cannot answer these HTTP requests after the command exits.
:::

## Page routes

| Route | What it serves |
|---|---|
| `GET /` | All files |
| `GET /duplicates` | Duplicate review |
| `GET /people` | People and face labeling |
| `GET /date` | Date drill-down |
| `GET /date/{year}` | Files for one year |
| `GET /date/{year}/{month}` | Files for one month |
| `GET /date/{year}/{month}/{day}` | Files for one day |
| `GET /people/cluster/{id}` | One face cluster |
| `GET /people/person/{name}` | One person |
| `GET /map` | Location cluster map with the full file grid |
| `GET /map?near={lat},{lon}` | Map centred on the cluster nearest a coordinate pair (used by the lightbox place link) |
| `GET /map/location/{name}` | One addressable location drill-down |
| `GET /events` | Automatic travel-trip overview |
| `GET /events/{key}` | One trip's media (`key` is its first photo anchor's compact time and full hash) |
| `GET /settings` | Gallery settings: import, export and reset |
| `GET /smart` | Reserved |

Date route segments are zero-padded where applicable: `YYYY`, `YYYY/MM` and
`YYYY/MM/DD`. Invalid date routes return `404 Not Found`.

`GET /date` also accepts range query parameters:

| Query | Meaning |
|---|---|
| `from=YYYY[-MM[-DD]]` | Inclusive lower bound |
| `to=YYYY[-MM[-DD]]` | Exclusive upper bound |

Partial bounds expand to the first day of the period. For example,
`from=2025` starts at `2025-01-01`, and `to=2018` stops before `2018-01-01`.
When `from` is present without `to`, the range runs through today. Invalid
query dates or empty ranges return `400 Bad Request`.

```bash
curl "http://127.0.0.1:7878/date/2025/06/03"
curl "http://127.0.0.1:7878/date?from=2016-05&to=2017"
```

The reserved `/smart` route currently returns a placeholder page with
`404 Not Found`.

Map location names use the same normalized, URL-safe identity rule as people.
When more than one cluster has the same normalized name, the route selects the
largest cluster, then the lowest cluster id. The optional positive `radius`
query is measured in kilometers; without it the route uses the library's
`routes.map.radiusKm` setting when it is above 0, and otherwise the cluster's
stored radius. An unknown name still returns the working map page with HTTP 200 and an
unknown-location state.

`GET /map?near={lat},{lon}` takes a `"lat,lon"` pair instead of a name and
resolves it to the cluster whose centroid is nearest by haversine distance,
bootstrapping that cluster's drill-down. The lightbox place link uses it because
a photo's reverse-geocoded place name is finer than any cluster name; the client
then rewrites the address bar to the resolved `/map/location/{name}`. A library
with no clusters resolves to the unselected map.

## Settings

The library's gallery settings, stored in `.videre/gallery.json`. See
[settings](/commands/gallery/#settings) for the keys and defaults. Every
response has the same shape:

```json
{
  "effective": { "resume": { "route": "/" }, "routes": { "files": { "view": "list", "...": "..." } } },
  "overrides": { "routes": { "files": { "view": "list" } } },
  "ignored": [],
  "error": null,
  "path": "/Users/me/Photos/.videre/gallery.json"
}
```

`effective` is the defaults with `overrides` (the file's contents) merged on
top. `ignored` lists the dotted paths of overrides whose type does not match
the default, which the merge skipped. `error` explains why the file could not
be read, when it exists but is not a JSON object; the effective settings are
then the defaults.

Write bodies must be a JSON object sent as `application/json` or
`application/merge-patch+json`. A write answers `409 Conflict` while the file
is unreadable, so hand edits are never overwritten, and `413 Payload Too Large`
when the result would exceed 64 KiB.

### `GET /api/settings`

The current settings, read from disk, so a hand edit shows up without a
restart.

### `PATCH /api/settings`

A JSON merge patch ([RFC 7396](https://www.rfc-editor.org/rfc/rfc7396)) over
the stored overrides. `null` removes a key, reverting it to its default. After
every write, `PATCH` or `PUT`, any value equal to its default is dropped, so
the file only ever holds what differs.

```bash
curl -X PATCH -H 'Content-Type: application/merge-patch+json' \
  -d '{"routes":{"files":{"tile":{"rowHeight":320}}}}' \
  http://127.0.0.1:7878/api/settings
```

### `PUT /api/settings`

Import. The body must hold a `routes` object, which replaces the stored
`routes` wholesale; everything else in the file, including where the library
reopens, is kept. `{"routes":{}}` resets every preference to its default.

## Files

### `GET /api/files`

Returns a page of file rows. By default it lists all scanned paths.

| Query | Meaning |
|---|---|
| `view=all` | All database rows. This is the default |
| `view=date` | One row per hash, ordered for the date view |
| `offset=<n>` | Page offset. The default is `0` |
| `limit=<n>` | Page size. The default is `200`, capped by the server |
| `date=YYYY[-MM[-DD]]` | Date prefix used by the date view |
| `from=YYYY[-MM[-DD]]` | Inclusive date lower bound for `view=date` |
| `to=YYYY[-MM[-DD]]` | Exclusive date upper bound for `view=date` |
| `hashes=<a,b,c>` | Comma-separated hashes to resolve after a search |
| `lat=<number>` | Center latitude for a `view=all` proximity filter |
| `lon=<number>` | Center longitude for a `view=all` proximity filter |
| `radius=<km>` | Positive radius in kilometers for a `view=all` proximity filter |
| `sort=<date\|name\|size\|rating\|liked\|type>` | Field the files are ordered by. Default `date`. Unknown values fall back to the default |
| `dir=<asc\|desc>` | Sort direction. Default `desc`. `path` is always the final tie-break, so pages stay stable |

`lat`, `lon` and `radius` must be supplied together. The server first narrows
GPS-bearing rows with the coordinate index, then applies exact great-circle
distance, so the returned page and `total` describe the same circle. The date
view ignores all three parameters and keeps its own one-row-per-hash behavior.

Every sort puts nulls last in both directions (undated files, unrated files),
and appends `path` as the final tie-break, so two consecutive pages never
share a row and "Show more" concatenates into the sorted order.

```bash
curl "http://127.0.0.1:7878/api/files?limit=1"
curl "http://127.0.0.1:7878/api/files?view=date&from=2025-01-01&to=2026-01-01&limit=100"
curl "http://127.0.0.1:7878/api/files?view=all&lat=52.52&lon=13.405&radius=25"
```

```json
{
  "total": 42,
  "offset": 0,
  "files": [
    {
      "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "path": "/Users/me/Photos/IMG_0001.jpg",
      "ext": "jpg",
      "size": 2457600,
      "cr": "2026-04-12T10:30:00",
      "mo": "2026-04-12T10:31:00",
      "ex": "2026-04-12T10:30:00",
      "lat": 41.0082,
      "lon": 28.9784,
      "w": 4032,
      "h": 3024,
      "tb": null,
      "fb": null,
      "meta": {
        "faces": [{ "id": 12, "name": "ayse" }],
        "location": { "lat": 41.0082, "lon": 28.9784 }
      },
      "copies": 1,
      "rating": 5,
      "pick": "keep",
      "label": "red",
      "liked": true
    }
  ]
}
```

### `PATCH /api/files/{hash}`

Updates gallery marks for one hash. Every field is optional. A missing field
leaves that mark unchanged.

| Field | Meaning |
|---|---|
| `rating` | `1` through `5`, or `0` to clear the rating |
| `pick` | Pick state such as `keep`, `reject`, or `none` to clear it |
| `label` | Colour label such as `red`, or `none` to clear it |
| `liked` | Boolean like state |

```bash
curl -i -X PATCH \
  "http://127.0.0.1:7878/api/files/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" \
  -H "content-type: application/json" \
  -d '{"rating":5,"pick":"keep","label":"red","liked":true}'
```

```http
HTTP/1.1 200 OK
content-length: 0
```

### `GET /api/files/{hash}/raw`

Serves bytes for one library file. The optional `size` query asks the gallery
to return a thumbnail-sized rendition when it can. JPEG and other browser-raster
renditions apply embedded EXIF orientation before resizing and encoding. The
gallery adds an opaque preview-version query when needed to keep browser and
server caches in step; local API callers do not need to supply it.

MP4 and MOV responses support one standard `Range: bytes=...` request and are
streamed from disk. A satisfiable range returns `206 Partial Content` with
`Accept-Ranges`, `Content-Range` and `Content-Length` headers. An invalid or
unsatisfiable range returns `416 Range Not Satisfiable`. This lets browsers load
video metadata without downloading whole videos or blocking image thumbnails.

```bash
curl -i \
  "http://127.0.0.1:7878/api/files/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/raw?size=512"
```

```http
HTTP/1.1 200 OK
content-type: image/jpeg
```

```bash
curl -i \
  -H "Range: bytes=0-1023" \
  "http://127.0.0.1:7878/api/files/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/raw"
```

```http
HTTP/1.1 206 Partial Content
accept-ranges: bytes
content-range: bytes 0-1023/2457600
content-length: 1024
```

### `POST /api/files/{hash}/rotate`

Rotates one photo 90 degrees clockwise by bumping its EXIF `Orientation` tag in
place - no pixels are re-encoded - and drops the file's cached previews so the
grid and lightbox re-render upright. The photo's display-canvas face boxes and
landmarks are turned with it, so face crops stay on their faces and keep their
people labels. Supported for EXIF-bearing images (JPEG, PNG, TIFF, WebP),
including one that has no EXIF yet, which gets its first EXIF block; any
other format (video, HEIC, and the like) returns `415 Unsupported Media Type`.
The response body carries the new orientation value.

```bash
curl -X POST \
  "http://127.0.0.1:7878/api/files/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/rotate"
```

```json
{ "orientation": 6 }
```

## Selections

The selection bar's writes. Each takes `hashes`, 1 to 10,000 content hashes
the library knows; an empty or oversized list answers `400
{"error":"invalid_parameter","field":"hashes"}`, and one unknown hash refuses
the whole request with `400 {"error":"unknown_hash"}`, so a stale page never
believes it changed something. Marks and tags are per content hash, so every
copy of a photo changes with it.

### `POST /api/files/marks`

The fields of `PATCH /api/files/{hash}` (`rating` 0 clears, `pick` and
`label` `"none"` clear, `liked`), for every hash. `pick` toggles: asking for
the pick every item already has clears it; the response's `pick` says which
was applied. A body with no mark field answers `400 {"error":"no_change"}`.

```bash
curl -X POST "http://127.0.0.1:7878/api/files/marks" \
  -H "content-type: application/json" -d '{"hashes":["aaaa…","bbbb…"],"rating":4,"liked":true}'
```

```json
{ "updated": 2, "pick": null }
```

### `POST /api/files/tags`

Adds `add` then removes `remove` on every hash. Tags are trimmed; blank ones
are ignored.

```json
{ "hashes": ["aaaa…"], "add": ["İstanbul"], "remove": ["draft"] }
```

### `GET /api/tags`

Every tag in the library with how many items carry it, most used first; the
selection bar's tag suggestions.

```json
[{ "tag": "İstanbul", "count": 12 }]
```

### `POST /api/files/rotate`

A quarter turn (`direction` `cw` or `ccw`) for every hash, as
`POST /api/files/{hash}/rotate` does for one. Items that cannot carry an
orientation (videos, RAW) are skipped and counted.

```json
{ "rotated": 10, "skipped": 2, "failed": 0 }
```

## Dates, search and locations

### `GET /api/dates`

Returns date buckets for the year, month or day drill-down.

| Query | Meaning |
|---|---|
| `level=year` | Return year buckets. This is the default |
| `level=month` | Return month buckets under `parent=YYYY` |
| `level=day` | Return day buckets under `parent=YYYY-MM` |
| `parent=<prefix>` | Parent year or month |

```bash
curl "http://127.0.0.1:7878/api/dates?level=month&parent=2026"
```

```json
{
  "buckets": [
    {
      "key": "2026-04",
      "count": 18,
      "sample": {
        "path": "/Users/me/Photos/IMG_0001.jpg",
        "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "ext": "jpg",
        "w": 4032,
        "h": 3024
      }
    }
  ]
}
```

### `GET /api/events`

Returns automatic travel trips, newest first. Home is inferred from the place
group with the most assignable media. A short trip needs ten distinct files
within a rolling three-hour window that may cross midnight, including three
dated, located photos. A multi-day trip needs ten files and at least two such
photo anchors on each of two dates. Local stops within one 20 km place group can
merge. A stop outside that radius starts a separate candidate trip that must
qualify independently; a home photo or a gap over 72 hours between destination
photo anchors also ends a trip. The ten-file, three-hour, and 20 km values are
initial defaults for a future editable Gallery configuration. Videos may join a photo-established trip
but never anchor one; GPS-less media join only with supporting capture-time and
location evidence. Only full stored capture dates count. Filesystem modification
times are not used for Events.

Each `kind` is currently `trip`. `key` is the first destination photo anchor's
`%Y%m%dT%H%M%S` wall-clock time, a hyphen and its full content hash; adding an
edge file cannot rename the trip. `title` uses the dominant stop's offline
place name, or falls back to `Trip, March 2020`. `place` is the full offline
name or `null`. The `empty_reason` field is `null` when trips are present; for
an empty list it is one of `no_media`, `no_capture_dates`,
`insufficient_location_evidence` or `no_qualifying_trips`. No prior
`videre locations` or `videre embed` run is needed. Local outings, manual
membership edits, and combining events are not supported in this iteration.

```bash
curl "http://127.0.0.1:7878/api/events"
```

```json
{
  "events": [
    {
      "key": "20200312T100000-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "kind": "trip",
      "title": "Budapest, March 2020 Trip",
      "start": "2020-03-12 10:00:00",
      "end": "2020-03-15 16:30:00",
      "count": 83,
      "place": "Budapest, HU",
      "sample": {
        "path": "/Users/me/Photos/IMG_0001.jpg",
        "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "ext": "jpg",
        "w": 4032,
        "h": 3024
      }
    }
  ],
  "empty_reason": null
}
```

### `GET /api/events/{key}/files`

Returns the files of one trip, by its exact members, in the same
`{total, offset, files}` shape as `GET /api/files`. `key` comes from `/api/events`:
the first destination photo anchor's compact time, a hyphen, and its full
content hash, so two trips starting in the same second stay distinct. An
unknown or stale key (the library or
the thresholds changed since it was read) returns `404`.

```bash
curl "http://127.0.0.1:7878/api/events/20200312T100000-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/files"
```

### `GET /api/search`

Ranks photos by text or by similarity to an existing hash. Pass exactly one of
`q` or `like`.

| Query | Meaning |
|---|---|
| `q=<text>` | Text query |
| `like=<hash>` | Find media similar to an existing hash |
| `limit=<n>` | Result count. The default is `24`, capped by the server |

```bash
curl "http://127.0.0.1:7878/api/search?q=red%20kite&limit=2"
```

```json
{
  "total": 2,
  "results": [
    {
      "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "score": 0.82
    },
    {
      "hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "score": 0.77
    }
  ]
}
```

### `GET /api/locations`

Resolves one coordinate pair to a place name and caches the answer in the
database.

```bash
curl "http://127.0.0.1:7878/api/locations?lat=41.0082&lon=28.9784"
```

```json
{
  "name": "Istanbul, Turkey"
}
```

When the location cannot be resolved, `name` is `null`.

```json
{
  "name": null
}
```

### `GET /api/location-clusters`

Returns the clusters produced by `videre locations`, largest first. The
`continent` field is derived locally from each centroid and is used by the Map
view's world overview. A library that has not run `videre locations` returns an
empty array.

```bash
curl "http://127.0.0.1:7878/api/location-clusters"
```

```json
[
  {
    "cluster_id": 7,
    "name": "Berlin, Germany",
    "route_name": "berlin_germany",
    "centroid_lat": 52.52,
    "centroid_lon": 13.405,
    "photo_count": 42,
    "radius_km": 15.0,
    "continent": "Europe"
  }
]
```

`route_name` is the normalized addressable identity used under
`/map/location/{name}`.

## Map basemap

The Map view renders MapLibre GL JS over an offline vector basemap: a PMTiles
archive downloaded once per machine into the shared geo cache on first use.
View time is fully offline; these three endpoints drive the one-time
acquisition and the local, Range-capable serving of the archive.

### `GET /tiles/basemap.pmtiles`

Serves the basemap archive with HTTP Range support (`Accept-Ranges: bytes`),
which is how the PMTiles protocol reads byte ranges out of the file. Returns
`404` when the archive is absent (the client then POSTs to
`/api/basemap/ensure`), `409` with the status body while a download is in
flight (the client keeps polling), and `200`/`206` once the archive is ready.

### `GET /api/basemap/status`

Reports the archive's download state, which the page polls after starting a
download.

```bash
curl "http://127.0.0.1:7878/api/basemap/status"
```

```json
{ "state": "ready", "bytes": 41943040 }
```

`state` is `absent`, `partial` (a download is in progress), or `ready`.

### `POST /api/basemap/ensure`

Starts the once-per-machine download if the archive is absent and none is
already running, then returns the current status immediately (same body shape
as the status endpoint). Repeated calls while a download runs are no-ops.

### `GET /vendor/{version}/{asset}`

Serves the vendored map libraries compiled into the binary, with a long-lived
immutable cache: `maplibre-gl.js`, `maplibre-gl.css`, and `pmtiles.js`. Loaded
only by the Map page, so the ~1 MB of script never weighs on the other gallery
views. The `{version}` segment is videre's own version, emitted by the page as a
cache buster so an upgrade fetches fresh bytes on the same port; its value is not
validated. Any other `{asset}` is `404`.

## People

### `GET /api/people`

Searches known people by their stored label or display name. Use the `name`
query to filter.

```bash
curl "http://127.0.0.1:7878/api/people?name=ay"
```

```json
["ayse", "ayse_yilmaz"]
```

### `POST /api/people`

Creates a person from one or more face ids.

```bash
curl -i -X POST "http://127.0.0.1:7878/api/people" \
  -H "content-type: application/json" \
  -d '{"name":"Ayse Yilmaz","face_ids":[12,13]}'
```

```http
HTTP/1.1 200 OK
content-length: 0
```

### `GET /api/people/{name}`

Returns the display name and assigned faces for one person.

```bash
curl "http://127.0.0.1:7878/api/people/ayse_yilmaz"
```

```json
{
  "label": "ayse_yilmaz",
  "full_name": "Ayse Yilmaz",
  "faces": [
    {
      "face_id": 12,
      "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "path": "/Users/me/Photos/IMG_0001.jpg",
      "is_primary": true
    }
  ]
}
```

### `PATCH /api/people/{name}`

Updates the display name for an existing person.

```bash
curl -i -X PATCH "http://127.0.0.1:7878/api/people/ayse_yilmaz" \
  -H "content-type: application/json" \
  -d '{"full_name":"Ayse Demir"}'
```

```http
HTTP/1.1 200 OK
content-length: 0
```

### `DELETE /api/people/{name}`

Unassigns a person from their faces.

```bash
curl -i -X DELETE "http://127.0.0.1:7878/api/people/ayse_yilmaz"
```

```http
HTTP/1.1 200 OK
content-length: 0
```

### `PUT /api/people/{name}/faces`

Replaces the set of faces assigned to a person. Teaching mutations return
a learning acknowledgement: the generation the action produced and the
ids of the durable evidence rows written for it. The background worker
uses that generation to decide when to retrain.

```bash
curl -i -X PUT "http://127.0.0.1:7878/api/people/ayse_yilmaz/faces" \
  -H "content-type: application/json" \
  -d '{"face_ids":[12,13,14]}'
```

```json
{
  "generation": 1,
  "event_ids": [1, 2],
  "message_key": "cluster_confirmed"
}
```

## Recluster

The People toolbar's recluster. Parameters are the eight clustering values of
[`videre faces`](/commands/faces/#clustering-parameters), by their flag names
with underscores: `eps`, `min_cluster_size`, `merge_sim`, `min_face_size`,
`max_generic_sim`, `max_landmark_error`, `min_blur`, `attach_sim`. A request
sends any of them; the rest come from the library's saved settings, then the
built-in values. Only unlabeled faces are regrouped; named faces never move.

### `GET /api/faces/cluster-params`

The parameters a recluster would use now, what the library has saved, the
built-in values, any problem reading the saved ones, and the face learning
summary.

```bash
curl "http://127.0.0.1:7878/api/faces/cluster-params"
```

```json
{
  "effective": { "eps": 0.7, "min_cluster_size": 3, "merge_sim": 0.35, "min_face_size": 80.0,
                 "max_generic_sim": 0.4, "max_landmark_error": 7.0, "min_blur": 80.0, "attach_sim": 0.4 },
  "saved": { "eps": 0.7 },
  "defaults": { "eps": 0.6, "min_cluster_size": 3, "merge_sim": 0.35, "min_face_size": 80.0,
                "max_generic_sim": 0.4, "max_landmark_error": 7.0, "min_blur": 80.0, "attach_sim": 0.4 },
  "warnings": [],
  "learning": "Learning: not used yet; naming people teaches it."
}
```

### `POST /api/faces/recluster/preview`

Computes the grouping these parameters would produce and writes nothing.

```bash
curl -X POST "http://127.0.0.1:7878/api/faces/recluster/preview" \
  -H "content-type: application/json" -d '{"eps":0.7,"min_cluster_size":2}'
```

```json
{
  "params": { "eps": 0.7, "min_cluster_size": 2, "...": "..." },
  "total_faces": 560,
  "clustered_faces": 310,
  "cluster_count": 42,
  "singletons": 250,
  "held_out": 61,
  "before": { "cluster_count": 0, "singletons": 560 }
}
```

`singletons` counts every unlabeled face left ungrouped, `held_out` the part
of them the quality gates kept out of clustering. `before` is the current
state. A value out of range answers `400` naming it:
`{"error":"invalid_parameter","field":"eps"}`.

### `POST /api/faces/recluster`

Applies the same pass: regroups the unlabeled faces, records a
`face-recluster` run (shown by [`videre status`](/commands/status/)), saves
the parameters that differ from the built-in values to `.videre/gallery.json`
under `faces.clustering` (removing the entry when none differ), and refreshes
the pending identity questions. From then on `videre faces` and `videre
watch` cluster this library with those values unless a flag says otherwise.

The response is the preview's, plus `"saved": true`, or `"saved": false`
with `settings_error` when `gallery.json` could not be read; the grouping is
applied either way and the unreadable file is left untouched. While a `videre
faces` or `videre watch` run holds the library's faces lock, it answers `409
{"error":"faces_busy"}` and changes nothing.

## Face learning

The gallery trains interpretable scorers from teaching actions in the
background and asks bounded Yes/No/Skip identity questions. The
resources below expose that state. Payloads carry scalar features and
provenance only; embeddings never leave the library.

### `GET /api/face-learning/status`

Returns the background training state and pending question count.

```bash
curl "http://127.0.0.1:7878/api/face-learning/status"
```

```json
{
  "generation": 3,
  "trained_generation": 3,
  "status": "current",
  "last_profile_id": 1,
  "last_candidate": "promoted",
  "last_error": null,
  "feedback_needed": null,
  "pending_questions": 2,
  "active_profile": { "profile_id": 1, "stage": "suggestion" },
  "summary": "Learning: profile 1 suggests names; grouping uses the settings above."
}
```

`active_profile` is the profile in use, or `null`. `summary` is one sentence
on what learning contributes right now, shown in the People toolbar: which
profile suggests names, or that learning is not used yet and why (the
feedback it is waiting for, or how many trained candidates missed the
quality checks and the first check they missed).

`status` is one of `current`, `stale` (feedback has arrived since the
last run), `training`, `waiting`, or `failed` (the last run failed; the
active profile stays as it is and `last_error` says why).

`waiting` means the last run found too little feedback to train on, which
is normal in a young library and not an error. `feedback_needed` says what
would change that, for example `dissolve 2 more wrong clusters`: naming
people only ever says which faces belong together, so the gallery also needs
a few dissolved clusters before it can learn what a wrong group looks like.
The next teaching action that records evidence trains again (naming a
person from a single face records none), and the first gallery start after
an upgrade tries once more. A failed run is retried the same ways. `last_error` is set only while `status` is `failed`.

`last_candidate` says what became of the last trained candidate:
`promoted` (it passed the quality gates and is the profile in use),
`rejected` (it failed them and the previous profile stays), or `null`
before any candidate was stored.

### `GET /api/face-learning/questions`

Returns pending identity questions, most valuable first.

```bash
curl "http://127.0.0.1:7878/api/face-learning/questions?limit=5"
```

Each question carries the subject face ids, the target person, the
validated decision evidence (per-feature contributions to the score),
and an evidence revision. `Yes` confirms the subject cluster as the
target person and teaches one positive example; `No` teaches one
negative example without labeling anyone; `Skip` only marks the
question skipped.

### `POST /api/face-learning/questions/{id}/answer`

```bash
curl -i -X POST "http://127.0.0.1:7878/api/face-learning/questions/4/answer" \
  -H "content-type: application/json" \
  -d '{"answer":"yes"}'
```

Returns the new delivery state and, for `yes` and `no`, the learning
acknowledgement. Answering a question whose subject, target, profile, or evidence
changed meanwhile returns `409 Conflict`. The outdated question is marked
superseded and disappears from the pending queue; no label or teaching event is
written. Fetch the pending questions again to see the next available question.

### `GET /api/face-learning/events`

Returns the durable teaching journal, newest first, with scalar feature
snapshots and provenance but no embeddings. Supports `limit` (1 to 200)
and `before` for stable pagination by event id.

### `GET /api/face-learning/events/{id}`

Returns one journal entry, or `404` for an unknown id.

## Faces and clusters

### `GET /api/faces`

Returns the face labeling state grouped into named people, clusters and
singletons.

```bash
curl "http://127.0.0.1:7878/api/faces"
```

```json
{
  "people": [
    {
      "label": "ayse_yilmaz",
      "full_name": "Ayse Yilmaz",
      "face_ids": [12, 13],
      "representative_id": 12,
      "hashes": [
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
      ]
    }
  ],
  "clusters": [
    {
      "cluster_id": 7,
      "face_ids": [21, 22],
      "hashes": [
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
      ]
    }
  ],
  "singletons": [
    {
      "face_id": 31,
      "hash": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
    }
  ]
}
```

### `PATCH /api/faces/{id}`

Marks one face as the primary face for a person.

```bash
curl -i -X PATCH "http://127.0.0.1:7878/api/faces/12" \
  -H "content-type: application/json" \
  -d '{"person_label":"ayse_yilmaz"}'
```

```http
HTTP/1.1 200 OK
content-length: 0
```

### `DELETE /api/faces/{id}`

Unassigns one face.

```bash
curl -i -X DELETE "http://127.0.0.1:7878/api/faces/12"
```

```http
HTTP/1.1 200 OK
content-length: 0
```

### `GET /api/faces/{id}/image`

Serves the cropped face thumbnail as JPEG bytes.

```bash
curl -i "http://127.0.0.1:7878/api/faces/12/image"
```

```http
HTTP/1.1 200 OK
content-type: image/jpeg
```

### `GET /api/faces/{id}/original`

Serves the original source file for a face, with a content type inferred from
the file.

```bash
curl -i "http://127.0.0.1:7878/api/faces/12/original"
```

```http
HTTP/1.1 200 OK
content-type: image/jpeg
```

### `GET /api/clusters/{id}`

Returns one face cluster.

```bash
curl "http://127.0.0.1:7878/api/clusters/7"
```

```json
{
  "cluster_id": 7,
  "faces": [
    {
      "face_id": 21,
      "hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "path": "/Users/me/Photos/IMG_0021.jpg"
    }
  ]
}
```

### `DELETE /api/clusters/{id}`

Dissolves one cluster.

```bash
curl -i -X DELETE "http://127.0.0.1:7878/api/clusters/7"
```

```http
HTTP/1.1 200 OK
content-length: 0
```

## Server Control

### `POST /api/quit`

Stops the running gallery server.

```bash
curl -i -X POST "http://127.0.0.1:7878/api/quit"
```

```http
HTTP/1.1 200 OK
content-length: 0
```

## Endpoint Index

| Endpoint | Purpose |
|---|---|
| `GET /api/files` | List files or fetch files by hash |
| `PATCH /api/files/{hash}` | Update marks on one file |
| `GET /api/files/{hash}/raw` | Serve bytes for one library file |
| `POST /api/files/{hash}/rotate` | Rotate one photo 90 degrees clockwise (EXIF) |
| `POST /api/files/marks` | Set marks on a selection |
| `POST /api/files/tags` | Add or remove tags on a selection |
| `GET /api/tags` | List the library's tags with counts |
| `POST /api/files/rotate` | Rotate a selection a quarter turn |
| `GET /api/dates` | Read date buckets |
| `GET /api/events` | List automatic travel trips and an empty reason when none qualify |
| `GET /api/events/{key}/files` | Exact files of one trip |
| `GET /api/search` | Rank by text or by an existing file hash |
| `GET /api/locations` | Resolve one coordinate pair to a place name |
| `GET /api/location-clusters` | List location clusters for the Map view |
| `GET /tiles/basemap.pmtiles` | Serve the offline basemap archive (Range) |
| `GET /api/basemap/status` | Report the basemap download state |
| `POST /api/basemap/ensure` | Start the one-time basemap download |
| `GET /vendor/{version}/{asset}` | Serve a vendored map library (MapLibre, pmtiles) |
| `GET /api/people` | Search people |
| `POST /api/people` | Create a person from faces |
| `GET /api/people/{name}` | Read one person |
| `PATCH /api/people/{name}` | Update one person's display name |
| `DELETE /api/people/{name}` | Unassign one person |
| `PUT /api/people/{name}/faces` | Attach faces to a person |
| `GET /api/faces` | Read people, clusters and singletons |
| `PATCH /api/faces/{id}` | Mark a face as primary for a person |
| `DELETE /api/faces/{id}` | Unassign one face |
| `GET /api/faces/{id}/image` | Serve one face thumbnail |
| `GET /api/faces/{id}/original` | Serve the source file for one face |
| `GET /api/clusters/{id}` | Read one cluster |
| `DELETE /api/clusters/{id}` | Dissolve one cluster |
| `GET /api/face-learning/status` | Report the background training state |
| `GET /api/face-learning/questions` | List pending identity questions |
| `POST /api/face-learning/questions/{id}/answer` | Answer one identity question |
| `GET /api/face-learning/events` | List the teaching journal |
| `GET /api/face-learning/events/{id}` | Read one journal entry |
| `POST /api/quit` | Stop the gallery server |
