---
title: videre locations
description: Group photos by where they were taken, using GPS data already in the files.
---

Groups photos by where they were taken, using GPS data already in the files.
No network access: place names come from an offline lookup.

```bash
videre locations                       # group and print a summary
videre locations --radius 25           # how far apart places can be, in km (default 15)
videre locations --json                # print one JSON object instead
videre locations --geojson             # print GeoJSON (opens in geojson.io, QGIS, ...)
videre locations --silent              # no summary
videre locations --db ~/photos.db      # use a specific database
```

`--radius` defaults to 15 km, roughly "which city was I in" granularity. It is
the one tunable parameter.

Having no GPS data at all is not an error: it reports zero groups and exits
successfully.

## Full recompute every run

Each run regroups from scratch rather than adding to previous results. There is
no expensive detection step to make incremental, and the grouping maths itself
is sub-second.

The slow part is writing the results back: measured at about 8 minutes on a
70,000 file library, in a single transaction. Since that holds the write lock
for the whole window, a concurrently running [`videre watch`](/commands/watch/)
will block until it finishes.

Group IDs are **not stable across runs**, only within a single run's output.

## `--geojson`

Emits a standard `FeatureCollection` of `Point` features, so the output drops
straight into geojson.io, QGIS, or anything else that reads GeoJSON. Per the
GeoJSON spec, coordinates are `[lon, lat]`, reversed from the order videre uses
everywhere else.

`--json` and `--geojson` are mutually exclusive.

## Related

This groups places already in your library. To search for an arbitrary place by
name, including ones you have no photos of yet, use
[`videre search --location`](/commands/search/) — that one does use the network.
