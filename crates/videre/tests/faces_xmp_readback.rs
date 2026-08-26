//! End-to-end proof that `videre faces` imports a face name from an XMP region:
//! a sidecar naming a face (as digiKam or Lightroom would write) is read on a
//! faces run, matched to the detected face by overlap, and the name applied, so
//! `videre search --person` then finds the photo.
//!
//! Needs the face models, so it is gated exactly like the other faces
//! integration tests: it skips on a cold cache and holds the shared-cache lock.

mod common;
use common::{face_models_cached, shared_cache_guard, skip_without_models, videre_bin as bin};

use rusqlite::Connection;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

fn run(args: &[&str]) {
    let out = Command::new(bin()).args(args).output().expect("run videre");
    assert!(
        out.status.success(),
        "videre {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Write a sidecar next to `photo` with one MWG face region named `name`, its
/// area the center-normalized form of the pixel `bbox` ("x,y,w,h") against
/// `w`x`h`, so it overlaps that detected face exactly.
fn write_region_sidecar(photo: &Path, name: &str, bbox: &str, iw: f64, ih: f64) {
    let n: Vec<f64> = bbox.split(',').map(|s| s.trim().parse().unwrap()).collect();
    let (x, y, bw, bh) = (n[0], n[1], n[2], n[3]);
    let (cx, cy, nw, nh) = ((x + bw / 2.0) / iw, (y + bh / 2.0) / ih, bw / iw, bh / ih);
    let doc = format!(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/" xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#">
<mwg-rs:Regions rdf:parseType="Resource"><mwg-rs:RegionList><rdf:Bag>
<rdf:li rdf:parseType="Resource"><mwg-rs:Name>{name}</mwg-rs:Name>
<mwg-rs:Area stArea:x="{cx:.6}" stArea:y="{cy:.6}" stArea:w="{nw:.6}" stArea:h="{nh:.6}" stArea:unit="normalized"/></rdf:li>
</rdf:Bag></mwg-rs:RegionList></mwg-rs:Regions></rdf:Description></rdf:RDF></x:xmpmeta>"#
    );
    let side = photo.with_extension("jpg.xmp");
    std::fs::write(side, doc).unwrap();
}

#[test]
fn imports_a_face_name_from_an_xmp_region() {
    let _serial = shared_cache_guard();
    if skip_without_models("faces xmp read-back", face_models_cached()) {
        return;
    }

    let dir = tempdir().unwrap();
    let photos = dir.path().join("photos");
    std::fs::create_dir(&photos).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ai-generated-couple.jpg");
    let photo = photos.join("couple.jpg");
    std::fs::copy(&src, &photo).unwrap();
    let db = dir.path().join("hashes.db");
    let (photos_s, db_s) = (photos.to_str().unwrap(), db.to_str().unwrap());

    // Scan (width/height come from the image header) and detect faces.
    run(&["scan", photos_s, "--db", db_s, "--silent"]);
    run(&["faces", "--db", db_s, "--min-cluster-size", "1", "--silent"]);

    // Read a detected face's bbox and the image dimensions, so the region we
    // write overlaps a real face regardless of the model's exact output.
    let (bbox, iw, ih): (String, f64, f64) = {
        let conn = Connection::open(&db).unwrap();
        let bbox: String = conn
            .query_row("SELECT bbox FROM faces ORDER BY id LIMIT 1", [], |r| {
                r.get(0)
            })
            .expect("the fixture must contain at least one detectable face");
        let (w, h): (f64, f64) = conn
            .query_row("SELECT width, height FROM file_hashes LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        (bbox, w, h)
    };
    assert!(iw > 0.0 && ih > 0.0, "scan must record image dimensions");

    // Write a sidecar naming that face, then re-run faces to import it.
    write_region_sidecar(&photo, "Ayşe", &bbox, iw, ih);
    run(&[
        "faces",
        "--db",
        db_s,
        "--reprocess",
        "--min-cluster-size",
        "1",
        "--xmp",
        "file",
        "--silent",
    ]);

    // The imported name is now searchable, and lands on a confirmed face.
    let out = Command::new(bin())
        .args(["search", "--person", "Ayşe", "--db", db_s])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("couple.jpg"),
        "expected the imported person to be searchable; got: {stdout}"
    );

    let conn = Connection::open(&db).unwrap();
    let confirmed: i64 = conn
        .query_row(
            "SELECT count(*) FROM faces WHERE person_label = 'ayse' AND confirmed = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        confirmed, 1,
        "the region name should confirm exactly one face"
    );
}
