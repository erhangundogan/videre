//! `videre faces` takes clustering parameters from its flags, then the
//! library's `gallery.json` (saved by the gallery's recluster), then the
//! built-in set, and says so in one line when `gallery.json` contributes.

mod common;

fn library_with_gallery_settings(settings: &str) -> common::TestLibrary {
    let lib = common::TestLibrary::new();
    lib.copy_fixture("tiny.jpg", "a.jpg");
    lib.scan();
    std::fs::write(lib.root.join(".videre/gallery.json"), settings).unwrap();
    lib
}

fn recluster(lib: &common::TestLibrary, extra: &[&str]) -> String {
    let out = lib
        .cmd()
        .args(["faces", "--recluster"])
        .args(extra)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    stderr
}

#[test]
fn saved_gallery_settings_reach_faces_and_are_named() {
    let lib = library_with_gallery_settings(
        r#"{ "faces": { "clustering": { "eps": 0.7, "min_cluster_size": 2 } } }"#,
    );
    let stderr = recluster(&lib, &[]);
    assert!(
        stderr.contains(
            "Clustering with gallery settings: eps 0.7, min_cluster_size 2 (from .videre/gallery.json)"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("eps=0.70"),
        "the pass runs with the saved eps: {stderr}"
    );
}

#[test]
fn a_flag_overrides_gallery_settings_and_the_line_says_so() {
    let lib = library_with_gallery_settings(r#"{ "faces": { "clustering": { "eps": 0.7 } } }"#);
    let stderr = recluster(&lib, &["--eps", "0.65"]);
    assert!(
        stderr.contains(
            "Clustering flags override gallery settings: eps 0.65 (gallery.json has 0.7)"
        ),
        "{stderr}"
    );
}

#[test]
fn no_line_without_gallery_settings_or_under_silent() {
    let lib = library_with_gallery_settings(r#"{ "routes": { "people": { "align": "top" } } }"#);
    assert!(!recluster(&lib, &[]).contains("gallery settings"));

    let lib = library_with_gallery_settings(r#"{ "faces": { "clustering": { "eps": 0.7 } } }"#);
    assert!(!recluster(&lib, &["--silent"]).contains("gallery settings"));
}

#[test]
fn a_broken_gallery_json_warns_and_does_not_stop_the_run() {
    let lib = library_with_gallery_settings("{oops");
    let stderr = recluster(&lib, &[]);
    assert!(
        stderr.contains("gallery clustering settings not read"),
        "{stderr}"
    );
}
