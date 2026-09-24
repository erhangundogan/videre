//! The defaults listed in the gallery docs must be the defaults the binary
//! ships, or the page quietly goes stale after the next change to either.

use std::path::Path;

#[test]
fn documented_defaults_match_the_shipped_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let doc =
        std::fs::read_to_string(root.join("docs/src/content/docs/commands/gallery.md")).unwrap();
    let marker = "<!-- gallery-defaults -->";
    let after = &doc[doc.find(marker).expect("marker in gallery.md") + marker.len()..];
    let fence = "```json\n";
    let start = after.find(fence).expect("a json block after the marker") + fence.len();
    let block = &after[start..start + after[start..].find("```").unwrap()];
    let shipped =
        std::fs::read_to_string(root.join("crates/videre/static/gallery-defaults.json")).unwrap();
    let documented: serde_json::Value = serde_json::from_str(block).unwrap();
    let shipped: serde_json::Value = serde_json::from_str(&shipped).unwrap();
    assert_eq!(
        documented, shipped,
        "update the defaults block in docs/src/content/docs/commands/gallery.md"
    );
}
