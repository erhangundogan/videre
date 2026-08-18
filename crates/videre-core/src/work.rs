//! One place that decides whether there is work, and therefore whether a model
//! is ever loaded.
//!
//! `embed`, `classify` and `faces` each used to hand-write the same shape:
//! compute a pending set, return early with a message if it is empty, narrow it
//! by the selection, print `N of M`, return early again if that emptied it, and
//! only then load a model.
//!
//! Three copies meant no single test could cover the behaviour, and one of them
//! was wrong: `classify`'s early return was never actually taken, so the command
//! reached `Embedder::load` and downloaded 778MB of model weights from inside a
//! unit test. On CI that woke an inference test which had always skipped, and
//! took the Ubuntu job from ~3 minutes to nearly 40.
//!
//! `with_work` is the structural half of the fix: the model load lives inside a
//! closure that only runs when there is work, so "nothing to do" cannot reach a
//! download however wrong a future guard is.

use crate::selection::{RowSelection, SelectionCtx};
use anyhow::Result;
use rusqlite::Connection;

/// A pending set that survived the selection, plus how large it was before.
///
/// `eligible` is kept so the caller can say `N of M`. Without the denominator a
/// filter that matched nothing and an empty library look identical.
pub struct Pending<T> {
    pub items: Vec<T>,
    pub eligible: usize,
}

/// Either there is work, or there is a reason there is not.
pub enum Work<T> {
    /// Nothing to do. Carries the message to show, already assembled.
    Nothing(String),
    Some(Pending<T>),
}

/// The verb a command uses for its own work.
///
/// Deliberately no item noun. `embed` counted "pending file(s)", `classify`
/// "pending hash(es)" and `faces` paths, which is three names for one idea and
/// a difference no user cares about. They are all `item(s)` now; carrying the
/// distinction as a parameter would have preserved the divergence and called it
/// configuration.
#[derive(Clone, Copy)]
pub struct Words {
    /// Lowercase, as it appears mid-sentence: `embed`, `classify`, `process`.
    pub verb: &'static str,
    /// Capitalised, as it starts a line: `Embedding`, `Classifying`.
    pub gerund: &'static str,
}

impl Words {
    pub const fn new(verb: &'static str, gerund: &'static str) -> Self {
        Words { verb, gerund }
    }
}

/// Narrows `pending` by `selection`, reporting as it goes.
///
/// Returns `Work::Nothing` when there is nothing to do, either because the
/// pending set was empty to begin with or because the selection emptied it. The
/// two cases carry different messages, since "you are up to date" and "your
/// filter matched nothing" call for different reactions from the reader.
///
/// `hash_of` reads an item's hash, so this works for any pending type: `embed`
/// passes rows, `faces` passes paths.
pub fn narrow<T>(
    pending: Vec<T>,
    hash_of: impl Fn(&T) -> &str,
    selection: &RowSelection,
    conn: &Connection,
    ctx: &SelectionCtx,
    words: Words,
    silent: bool,
) -> Result<Work<T>> {
    if pending.is_empty() {
        return Ok(Work::Nothing(format!(
            "Nothing to {}: everything eligible is already done.",
            words.verb
        )));
    }

    let eligible = pending.len();
    let items = if selection.is_empty() {
        pending
    } else {
        let resolved = selection.resolve(conn, ctx)?;
        match resolved.hashes {
            // `None` means the selection put no constraint on hashes, so
            // everything pending survives. It does NOT mean "matched nothing":
            // collapsing the two would turn a typo into a full-library run.
            None => pending,
            Some(h) => pending
                .into_iter()
                .filter(|item| h.contains(hash_of(item)))
                .collect(),
        }
    };

    if !selection.is_empty() && !silent {
        // Said before the work, not after. A command that quietly processes a
        // fraction of the library is the truncation bug of 0.14.1 with a much
        // longer feedback loop.
        eprintln!(
            "{} {} of {} pending item(s) ({})",
            words.gerund,
            items.len(),
            eligible,
            selection.describe()
        );
    }

    if items.is_empty() {
        return Ok(Work::Nothing(format!(
            "Nothing to {}: the selection matched nothing pending.",
            words.verb
        )));
    }

    Ok(Work::Some(Pending { items, eligible }))
}

/// Runs `f` only when there is work, printing the reason when there is not.
///
/// This is the point of the module. A caller cannot load a model unless it is
/// inside `f`, so no future command can reach a 778MB download by getting a
/// guard wrong: there is no code path from `Work::Nothing` to the closure.
///
/// Returns `None` when `f` did not run, for callers that need to tell the
/// difference. Most do not and can ignore it.
pub fn with_work<T, R>(
    work: Work<T>,
    silent: bool,
    f: impl FnOnce(Pending<T>) -> Result<R>,
) -> Result<Option<R>> {
    match work {
        Work::Nothing(msg) => {
            if !silent {
                eprintln!("{msg}");
            }
            Ok(None)
        }
        Work::Some(pending) => f(pending).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const W: Words = Words::new("embed", "Embedding");

    fn conn() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    /// A library with one jpg and one mov, so a selection can actually match
    /// and actually miss. Mirrors the fixture in `selection.rs`.
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE file_hashes (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, size_bytes INTEGER,
                created_at TEXT, modified_at TEXT, ext TEXT, mime TEXT, phash INTEGER,
                exif_date TEXT, gps_lat REAL, gps_lon REAL, width INTEGER, height INTEGER);
             INSERT INTO file_hashes (path, hash, ext, mime) VALUES
               ('/lib/a.jpg','h_jpg','jpg','image/jpeg'),
               ('/lib/b.mov','h_mov','mov','video/quicktime');",
        )
        .unwrap();
        c
    }

    fn hash(s: &String) -> &str {
        s.as_str()
    }

    #[test]
    fn an_empty_pending_set_is_nothing_to_do() {
        let c = conn();
        let w = narrow(
            Vec::<String>::new(),
            hash,
            &RowSelection::default(),
            &c,
            &SelectionCtx::default(),
            W,
            true,
        )
        .unwrap();
        match w {
            Work::Nothing(m) => {
                assert_eq!(m, "Nothing to embed: everything eligible is already done.")
            }
            Work::Some(_) => panic!("an empty pending set must not be work"),
        }
    }

    #[test]
    fn no_selection_leaves_the_pending_set_untouched() {
        let c = conn();
        let w = narrow(
            vec!["a".to_string(), "b".to_string()],
            hash,
            &RowSelection::default(),
            &c,
            &SelectionCtx::default(),
            W,
            true,
        )
        .unwrap();
        match w {
            Work::Some(p) => {
                assert_eq!(p.items.len(), 2);
                assert_eq!(p.eligible, 2, "eligible is the count before narrowing");
            }
            Work::Nothing(m) => panic!("unfiltered work was dropped: {m}"),
        }
    }

    #[test]
    fn a_selection_that_matches_nothing_is_nothing_to_do() {
        // The pending set is non-empty and the filter excludes all of it. This
        // must report "your filter matched nothing", not "you are up to date":
        // the two call for opposite reactions from the reader.
        let c = db();
        let mut s = RowSelection::default();
        s.exts = vec!["png".to_string()]; // present in neither row
        let w = narrow(
            vec!["h_jpg".to_string(), "h_mov".to_string()],
            hash,
            &s,
            &c,
            &SelectionCtx::default(),
            W,
            true,
        )
        .unwrap();
        match w {
            Work::Nothing(m) => {
                assert_eq!(
                    m,
                    "Nothing to embed: the selection matched nothing pending."
                )
            }
            Work::Some(p) => panic!("{} item(s) survived a filter matching none", p.items.len()),
        }
    }

    #[test]
    fn a_selection_keeps_only_what_it_matched() {
        let c = db();
        let mut s = RowSelection::default();
        s.exts = vec!["jpg".to_string()];
        let w = narrow(
            vec!["h_jpg".to_string(), "h_mov".to_string()],
            hash,
            &s,
            &c,
            &SelectionCtx::default(),
            W,
            true,
        )
        .unwrap();
        match w {
            Work::Some(p) => {
                assert_eq!(p.items, vec!["h_jpg".to_string()]);
                assert_eq!(p.eligible, 2, "the denominator is the pre-filter count");
            }
            Work::Nothing(m) => panic!("a matching filter dropped everything: {m}"),
        }
    }

    #[test]
    fn the_closure_never_runs_when_there_is_nothing_to_do() {
        // The regression guard for the incident this module exists to prevent.
        // The model load lives inside the closure, so this assertion is what
        // makes a download unreachable with nothing to process.
        let ran = Cell::new(false);
        let out = with_work(Work::<String>::Nothing("nothing".into()), true, |_| {
            ran.set(true);
            Ok(())
        })
        .unwrap();
        assert!(
            !ran.get(),
            "no work must mean no closure, and so no model load"
        );
        assert!(out.is_none());
    }

    #[test]
    fn the_closure_runs_and_returns_its_value_when_there_is_work() {
        let ran = Cell::new(false);
        let out = with_work(
            Work::Some(Pending {
                items: vec!["a".to_string()],
                eligible: 1,
            }),
            true,
            |p| {
                ran.set(true);
                Ok(p.items.len())
            },
        )
        .unwrap();
        assert!(ran.get());
        assert_eq!(out, Some(1));
    }

    #[test]
    fn an_error_from_the_closure_is_not_swallowed() {
        let out = with_work(
            Work::Some(Pending {
                items: vec!["a".to_string()],
                eligible: 1,
            }),
            true,
            |_| -> Result<()> { anyhow::bail!("boom") },
        );
        assert!(
            out.is_err(),
            "the closure's failure is the caller's failure"
        );
    }
}
