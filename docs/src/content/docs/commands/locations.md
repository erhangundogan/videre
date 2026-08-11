---
title: videre locations
description: Group photos by where they were taken, using GPS data already in the files.
---

Groups photos by where they were taken, using GPS coordinates
[`videre scan`](/commands/scan/) already extracted from EXIF. No network access:
place names come from an offline lookup.

```bash
videre locations                       # group and print a summary
videre locations --radius 25           # how far apart places can be, in km (default 15)
videre locations --json                # print one JSON object instead
videre locations --geojson             # print GeoJSON (opens in geojson.io, QGIS, ...)
videre locations --silent              # no summary
videre locations --db ~/photos.db      # use a specific database
```

## The workflow

```bash
videre scan ~/Photos       # GPS comes from EXIF during the scan
videre locations           # group those coordinates into places
```

Output is a summary of the places found, largest first:

```
142 location cluster(s) found
  Berlin, Germany              3,841 photos
  Kreuzberg, Berlin, Germany     612 photos
  Lisbon, Portugal               498 photos
  ...
```

Nothing else is required. GPS is already in the database after a scan, so this
does not depend on [`watch --location`](/commands/watch/) having run.

## Choosing a radius

`--radius` is the only real tuning, and it decides what counts as one place:

| Radius | Groups look like |
|---|---|
| 2 to 5 km | Neighbourhoods and individual venues |
| 15 km (default) | Which city was I in |
| 50 to 100 km | Regions, metro areas |
| 300+ km | Countries, trips |

There is no correct value. A single holiday reads better at 5 km, a decade of
photos at 15 or more. Since every run recomputes from scratch, trying another
radius costs nothing but time.

```bash
videre locations --radius 5     # break a city into districts
videre locations --radius 200   # collapse a trip into one place
```

Every coordinate ends up in some group. Unlike face grouping there is no quality
gate, so a single photo taken somewhere unique becomes its own one-photo place,
which is correct: a GPS fix is never ambiguous the way a blurry face is.

## Feeding a map

`--geojson` emits a standard `FeatureCollection` of points, which drops straight
into geojson.io, QGIS, or anything else that reads GeoJSON:

```bash
videre locations --geojson > places.geojson
videre locations --radius 5 --geojson | pbcopy    # paste into geojson.io
```

Per the GeoJSON spec, coordinates are `[lon, lat]`, reversed from the order
videre uses everywhere else. That is the spec's convention, not a bug.

`--json` gives the same data in videre's own shape, with `id`, `name`,
`centroid_lat`, `centroid_lon` and `photo_count` per cluster:

```bash
videre locations --json | jq -r '.clusters[] | "\(.photo_count)\t\(.name)"' | sort -rn
```

`--json` and `--geojson` are mutually exclusive.

## Caveats

:::caution[Every run is a full recompute, and it is slower than it sounds]
Each run clears the previous results and regroups from scratch. The clustering
itself is sub-second, but writing the results back measured about **8 minutes on
a 70,000 file library**.

The whole recompute runs inside one transaction, so it holds the single write
lock for that entire window. A [`videre watch`](/commands/watch/) running at the
same time will block until it finishes.

On a large library, run it deliberately rather than casually, and not while
something else needs to write.
:::

**Cluster IDs are not stable between runs.** They are only meaningful within one
run's output. Do not store them or build anything that assumes `id` 7 is the
same place tomorrow. Names are re-derived each time; the IDs are not.

**`photo_count` counts files, not distinct photos.** Two copies of one image at
the same coordinates count twice, because it counts rows in the database. That
makes the numbers a good measure of storage and a slightly inflated measure of
"how many pictures did I take here".

**Only photos with GPS take part.** Most phone photos have it; most scans,
screenshots and older camera photos do not. Nothing is wrong if a large part of
your library never appears here. A library with no GPS at all is not an error:
it reports zero clusters and exits successfully.

**Place names come from an offline lookup**, so they are coarser than a live
geocoder would give and occasionally name a nearby larger place instead of the
exact one. That is the tradeoff for making this work with no network.

**Centroids are a plain average**, which is wrong near the antimeridian
(+/-180 longitude) and the poles. A group spanning that line gets a centroid
somewhere unhelpful. Accepted rather than solved, since this is
15 km-granularity grouping.

## Related

This groups places already in your library. To search for an arbitrary place by
name, including somewhere you have no photos of yet, use
[`videre search --location`](/commands/search/). That one does use the network,
and does not depend on this command having run.
