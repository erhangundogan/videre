# `cities.csv`

Place names for offline reverse geocoding, in the schema
`reverse_geocoder::ReverseGeocoder::from_path` expects:
`lat,lon,name,admin1,admin2,cc` with CRLF line endings.

**Why this exists rather than the crate's own data.** `reverse_geocoder` embeds
GeoNames' `asciiname` column, which is ASCII by definition and mangles a fifth
of the world's place names: `Üsküdar` is stored as `UEskuedar`, `Malmö` as
`Malmoe`. That is GeoNames' data, not a defect in the crate. It cannot be
undone in code, since `UE` -> `Ü` is ambiguous and would corrupt names that are
already correct, so the only fix is to supply the `name` column instead.

`admin1` and `admin2` are written empty on purpose: `location_name` formats only
`name` and `cc`, nothing in videre reads the other two, and dropping them makes
this file smaller than the one it replaces (5.7MB against 7.5MB).

## Regenerating

Source: <https://download.geonames.org/export/dump/cities1000.zip>
(CC BY 4.0). Columns are `geonameid, name, asciiname, alternatenames, lat, lon,
...` tab-separated; **field 2 is the one to use, not field 3.**

```bash
curl -sLO https://download.geonames.org/export/dump/cities1000.zip
unzip -o cities1000.zip
python3 - <<'PY'
import csv
rows = []
for line in open("cities1000.txt", encoding="utf-8"):
    p = line.rstrip("\n").split("\t")
    if len(p) < 9:
        continue
    name, lat, lon, cc = p[1], p[4], p[5], p[8]
    if name and cc:
        rows.append((lat, lon, name, "", "", cc))
with open("cities.csv", "w", encoding="utf-8", newline="") as f:
    w = csv.writer(f, lineterminator="\r\n")
    w.writerow(["lat", "lon", "name", "admin1", "admin2", "cc"])
    w.writerows(rows)
print(len(rows), "rows")
PY
```

`the_bundled_data_is_actually_unicode` in `src/location.rs` fails if a
regenerated file drops below 30,000 non-ASCII rows, which is what building from
field 3 by mistake looks like.

**Updating the data does not update anyone's library.** `location_name` is
resolved at write time, so `file_hashes.location_name` and
`location_clusters.name` keep whatever was current when they were written. A
data change needs a `videre locations` recompute to take effect.
