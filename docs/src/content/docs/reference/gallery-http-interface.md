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
| `GET /events` | Automatic time-and-place event overview |
| `GET /events/{key}` | One event's photos (`key` is its compact start time and a short hash) |
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
query is measured in kilometers; without it the route uses the cluster's stored
radius. An unknown name still returns the working map page with HTTP 200 and an
unknown-location state.

`GET /map?near={lat},{lon}` takes a `"lat,lon"` pair instead of a name and
resolves it to the cluster whose centroid is nearest by haversine distance,
bootstrapping that cluster's drill-down. The lightbox place link uses it because
a photo's reverse-geocoded place name is finer than any cluster name; the client
then rewrites the address bar to the resolved `/map/location/{name}`. A library
with no clusters resolves to the unselected map.

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

`lat`, `lon` and `radius` must be supplied together. The server first narrows
GPS-bearing rows with the coordinate index, then applies exact great-circle
distance, so the returned page and `total` describe the same circle. The date
view ignores all three parameters and keeps its own one-row-per-hash behavior.

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
people labels. Supported for EXIF-bearing images (JPEG, PNG, TIFF, WebP); any
other format (video, HEIC, and the like) returns `415 Unsupported Media Type`.

Rewriting the tag changes the file's content, and so its content hash. The new
hash is recorded at once, the faces move to it (unless another path holds the
old content, in which case they stay with that copy), and the response carries
it with the new orientation value. Use the new hash for later requests about
this photo; the old one no longer names it.

```bash
curl -X POST \
  "http://127.0.0.1:7878/api/files/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/rotate"
```

```json
{ "orientation": 6, "hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
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

Returns the automatic event overview, newest first. An event is a run of
photos with no gap longer than six hours and no jump greater than five
kilometers between two located shots; both are computed from the library on
each request, so there is nothing to precompute. `key` is the event's start
time in a URL-safe compact form (`%Y%m%dT%H%M%S`), used as the drill-down
address. `place` is the offline reverse-geocode of the event's GPS centroid,
or `null` when no member carried GPS.

```bash
curl "http://127.0.0.1:7878/api/events"
```

```json
{
  "events": [
    {
      "key": "20210810T143207-3f9a1c2e",
      "start": "2021-08-10 14:32:07",
      "end": "2021-08-12 09:15:44",
      "count": 83,
      "place": "Bodrum, TR",
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

### `GET /api/events/{key}/files`

Returns the files of one event, by its exact members, in the same
`{total, offset, files}` shape as `GET /api/files`. `key` comes from `/api/events`:
the event's compact start time, a hyphen, and the first eight characters of
its first file's hash, so two events starting in the same second stay
distinct. An unknown or stale key (the library or
the thresholds changed since it was read) returns `404`.

```bash
curl "http://127.0.0.1:7878/api/events/20210810T143207-3f9a1c2e/files"
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
  "pending_questions": 2
}
```

`status` is one of `current`, `stale` (feedback has arrived since the
last run), `training`, or `failed` (the last run failed; the active
profile stays as it is and `last_error` says why).

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
| `GET /api/dates` | Read date buckets |
| `GET /api/events` | List automatic time-and-place events |
| `GET /api/events/{key}/files` | Files of one event |
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
