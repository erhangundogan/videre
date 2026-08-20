//! The changelog must be correct for the version being released.
//!
//! Two failures happened in the same file within a day, both silent on the
//! rendered page and both only visible once something automated touched it.
//!
//! `CHANGELOG.md` contained **two** `## [Unreleased]` headings: the real one,
//! and a block of fixes written for 0.13.0 that was never filed under it. The
//! 0.18.0 release renamed headings with a `sed` on `^## \[Unreleased\]$`, which
//! matched both, so a year-old section was published as part of 0.18.0.
//! Separately, the reference links that make each heading a link to its diff
//! had not been maintained since 0.12.0.
//!
//! :warning: **Backfilling the historical links is deliberately not required.**
//! Sixteen releases between 0.12.1 and 0.15.10 render as literal bracketed text
//! and will stay that way: decided 2026-08-20, on the grounds that old
//! changelog entries are not worth the archaeology. This file therefore checks
//! **only the version currently being shipped**, plus the structural mistake
//! that corrupts entries which were already correct. Do not "improve" it into
//! asserting every heading has a link; that reopens a decision that was made.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn changelog() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md")
}

fn text() -> String {
    let p = changelog();
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// The `x.y.z` of every `## [x.y.z] - date` heading, in file order.
fn headings(s: &str) -> Vec<String> {
    s.lines()
        .filter_map(|l| l.strip_prefix("## ["))
        .filter_map(|l| l.split(']').next())
        .map(str::to_string)
        .collect()
}

/// A heading may appear once. Twice means a later rename will hit both.
#[test]
fn no_version_appears_twice() {
    let s = text();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for h in headings(&s) {
        *seen.entry(h).or_default() += 1;
    }
    let dupes: Vec<_> = seen.iter().filter(|(_, &n)| n > 1).map(|(v, n)| format!("{v} x{n}")).collect();
    assert!(
        dupes.is_empty(),
        "these headings appear more than once in CHANGELOG.md: {}.\n\
         A duplicate is invisible on the rendered page and only becomes wrong when \
         a release renames headings, which is how a 0.13.0 section shipped as part \
         of 0.18.0.",
        dupes.join(", ")
    );
}

/// The version in Cargo.toml must have an entry, or a release ships undescribed.
#[test]
fn the_version_being_shipped_has_an_entry() {
    let s = text();
    let v = env!("CARGO_PKG_VERSION");
    // An unreleased working tree is the normal state between releases: the
    // section is still headed Unreleased and gets renamed at release time.
    let released = headings(&s).iter().any(|h| h == v);
    let pending = s.contains("## [Unreleased]");
    assert!(
        released || pending,
        "CHANGELOG.md has neither a `## [{v}]` heading nor an `## [Unreleased]` \
         section, so version {v} would ship with nothing describing it."
    );
}

/// A released version needs its reference link, or the heading renders as
/// literal brackets. This is the check that stops the gap growing further.
#[test]
fn a_released_version_has_its_reference_link() {
    let s = text();
    let v = env!("CARGO_PKG_VERSION");
    if !headings(&s).iter().any(|h| h == v) {
        return; // still unreleased; covered by the test above
    }
    assert!(
        s.contains(&format!("\n[{v}]: ")),
        "CHANGELOG.md has a `## [{v}]` heading but no `[{v}]: ...compare/...` \
         reference at the bottom, so it renders as literal text instead of a \
         link to the diff. Add one following the pattern of the entry above it."
    );
}

/// `[Unreleased]` and a released heading cannot both be the newest thing.
#[test]
fn unreleased_carries_its_own_link_when_present() {
    let s = text();
    if !s.contains("## [Unreleased]") {
        return;
    }
    assert!(
        s.contains("\n[Unreleased]: "),
        "CHANGELOG.md has an `## [Unreleased]` section with no `[Unreleased]: ` \
         reference, so the heading renders as literal text."
    );
}
