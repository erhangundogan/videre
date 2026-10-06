//! Google Takeout's file names: a creation sits beside its original.
//!
//! Google Photos exports both the photo it was given and what it made from
//! it, in the same folder: `a-edited.jpg` (an edit), `a-EFFECTS.jpg` (a
//! filter or crop it suggested) and `a-SMILE.jpg` (a face pasted smiling).
//! The bytes differ, and the pixels can be far apart, so neither content
//! identity nor a pixel comparison pairs them; the name is Google's own
//! statement that one was made from the other. Used by `import` (the sidecar
//! of a creation is its original's, and the hint after a Takeout import),
//! dedupe's `creation` kind and the `is:creation` query term.

use std::collections::HashMap;
use std::path::Path;

/// The suffixes Google writes, in the case it writes them.
const CREATIONS: [&str; 3] = ["-edited", "-EFFECTS", "-SMILE"];

/// `a-edited.jpg` -> `a.jpg`; `a-EFFECTS(1).jpg` -> `a(1).jpg`, since Google's
/// duplicate counter follows the suffix. `None` for a name that is not a
/// creation.
pub fn original_name(file_name: &str) -> Option<String> {
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (file_name, None),
    };
    let (stem, counter) = split_counter(stem);
    let base = CREATIONS
        .iter()
        .find_map(|suffix| stem.strip_suffix(suffix))
        .filter(|base| !base.is_empty())?;
    Some(match ext {
        Some(ext) => format!("{base}{counter}.{ext}"),
        None => format!("{base}{counter}"),
    })
}

/// Every name the original of a creation may have, most likely first:
/// [`original_name`], then the same without the creation's counter (a second
/// effect of one photo is `a-EFFECTS(1).jpg` beside `a.jpg`), each with the
/// extension as written, in upper case, then in lower case (Google writes
/// `a-SMILE.jpg` beside `a.JPG`). Empty for a name that is not a creation.
pub fn original_names(file_name: &str) -> Vec<String> {
    let Some(first) = original_name(file_name) else {
        return Vec::new();
    };
    let (stem, ext) = match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (file_name, None),
    };
    let (stem, counter) = split_counter(stem);
    let base = CREATIONS
        .iter()
        .find_map(|suffix| stem.strip_suffix(suffix))
        .unwrap_or(stem);
    let mut stems = vec![format!("{base}{counter}")];
    if !counter.is_empty() {
        stems.push(base.to_string());
    }
    let mut names = vec![first];
    for stem in stems {
        let exts = match ext {
            Some(e) => vec![
                Some(e.to_string()),
                Some(e.to_uppercase()),
                Some(e.to_lowercase()),
            ],
            None => vec![None],
        };
        for e in exts {
            let name = match e {
                Some(e) => format!("{stem}.{e}"),
                None => stem.clone(),
            };
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
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

/// A creation (an edit, effect or smile) and the original it was made from,
/// in the same folder. `edit` is the creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditedPair<'a> {
    pub edit: &'a str,
    pub original: &'a str,
}

/// The paths the same-folder original of `path` may have, most likely first,
/// when `path` names a creation (see [`original_names`]).
fn original_paths(path: &str) -> Vec<String> {
    let path = Path::new(path);
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    original_names(name)
        .into_iter()
        .map(|n| path.with_file_name(n).to_string_lossy().into_owned())
        .collect()
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
            let &(original, original_hash) = original_paths(edit)
                .iter()
                .find_map(|wanted| by_path.get(wanted.as_str()))?;
            (original_hash != edit_hash).then_some(EditedPair { edit, original })
        })
        .collect();
    pairs.sort_by(|a, b| a.edit.cmp(b.edit));
    pairs
}

/// How many of `paths` are creations whose original is among them, by name
/// alone.
/// For `import`, which has not hashed anything yet: identical bytes among
/// these are rare (none of 3,763 pairs in one real export).
pub fn edited_name_pairs<P: AsRef<Path>>(paths: &[P]) -> usize {
    let present: std::collections::HashSet<&Path> = paths.iter().map(|p| p.as_ref()).collect();
    paths
        .iter()
        .filter(|p| {
            p.as_ref().to_str().is_some_and(|p| {
                original_paths(p)
                    .iter()
                    .any(|original| present.contains(Path::new(original)))
            })
        })
        .count()
}

/// The content hashes of every creation in the library: `is:creation`.
pub fn creation_hashes(
    conn: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("SELECT path, hash FROM file_hashes")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let refs: Vec<(&str, &str)> = rows.iter().map(|(p, h)| (p.as_str(), h.as_str())).collect();
    let by_path: HashMap<&str, &str> = refs.iter().copied().collect();
    Ok(edited_pairs(&refs)
        .into_iter()
        .map(|pair| by_path[pair.edit].to_string())
        .collect())
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
    fn google_creations_name_their_original() {
        assert_eq!(
            original_name("IMG_0001-EFFECTS.jpg").as_deref(),
            Some("IMG_0001.jpg")
        );
        assert_eq!(
            original_name("IMG_0001-SMILE.jpg").as_deref(),
            Some("IMG_0001.jpg")
        );
        assert_eq!(
            original_name("Çiçek-EFFECTS(2).jpg").as_deref(),
            Some("Çiçek(2).jpg")
        );
        // Google writes the suffixes in this case only.
        assert_eq!(original_name("IMG_0001-effects.jpg"), None);
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

    /// Google writes a creation's extension in lower case beside an original
    /// in upper case, and a second creation of one photo takes a counter its
    /// original does not have.
    #[test]
    fn an_original_is_found_across_extension_case_and_a_creations_counter() {
        let rows = [
            ("d/DSC00295.JPG", "h1"),
            ("d/DSC00295-SMILE.jpg", "h2"),
            ("d/IMG_1108.JPG", "h3"),
            ("d/IMG_1108-edited(1).JPG", "h4"),
            ("d/Çiçek.jpg", "h5"),
            ("d/Çiçek-EFFECTS.jpg", "h6"),
            ("d/Çiçek-EFFECTS(1).jpg", "h7"),
            ("d/a(1).jpg", "h8"),
            ("d/a.jpg", "h9"),
            ("d/a-edited(1).jpg", "h10"),
        ];
        let pairs: Vec<(&str, &str)> = edited_pairs(&rows)
            .into_iter()
            .map(|p| (p.edit, p.original))
            .collect();
        assert_eq!(
            pairs,
            [
                ("d/DSC00295-SMILE.jpg", "d/DSC00295.JPG"),
                ("d/IMG_1108-edited(1).JPG", "d/IMG_1108.JPG"),
                // Its own counter's original first, when there is one.
                ("d/a-edited(1).jpg", "d/a(1).jpg"),
                ("d/Çiçek-EFFECTS(1).jpg", "d/Çiçek.jpg"),
                ("d/Çiçek-EFFECTS.jpg", "d/Çiçek.jpg"),
            ]
        );
        assert_eq!(
            edited_name_pairs(&rows.iter().map(|r| r.0).collect::<Vec<_>>()),
            5
        );
    }

    #[test]
    fn name_pairs_count_without_hashes() {
        let paths = ["d/a.jpg", "d/a-edited.jpg", "e/b-edited.jpg", "f/b.jpg"];
        assert_eq!(edited_name_pairs(&paths), 1);
    }
}
