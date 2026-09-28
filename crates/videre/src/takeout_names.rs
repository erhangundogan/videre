//! Google Takeout's file names: an `-edited` render sits beside its original.
//!
//! Google Photos exports both the photo it was given and every edit made to
//! it, as `a.jpg` and `a-edited.jpg` in the same folder. The bytes differ, so
//! content identity never pairs them; the name is Google's own statement that
//! one is an edit of the other. Used by `import` (the sidecar of an edit is its
//! original's, and the hint after a Takeout import), `dedupe --edited`, and the
//! gallery's duplicate review.

use std::collections::HashMap;
use std::path::Path;

const EDITED: &str = "-edited";

/// `a-edited.jpg` -> `a.jpg`; `a-edited(1).jpg` -> `a(1).jpg`, since Google's
/// duplicate counter follows the suffix. `None` for a name that is not an edit.
pub fn original_name(file_name: &str) -> Option<String> {
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (file_name, None),
    };
    let (stem, counter) = split_counter(stem);
    let base = stem.strip_suffix(EDITED).filter(|base| !base.is_empty())?;
    Some(match ext {
        Some(ext) => format!("{base}{counter}.{ext}"),
        None => format!("{base}{counter}"),
    })
}

/// `a-edited(1)` -> (`a-edited`, `(1)`); anything else is returned whole.
fn split_counter(stem: &str) -> (&str, &str) {
    if let Some(open) = stem.strip_suffix(')').and_then(|s| s.rfind('(')) {
        let digits = &stem[open + 1..stem.len() - 1];
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return (&stem[..open], &stem[open..]);
        }
    }
    (stem, "")
}

/// An edit and the original it was made from, in the same folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditedPair<'a> {
    pub edit: &'a str,
    pub original: &'a str,
}

/// The same-folder original of `path`, when `path` names an edit.
fn original_path(path: &str) -> Option<String> {
    let path = Path::new(path);
    let name = path.file_name()?.to_str()?;
    let original = original_name(name)?;
    Some(path.with_file_name(original).to_string_lossy().into_owned())
}

/// Every pair among `(path, hash)` rows: the name rule above, same folder,
/// both present, and different hashes (identical bytes are already exact
/// duplicates). Sorted by the edit's path.
pub fn edited_pairs<'a>(rows: &[(&'a str, &'a str)]) -> Vec<EditedPair<'a>> {
    let by_path: HashMap<&str, (&'a str, &'a str)> = rows
        .iter()
        .map(|&(path, hash)| (path, (path, hash)))
        .collect();
    let mut pairs: Vec<EditedPair<'a>> = rows
        .iter()
        .filter_map(|&(edit, edit_hash)| {
            let wanted = original_path(edit)?;
            let &(original, original_hash) = by_path.get(wanted.as_str())?;
            (original_hash != edit_hash).then_some(EditedPair { edit, original })
        })
        .collect();
    pairs.sort_by(|a, b| a.edit.cmp(b.edit));
    pairs
}

/// How many of `paths` are edits whose original is among them, by name alone.
/// For `import`, which has not hashed anything yet: identical bytes among
/// these are rare (none of 3,763 pairs in one real export).
pub fn edited_name_pairs<P: AsRef<Path>>(paths: &[P]) -> usize {
    let present: std::collections::HashSet<&Path> = paths.iter().map(|p| p.as_ref()).collect();
    paths
        .iter()
        .filter_map(|p| p.as_ref().to_str().and_then(original_path))
        .filter(|original| present.contains(Path::new(original)))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_edit_names_its_original() {
        assert_eq!(original_name("a-edited.jpg").as_deref(), Some("a.jpg"));
        assert_eq!(
            original_name("a-edited(1).jpg").as_deref(),
            Some("a(1).jpg")
        );
        assert_eq!(original_name("a-edited").as_deref(), Some("a"));
        assert_eq!(
            original_name("Pazar öğleden sonra-edited.JPG").as_deref(),
            Some("Pazar öğleden sonra.JPG")
        );
    }

    #[test]
    fn other_names_are_not_edits() {
        for name in [
            "a.jpg",
            "-edited.jpg",
            "a-edited-x.jpg",
            "a-edited(x).jpg",
            "a(1).jpg",
        ] {
            assert_eq!(original_name(name), None, "{name}");
        }
    }

    #[test]
    fn pairs_are_same_folder_and_different_bytes() {
        let rows = [
            ("d/a.jpg", "h1"),
            ("d/a-edited.jpg", "h2"),
            ("e/b-edited.jpg", "h3"),
            ("f/b.jpg", "h4"),
            ("d/c-edited.jpg", "h5"),
            ("d/c.jpg", "h5"),
            ("d/lonely-edited.jpg", "h6"),
        ];
        assert_eq!(
            edited_pairs(&rows),
            vec![EditedPair {
                edit: "d/a-edited.jpg",
                original: "d/a.jpg"
            }]
        );
    }

    #[test]
    fn name_pairs_count_without_hashes() {
        let paths = ["d/a.jpg", "d/a-edited.jpg", "e/b-edited.jpg", "f/b.jpg"];
        assert_eq!(edited_name_pairs(&paths), 1);
    }
}
