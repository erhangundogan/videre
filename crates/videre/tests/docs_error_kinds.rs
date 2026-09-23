//! Every error kind a log line can carry is documented in the logging guide,
//! so a new kind cannot ship without telling users what it means.

#[test]
fn every_error_kind_is_documented() {
    let guide = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/src/content/docs/guides/logging-and-errors.md"
    ))
    .unwrap();
    for kind in videre_core::error_kind::ErrorKind::ALL {
        assert!(
            guide.contains(&format!("| `{}` |", kind.code())),
            "{} is missing from the guide's error kinds table",
            kind.code()
        );
    }
}
