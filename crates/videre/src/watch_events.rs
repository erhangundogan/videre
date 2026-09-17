//! Turning debounced filesystem events into bounded work.
//!
//! One load-bearing rule: watch must never react to its own writes. Everything
//! videre writes into the watched tree is either under `.videre` (db, WAL,
//! locks, caches keyed there) or an `.xmp` sidecar written beside the media
//! file. Dropping both classes of event closes the feedback loop, so every
//! surviving path is genuine external input.

use notify::event::EventKind;
use notify::Event;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// False for anything videre writes into the watched tree: any `.videre`
/// component, and any `.xmp` sidecar (case-insensitive extension).
pub fn is_watchable_path(p: &Path) -> bool {
    if p.components().any(|c| c.as_os_str() == ".videre") {
        return false;
    }
    if let Some(ext) = p.extension() {
        if ext.eq_ignore_ascii_case("xmp") {
            return false;
        }
    }
    true
}

/// create/modify/rename materialise bytes worth scanning; remove/other
/// contribute nothing (orphan rows are the reconcile/prune's job, unchanged
/// from today). notify reports renames as `Modify(ModifyKind::Name(..))`,
/// which `Modify` already covers; if the installed version ever surfaces a
/// rename kind outside `Modify`, add it here plus a test rather than
/// widening to "everything".
fn is_materialising(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Create(_) | EventKind::Modify(_))
}

/// Deduped, sorted, filtered set of paths a debounced batch asks us to scan.
pub fn affected_paths<'a>(events: impl IntoIterator<Item = &'a Event>) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    for ev in events {
        if !is_materialising(&ev.kind) {
            continue;
        }
        for path in &ev.paths {
            if is_watchable_path(path) {
                set.insert(path.clone());
            }
        }
    }
    set.into_iter().collect()
}

/// True when the backend signalled it dropped events (queue overflow,
/// FSEvents must-rescan). The caller answers with one incremental
/// full-tree reconcile.
pub fn needs_full_rescan<'a>(events: impl IntoIterator<Item = &'a Event>) -> bool {
    events.into_iter().any(|e| e.need_rescan())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, EventKind, Flag, ModifyKind, RemoveKind};
    use notify::Event;
    use std::path::{Path, PathBuf};

    fn ev(kind: EventKind, paths: &[&str]) -> Event {
        Event {
            kind,
            paths: paths.iter().map(PathBuf::from).collect(),
            attrs: Default::default(),
        }
    }

    #[test]
    fn videre_state_and_xmp_are_never_watchable() {
        assert!(!is_watchable_path(Path::new("/lib/.videre/hashes.db")));
        assert!(!is_watchable_path(Path::new("/lib/.videre/locks/x.lock")));
        assert!(!is_watchable_path(Path::new("/lib/photos/IMG.jpg.xmp")));
        assert!(!is_watchable_path(Path::new("/lib/photos/IMG.JPG.XMP")));
        assert!(is_watchable_path(Path::new("/lib/photos/IMG.jpg")));
    }

    #[test]
    fn create_and_modify_contribute_paths_remove_does_not() {
        let events = [
            ev(EventKind::Create(CreateKind::File), &["/lib/a.jpg"]),
            ev(EventKind::Modify(ModifyKind::Any), &["/lib/b.jpg"]),
            ev(EventKind::Remove(RemoveKind::File), &["/lib/gone.jpg"]),
        ];
        let got = affected_paths(events.iter());
        assert_eq!(
            got,
            vec![PathBuf::from("/lib/a.jpg"), PathBuf::from("/lib/b.jpg")]
        );
    }

    #[test]
    fn a_batch_is_deduped_and_filtered() {
        let events = [
            ev(EventKind::Create(CreateKind::File), &["/lib/b.jpg"]),
            ev(EventKind::Modify(ModifyKind::Any), &["/lib/a.jpg"]),
            ev(EventKind::Modify(ModifyKind::Any), &["/lib/a.jpg"]),
            ev(EventKind::Modify(ModifyKind::Any), &["/lib/a.jpg.xmp"]),
            ev(
                EventKind::Create(CreateKind::File),
                &["/lib/.videre/hashes.db-wal"],
            ),
        ];
        assert_eq!(
            affected_paths(events.iter()),
            vec![PathBuf::from("/lib/a.jpg"), PathBuf::from("/lib/b.jpg")]
        );
    }

    #[test]
    fn a_rescan_flag_is_detected() {
        let plain = ev(EventKind::Create(CreateKind::File), &["/lib/a.jpg"]);
        assert!(!needs_full_rescan(std::iter::once(&plain)));
        let dropped = Event::new(EventKind::Any).set_flag(Flag::Rescan);
        assert!(needs_full_rescan(std::iter::once(&dropped)));
    }
}
