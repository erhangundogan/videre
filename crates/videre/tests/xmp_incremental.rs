//! Incremental XMP reconcile: a file's marks are imported the first time it is
//! seen and when its sidecar changes, and skipped otherwise. The "Reading
//! metadata for N file(s)" stderr line is the observable signal for how many
//! files a run actually reconciled.

mod common;
use common::TestLibrary;

fn scan_stderr(lib: &TestLibrary, args: &[&str]) -> String {
    let out = lib.cmd().args(args).output().expect("run videre");
    assert!(
        out.status.success(),
        "videre {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn write_sidecar(lib: &TestLibrary, rel: &str, rating: i64) {
    std::fs::write(
        lib.root.join(rel),
        format!(
            r#"<?xpacket begin="?"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF
 xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
 xmlns:xmp="http://ns.adobe.com/xap/1.0/">
 <rdf:Description xmp:Rating="{rating}"/>
</rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#
        ),
    )
    .expect("write sidecar");
}

fn rating_matches(lib: &TestLibrary, rating: &str) -> bool {
    let out = lib
        .cmd()
        .args(["search", "--rating", rating])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains("IMG.jpg")
}

#[test]
fn second_scan_reconciles_nothing_when_unchanged() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    write_sidecar(&lib, "photos/IMG.jpg.xmp", 3);

    // First scan imports the rating and reports one file reconciled.
    let first = scan_stderr(&lib, &["scan"]);
    assert!(first.contains("Reading metadata for 1 file"), "got: {first}");
    assert!(rating_matches(&lib, "3"));

    // Second scan, nothing changed: no metadata reading phase at all.
    let second = scan_stderr(&lib, &["scan"]);
    assert!(
        !second.contains("Reading metadata"),
        "an unchanged rescan must not re-read any XMP; got: {second}"
    );
}

#[test]
fn a_sidecar_appearing_after_the_first_scan_is_imported_on_rescan() {
    // The media is unchanged (not re-hashed), so this exercises the sidecar-only
    // path: a new sidecar appears, its mtime differs from the stored "no sidecar"
    // state, and its marks are imported without any full-media read. Under the
    // default `--xmp db` precedence the DB has no rating yet, so the file wins.
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg"); // no sidecar at first scan
    lib.scan();
    assert!(!rating_matches(&lib, "4"), "no rating before the sidecar exists");

    // The sidecar appears; the media file is untouched.
    write_sidecar(&lib, "photos/IMG.jpg.xmp", 4);
    let out = scan_stderr(&lib, &["scan"]);
    assert!(out.contains("Reading metadata for 1 file"), "got: {out}");
    assert!(
        rating_matches(&lib, "4"),
        "a newly appeared sidecar must be imported on the next scan"
    );

    // And once imported, a further unchanged rescan reads nothing again.
    let quiet = scan_stderr(&lib, &["scan"]);
    assert!(!quiet.contains("Reading metadata"), "got: {quiet}");
}

#[test]
fn sidecarless_file_is_skipped_on_the_second_scan() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg"); // no sidecar
                                                    // First scan records the "no sidecar" state.
    lib.scan();
    // Second scan must skip it: no metadata reading phase.
    let second = scan_stderr(&lib, &["scan"]);
    assert!(
        !second.contains("Reading metadata"),
        "a sidecar-less file must be skipped on rescan; got: {second}"
    );
}

#[test]
fn xmp_file_forces_a_full_reconcile_even_when_unchanged() {
    let lib = TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "photos/IMG.jpg");
    write_sidecar(&lib, "photos/IMG.jpg.xmp", 3);
    lib.scan();

    // Override in the DB.
    lib.cmd()
        .args(["mark", "--path", "photos", "--rating", "5", "--silent"])
        .output()
        .unwrap();
    assert!(rating_matches(&lib, "5"));

    // The sidecar is unchanged, so default db would skip; --xmp file must still
    // reconcile and revert to 3.
    let out = scan_stderr(&lib, &["scan", "--xmp", "file"]);
    assert!(out.contains("Reading metadata for 1 file"), "got: {out}");
    assert!(
        rating_matches(&lib, "3"),
        "--xmp file must revert to the sidecar value"
    );
}
