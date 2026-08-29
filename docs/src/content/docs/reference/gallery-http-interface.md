---
title: Gallery HTTP interface
description: The local HTTP routes served while videre gallery is running.
---

`videre gallery` starts a local server for its own browser interface. The
routes below exist while that process is running, bound to `127.0.0.1`.

This is a local interface, not a hosted service. It is documented so you can
understand what the gallery is doing and call it from your own local tools when
that is useful.

```bash
videre gallery --browse
```

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

## JSON endpoints

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
