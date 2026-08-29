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

The examples below use `http://127.0.0.1:7878`, the default address.

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
| `GET /people/cluster/{id}` | One face cluster |
| `GET /people/person/{name}` | One person |
| `GET /map` | Reserved |
| `GET /events` | Reserved |
| `GET /smart` | Reserved |

The reserved routes currently return a placeholder page with `404 Not Found`.

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
| `hashes=<a,b,c>` | Comma-separated hashes to resolve after a search |

```bash
curl "http://127.0.0.1:7878/api/files?limit=1"
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
to return a thumbnail-sized rendition when it can.

```bash
curl -i \
  "http://127.0.0.1:7878/api/files/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/raw?size=512"
```

```http
HTTP/1.1 200 OK
content-type: image/jpeg
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

Replaces the set of faces assigned to a person.

```bash
curl -i -X PUT "http://127.0.0.1:7878/api/people/ayse_yilmaz/faces" \
  -H "content-type: application/json" \
  -d '{"face_ids":[12,13,14]}'
```

```http
HTTP/1.1 200 OK
content-length: 0
```

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
| `GET /api/dates` | Read date buckets |
| `GET /api/search` | Rank by text or by an existing file hash |
| `GET /api/locations` | Resolve one coordinate pair to a place name |
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
| `POST /api/quit` | Stop the gallery server |
