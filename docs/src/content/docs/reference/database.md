---
title: The database
description: The SQLite schema videre writes, and how to query it yourself.
---

Everything videre knows lives in one ordinary SQLite file. No proprietary
format, no daemon holding it open, nothing you need videre itself to read. If
you want to answer a question videre has no command for, write the query.

```bash
sqlite3 ~/.videre/hashes.db
```

See [where your data lives](/reference/paths/) for how that path is resolved.

## file_hashes

One row per file path. This is what [`videre scan`](/commands/scan/) writes and
what nearly everything else reads.

```sql
CREATE TABLE file_hashes (
    path        TEXT PRIMARY KEY,
    hash        TEXT NOT NULL,
    size_bytes  INTEGER,
    created_at  TEXT,
    modified_at TEXT,
    ext         TEXT,
    mime        TEXT,
    phash       INTEGER,
    exif_date   TEXT,
    gps_lat     REAL,
    gps_lon     REAL,
    width       INTEGER,
    height      INTEGER
);
```

Plus two columns added by later versions, through a migration that runs
automatically when the database is opened:

```sql
ALTER TABLE file_hashes ADD COLUMN location_name TEXT;
ALTER TABLE file_hashes ADD COLUMN location_cluster_id INTEGER;
```

| Column | Notes |
|---|---|
| `path` | Absolute path. The primary key, so re-scanning updates in place |
| `hash` | BLAKE3 of the file contents. Two identical files share one hash |
| `mime` | Detected from the file's leading bytes, not its name |
| `phash` | Perceptual fingerprint, only with [`scan --similar`](/commands/scan/). NULL otherwise |
| `exif_date` | Camera-local, no timezone. `0000-*` values are discarded as absent |
| `location_name` | Filled in lazily, not by `scan`. See [`watch --location`](/commands/watch/) |
| `location_cluster_id` | Set by [`videre locations`](/commands/locations/) |

`created_at` is always empty on Linux; the birth time needs a macOS syscall.

:::note[hash is the join key, not path]
Faces, classifications and embeddings are all keyed by `hash`, never by `path`.
That is what lets duplicate copies of one photo share a single face detection or
embedding, and it means a file that moves keeps its work as soon as it is
re-scanned.
:::

## faces

One row per detected face, written by [`videre faces`](/commands/faces/).

```sql
CREATE TABLE faces (
    id            INTEGER PRIMARY KEY,
    hash          TEXT NOT NULL,
    bbox          TEXT NOT NULL,
    landmark      TEXT,
    embedding     BLOB NOT NULL,
    cluster_id    INTEGER,
    person_label  TEXT,
    confirmed     INTEGER DEFAULT 0,
    is_primary    INTEGER DEFAULT 0
);
```

`bbox` and `landmark` are JSON. `embedding` is a 512-dimension ArcFace vector
stored as raw f16, so 1024 bytes. `cluster_id` is assigned by grouping;
`person_label` and `confirmed` are what the labeling UI writes, and
[`search --person`](/commands/search/) reads.

```sql
CREATE TABLE faces_scanned (
    hash        TEXT PRIMARY KEY,
    scanned_at  TEXT DEFAULT (datetime('now'))
);
```

This is what makes face detection resumable. Every processed image is recorded
here **including images with no faces**, which produce no `faces` row at all.
Without it, every photo of a landscape would be re-examined on every run.

## classifications

Written by [`videre classify`](/commands/classify/).

```sql
CREATE TABLE classifications (
    model_id      TEXT NOT NULL,
    hash          TEXT NOT NULL,
    category      TEXT NOT NULL,
    confidence    REAL NOT NULL,
    classified_at TEXT NOT NULL,
    PRIMARY KEY (model_id, hash)
);
```

`category` is `photo`, `screenshot`, `document`, `meme`, or `unknown`. The key
includes `model_id`, so two [models](/reference/models/) can classify the same
library without overwriting each other.

## location_clusters and geocode_cache

```sql
CREATE TABLE location_clusters (
    id            INTEGER PRIMARY KEY,
    centroid_lat  REAL NOT NULL,
    centroid_lon  REAL NOT NULL,
    name          TEXT,
    photo_count   INTEGER NOT NULL,
    radius_km     REAL NOT NULL,
    created_at    TEXT NOT NULL
);

CREATE TABLE geocode_cache (
    query       TEXT PRIMARY KEY,
    lat         REAL NOT NULL,
    lon         REAL NOT NULL,
    resolved_at TEXT NOT NULL
);
```

[`videre locations`](/commands/locations/) rebuilds `location_clusters` from
scratch on every run, so `id` is not stable between runs.

`geocode_cache` is the one table written by a network call: it remembers place
names looked up by [`search --location`](/commands/search/) so a repeated query
never repeats the request.

## pipeline_runs

One row per tracked command, not an append-only log, so it holds the *last* run
of each. This is what [`videre stats`](/commands/stats/) reports.

```sql
CREATE TABLE pipeline_runs (
    command      TEXT PRIMARY KEY,
    started_at   TEXT NOT NULL,
    finished_at  TEXT,
    status       TEXT NOT NULL,
    duration_ms  INTEGER,
    summary      TEXT
);
```

`status` is stored as `running`, `success`, `failed` or `interrupted`. A fifth
value, `crashed`, is never written: it is computed when reading, when a row says
`running` but no live process holds that command's lock.

## The embeddings database

Embeddings are **not** in the main file. Each library and model pair gets its
own database under `~/.videre/embeddings/`, attached when needed:

```sql
CREATE TABLE embeddings (
    hash        TEXT PRIMARY KEY NOT NULL,
    model_id    TEXT NOT NULL,
    embedding   BLOB NOT NULL,
    embedded_at TEXT NOT NULL
);
```

`embedding` is an L2-normalized f16 vector. [Search models](/reference/models/)
explains the layout and why it is split out.

To query it alongside the main database, attach it yourself:

```sql
ATTACH DATABASE '~/.videre/embeddings/<library>/<model>.db' AS emb;
SELECT COUNT(*) FROM emb.embeddings;
```

## Useful queries

Duplicate groups, largest first:

```sql
SELECT hash, COUNT(*) n, SUM(size_bytes)/1048576.0 mb
FROM file_hashes GROUP BY hash HAVING n > 1 ORDER BY mb DESC;
```

Total space wasted by duplicates, in MB:

```sql
SELECT SUM(size_bytes * (cnt - 1))/1048576.0
FROM (SELECT size_bytes, COUNT(*) cnt FROM file_hashes GROUP BY hash HAVING cnt > 1);
```

Photos per named person:

```sql
SELECT person_label, COUNT(DISTINCT hash) photos
FROM faces WHERE confirmed = 1 AND person_label IS NOT NULL
GROUP BY person_label ORDER BY photos DESC;
```

What is in the library, by type:

```sql
SELECT ext, COUNT(*) n, SUM(size_bytes)/1073741824.0 gb
FROM file_hashes GROUP BY ext ORDER BY n DESC;
```

Files scanned but never given a type, meaning a scan did not finish them:

```sql
SELECT COUNT(*) FROM file_hashes WHERE mime IS NULL;
```

Those are what [`scan --retry-incomplete`](/commands/scan/) picks up.

## Writing to it yourself

videre opens every connection in WAL mode, so one writer and many readers
coexist. You can safely run `sqlite3` queries while
[`videre watch`](/commands/watch/) is running.

Reading is entirely safe. If you write, note that videre assumes `hash` is a
real BLAKE3 of the file at `path`, and [`videre prune`](/commands/prune/)
deletes embeddings and cached thumbnails whose hash no longer appears in
`file_hashes`. Deleting rows by hand therefore discards the derived work for
those photos too.
