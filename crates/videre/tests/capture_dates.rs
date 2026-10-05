//! One capture date per file: scan resolves it from EXIF, a video's own
//! date, a Google Takeout sidecar, or the file's time, and fills a missing
//! location from the sidecar.

mod common;
use common::TestLibrary;
use std::path::Path;

/// `IMG-20160104-WA0000.jpg`'s real sidecar values: taken 4 Jan 2016,
/// 20:29:58 UTC.
const TAKEN: i64 = 1_451_939_398;

fn sidecar(media: &Path, taken: i64, lat: f64, lon: f64) {
    let name = format!(
        "{}.supplemental-metadata.json",
        media.file_name().unwrap().to_string_lossy()
    );
    std::fs::write(
        media.with_file_name(name),
        format!(
            r#"{{"title":"x","photoTakenTime":{{"timestamp":"{taken}"}},
                "creationTime":{{"timestamp":"1600000000"}},
                "geoData":{{"latitude":{lat},"longitude":{lon}}}}}"#
        ),
    )
    .unwrap();
}

/// A JPEG with no EXIF at all, as WhatsApp and Viber save them. `bytes`
/// varies the pixels, so two such files are not one content key.
fn undated_jpeg(lib: &TestLibrary, rel: &str, shade: u8) -> std::path::PathBuf {
    let path = lib.context().paths.root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    image::RgbImage::from_pixel(16, 16, image::Rgb([shade, 90, 200]))
        .save(&path)
        .unwrap();
    path
}

struct Row {
    capture_date: Option<String>,
    date_source: Option<String>,
    exif_date: Option<String>,
    gps: Option<(f64, f64)>,
    gps_source: Option<String>,
}

fn row(lib: &TestLibrary, path: &Path) -> Row {
    lib.conn()
        .query_row(
            "SELECT capture_date, date_source, exif_date, gps_lat, gps_lon, gps_source
             FROM file_hashes WHERE path LIKE '%' || ?1",
            // By the end of the path: the library stores its canonical form
            // (`/private/var/...` on macOS), the test built `/var/...`.
            [format!(
                "/{}/{}",
                path.parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy(),
                path.file_name().unwrap().to_string_lossy()
            )],
            |r| {
                let lat: Option<f64> = r.get(3)?;
                let lon: Option<f64> = r.get(4)?;
                Ok(Row {
                    capture_date: r.get(0)?,
                    date_source: r.get(1)?,
                    exif_date: r.get(2)?,
                    gps: lat.zip(lon),
                    gps_source: r.get(5)?,
                })
            },
        )
        .unwrap()
}

#[test]
fn an_undated_takeout_photo_takes_its_sidecar_date_and_place() {
    let lib = TestLibrary::new();
    let photo = undated_jpeg(&lib, "Fotoğraflar/IMG-20160104-WA0000.jpg", 10);
    sidecar(&photo, TAKEN, 52.52, 13.40);
    lib.scan();
    let r = row(&lib, &photo);
    assert_eq!(r.date_source.as_deref(), Some("sidecar"));
    assert_eq!(r.capture_date, videre_core::capture_date::wall_clock(TAKEN));
    assert_eq!(
        r.exif_date, None,
        "the camera value stays what the camera wrote"
    );
    assert_eq!(r.gps, Some((52.52, 13.40)));
    assert_eq!(r.gps_source.as_deref(), Some("sidecar"));
}

#[test]
fn exif_beats_the_sidecar_for_both_date_and_place() {
    let lib = TestLibrary::new();
    let photo = lib.copy_fixture("gps_south_west.jpg", "Tatil/güney.jpg");
    sidecar(&photo, TAKEN, 52.52, 13.40);
    lib.scan();
    let r = row(&lib, &photo);
    assert_ne!(r.gps, Some((52.52, 13.40)), "EXIF GPS kept: {:?}", r.gps);
    assert_eq!(r.gps_source.as_deref(), Some("file"));
    if r.exif_date.is_some() {
        assert_eq!(r.date_source.as_deref(), Some("exif"));
        assert_eq!(r.capture_date, r.exif_date);
    }
}

#[test]
fn a_sidecar_at_zero_zero_is_no_place() {
    let lib = TestLibrary::new();
    let photo = undated_jpeg(&lib, "Fotoğraflar/WhatsApp-çiçek.jpg", 20);
    sidecar(&photo, TAKEN, 0.0, 0.0);
    lib.scan();
    let r = row(&lib, &photo);
    assert_eq!(r.gps, None);
    assert_eq!(r.gps_source, None);
    assert_eq!(r.date_source.as_deref(), Some("sidecar"));
}

#[test]
fn a_truncated_sidecar_name_still_matches() {
    let lib = TestLibrary::new();
    let photo = undated_jpeg(&lib, "Fotoğraflar/uzun-isimli-fotoğraf.jpg", 30);
    std::fs::write(
        photo.with_file_name("uzun-isimli-fotoğraf.jpg.suppl.json"),
        format!(r#"{{"photoTakenTime":{{"timestamp":"{TAKEN}"}}}}"#),
    )
    .unwrap();
    lib.scan();
    assert_eq!(row(&lib, &photo).date_source.as_deref(), Some("sidecar"));
}

#[test]
fn a_file_with_nothing_else_is_dated_by_its_own_time_in_local_time() {
    let lib = TestLibrary::new();
    let photo = undated_jpeg(&lib, "Ekran/görüntü.jpg", 40);
    filetime::set_file_mtime(&photo, filetime::FileTime::from_unix_time(TAKEN, 0)).unwrap();
    lib.scan();
    let r = row(&lib, &photo);
    assert_eq!(r.date_source.as_deref(), Some("mtime"));
    assert_eq!(r.capture_date, videre_core::capture_date::wall_clock(TAKEN));
}

/// A library written before capture dates has rows with no source (and an
/// EXIF date stored as the camera wrote it, hour 24 included). The next scan
/// resolves them without hashing a single file again.
#[test]
fn rows_from_before_capture_dates_resolve_on_the_next_scan_without_rehashing() {
    let lib = TestLibrary::new();
    let undated = undated_jpeg(&lib, "Fotoğraflar/IMG-20160104-WA0000.jpg", 50);
    sidecar(&undated, TAKEN, 41.04, 29.0);
    let dated = lib.copy_fixture("tiny.jpg", "Fotoğraflar/gece-yarısı.jpg");
    lib.scan();
    lib.conn()
        .execute_batch(
            "UPDATE file_hashes SET capture_date = NULL, date_source = NULL,
                 gps_source = NULL, gps_lat = NULL, gps_lon = NULL;
             UPDATE file_hashes SET exif_date = '2015-10-29T24:03:39'
                 WHERE path LIKE '%gece-yarısı.jpg';",
        )
        .unwrap();

    let out = lib.cmd().args(["scan", "--json"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["total_files"], 0, "nothing rehashed: {doc}");

    let r = row(&lib, &undated);
    assert_eq!(r.date_source.as_deref(), Some("sidecar"));
    assert_eq!(r.capture_date, videre_core::capture_date::wall_clock(TAKEN));
    assert_eq!(r.gps, Some((41.04, 29.0)));
    let r = row(&lib, &dated);
    assert_eq!(r.date_source.as_deref(), Some("exif"));
    assert_eq!(r.capture_date.as_deref(), Some("2015-10-30T00:03:39"));
    assert_eq!(r.exif_date.as_deref(), Some("2015-10-30T00:03:39"));
}

/// status counts a sidecar-dated photo as fix-dates work until fix-dates
/// writes it, then not.
#[test]
fn status_counts_a_sidecar_date_as_fix_dates_work() {
    let lib = TestLibrary::new();
    let photo = undated_jpeg(&lib, "Fotoğraflar/IMG-20160104-WA0000.jpg", 60);
    sidecar(&photo, TAKEN, 0.0, 0.0);
    lib.scan();
    let outstanding = |lib: &TestLibrary| -> i64 {
        let out = lib.cmd().args(["status", "--json"]).output().unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        doc["report"]["coverage"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["stage"] == "fix-dates")
            .unwrap()["outstanding"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(outstanding(&lib), 1);
    assert!(lib
        .cmd()
        .args(["fix-dates", "--yes"])
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(outstanding(&lib), 0);
}
