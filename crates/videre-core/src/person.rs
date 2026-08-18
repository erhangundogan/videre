//! Two forms of a person's name: one to identify them by, one to show.
//!
//! A person used to be a single string on every face row, compared with `=`.
//! That made `alice` and `Alice` two different people: person search returned
//! half the photos and nothing in the interface could explain why.
//!
//! So a name now has two forms. [`normalize`] produces the identity - lowercase,
//! ASCII, no punctuation - which is what gets stored, matched and put in a URL.
//! [`display_name`] keeps what was typed, which is what a reader should see.
//! `Erhan` and `erhan` normalize to the same identity and are therefore the same
//! person by construction, rather than by a comparison rule every call site has
//! to remember.

/// Turkish letters, mapped before Unicode casing gets a chance to.
///
/// `to_lowercase` is Unicode-default and not locale-aware, which matters most
/// for the dotted and dotless I: `İ` lowercases to `i` plus a combining dot
/// rather than to `i`, and `I` lowercases to `i` where Turkish would say `ı`.
/// Mapping these explicitly means the result does not depend on which of those
/// two conventions the standard library happens to implement.
const TURKISH: [(char, char); 12] = [
    ('ı', 'i'),
    ('İ', 'i'),
    ('ğ', 'g'),
    ('Ğ', 'g'),
    ('ş', 's'),
    ('Ş', 's'),
    ('ö', 'o'),
    ('Ö', 'o'),
    ('ü', 'u'),
    ('Ü', 'u'),
    ('ç', 'c'),
    ('Ç', 'c'),
];

/// Latin-1 and Latin Extended-A letters that carry a diacritic, folded to the
/// letter underneath.
///
/// Folding rather than stripping is the whole point: dropping the character
/// instead would turn `Şefik` into `efik` and `Çağdaş` into `ada`, eating the
/// first letter of any name that starts with one. Measured on a real library,
/// 14 of 85 names were affected.
fn fold(c: char) -> Option<char> {
    if let Some((_, to)) = TURKISH.iter().find(|(from, _)| *from == c) {
        return Some(*to);
    }
    Some(match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' | 'Ā' | 'Ă' | 'Ą' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
        'É' | 'È' | 'Ê' | 'Ë' | 'Ē' | 'Ė' | 'Ę' | 'Ě' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'ī' | 'į' => 'i',
        'Í' | 'Ì' | 'Î' | 'Ï' | 'Ī' | 'Į' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ø' | 'ō' => 'o',
        'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ø' | 'Ō' => 'o',
        'ú' | 'ù' | 'û' | 'ū' | 'ů' => 'u',
        'Ú' | 'Ù' | 'Û' | 'Ū' | 'Ů' => 'u',
        'ñ' | 'ń' | 'ň' => 'n',
        'Ñ' | 'Ń' | 'Ň' => 'n',
        'ý' | 'ÿ' => 'y',
        'Ý' | 'Ÿ' => 'y',
        'ć' | 'č' => 'c',
        'Ć' | 'Č' => 'c',
        'ś' | 'š' => 's',
        'Ś' | 'Š' => 's',
        'ź' | 'ż' | 'ž' => 'z',
        'Ź' | 'Ż' | 'Ž' => 'z',
        'ł' => 'l',
        'Ł' => 'l',
        'đ' | 'ð' => 'd',
        'Đ' => 'd',
        'ß' => 's',
        _ => return None,
    })
}

/// The identity form of a name: what gets stored, matched, and put in a URL.
///
/// Trim, fold diacritics to ASCII, lowercase, spaces to `_`, then drop anything
/// left outside `[a-z0-9_]`. Punctuation goes - `!#$%^&?~|{}[]-=` and the rest -
/// so `Anne-Marie` becomes `annemarie`; the hyphen survives in
/// [`display_name`].
///
/// Returns `None` when nothing usable remains, which is the caller's cue to
/// reject the input rather than store an empty identity.
///
/// ```
/// use videre_core::person::normalize;
/// assert_eq!(normalize("Işıl Özyeğin").as_deref(), Some("isil_ozyegin"));
/// assert_eq!(normalize("  Erhan  ").as_deref(), Some("erhan"));
/// assert_eq!(normalize("!!!"), None);
/// ```
pub fn normalize(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_sep = true; // leading separators are dropped

    for ch in raw.trim().chars() {
        // Fold first, so casing never sees a character it would treat by a
        // different convention than the one this function promises.
        let ch = fold(ch).unwrap_or(ch);

        // `_` is a separator on the way in as well as out. Reads normalize
        // too, so this runs on values that are already identities: dropping the
        // underscore would turn `isil_ozyegin` into `isilozyegin` and every
        // multi-word person URL would resolve to nothing.
        if ch.is_whitespace() || ch == '_' {
            if !last_was_sep {
                out.push('_');
                last_was_sep = true;
            }
            continue;
        }
        // `to_lowercase` yields a sequence: `İ` without the mapping above would
        // give two chars. The fold has already handled the cases that matter,
        // and taking every char keeps this correct for anything it has not.
        for lower in ch.to_lowercase() {
            if lower.is_ascii_alphanumeric() {
                out.push(lower);
                last_was_sep = false;
            }
            // Everything else - punctuation, and any diacritic that survived
            // folding, such as a combining mark - is dropped.
        }
    }

    while out.ends_with('_') {
        out.pop();
    }
    (!out.is_empty()).then_some(out)
}

/// The display form: what was typed, tidied but not transformed.
///
/// Trims and collapses internal whitespace, so `"Ahmet   Ari"` becomes
/// `"Ahmet Ari"`, and caps length the same way the labeling UI does. Case,
/// diacritics and punctuation are all preserved: this is what a reader sees.
pub fn display_name(raw: &str) -> Option<String> {
    let filtered: String = raw
        .chars()
        .filter(|c| !c.is_control() && !is_disallowed_format_char(*c))
        .collect();
    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(60).collect())
}

/// Bidi and zero-width format characters that let a name reorder or hide the
/// text around it when rendered.
///
/// U+200C (ZWNJ) and U+200D (ZWJ) are deliberately kept: Persian and Indic text
/// require them, and emoji ZWJ sequences are built from them.
fn is_disallowed_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'
            | '\u{200E}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Every identity a typed name should match.
///
/// A person has two names and a user may type either: the identity from the
/// URL (`erhan_gundogan`) or the display name from the screen
/// (`Erhan Gündoğan`). They agree until someone renames a person, and after
/// that only this lookup connects them.
///
/// :warning: **The display comparison happens here, in Rust, and must not be
/// pushed into SQL.** SQLite's `LOWER()` is ASCII-only - it leaves `Ö` alone
/// while Rust produces `ö` - so `LOWER(full_name) = ?` silently matched no
/// name containing a Turkish character, which is most of the names this was
/// built for. It surfaces only after a rename, when the two forms first
/// disagree, so it passes every check made before one.
///
/// Returns the normalized query first, then any identity whose display name
/// matches it. `people` holds one row per person, so reading it whole is
/// cheaper than being clever.
pub fn resolve_identities(
    conn: &rusqlite::Connection,
    typed_name: &str,
) -> rusqlite::Result<Vec<String>> {
    let normalized = normalize(typed_name).unwrap_or_else(|| typed_name.to_string());
    let typed = typed_name.trim().to_lowercase();

    let mut identities = vec![normalized.clone()];
    if crate::db::table_exists(conn, "people").unwrap_or(false) {
        let mut stmt = conn.prepare("SELECT name, full_name FROM people")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (identity, full) = row?;
            let matches_display = full.trim().to_lowercase() == typed
                || normalize(&full).as_deref() == Some(normalized.as_str());
            if matches_display && !identities.contains(&identity) {
                identities.push(identity);
            }
        }
    }
    Ok(identities)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every case here is a real name from the library this was built for, or a
    /// deliberate edge of the rule. Predicted values were verified against a
    /// reference implementation before the plan was written.
    #[test]
    fn names_normalize_the_way_the_spec_says() {
        let cases = [
            ("  Erhan  ", "erhan", "Erhan"),
            ("erhan", "erhan", "erhan"),
            ("ERHAN", "erhan", "ERHAN"),
            ("Ayşegül", "aysegul", "Ayşegül"),
            ("Şefik", "sefik", "Şefik"),
            ("Çağdaş", "cagdas", "Çağdaş"),
            ("Ömercan", "omercan", "Ömercan"),
            ("Sertuğ", "sertug", "Sertuğ"),
            ("Serdar Başaran", "serdar_basaran", "Serdar Başaran"),
            ("Ahmet   Arı", "ahmet_ari", "Ahmet Arı"),
            ("Anne-Marie", "annemarie", "Anne-Marie"),
        ];
        for (input, name, display) in cases {
            assert_eq!(
                normalize(input).as_deref(),
                Some(name),
                "normalize({input:?})"
            );
            assert_eq!(
                display_name(input).as_deref(),
                Some(display),
                "display_name({input:?})"
            );
        }
    }

    /// The dotted and dotless I, where Unicode-default casing and Turkish
    /// disagree, and where this would otherwise be right only by accident.
    ///
    /// `İ` lowercases to `i` plus a combining dot under Unicode rules, so
    /// without the explicit mapping the identity would depend on whether the
    /// combining mark happened to be dropped later. `I` lowercases to `i` where
    /// Turkish would say `ı`; `i` is what is wanted here, and it is pinned so
    /// nobody "fixes" it into locale-aware casing without seeing this.
    #[test]
    fn the_dotted_and_dotless_i_are_pinned() {
        for (input, want) in [
            ("İrfan", "irfan"),
            ("Irmak", "irmak"),
            ("ICE", "ice"),
            ("Işıl", "isil"),
            ("Işıl Özyeğin", "isil_ozyegin"),
            ("ışık", "isik"),
        ] {
            assert_eq!(
                normalize(input).as_deref(),
                Some(want),
                "normalize({input:?})"
            );
        }
    }

    /// The full Turkish set, given as reference by the user 2026-08-18:
    /// `öÖüÜıIiİşŞçÇğĞ`.
    ///
    /// It includes plain `I` and `i` on purpose, because Turkish pairs them
    /// differently from English: `I` is the capital of dotless `ı`, and `İ` is
    /// the capital of dotted `i`. Those two pairs are the only reason this
    /// function maps letters explicitly instead of trusting `to_lowercase`.
    #[test]
    fn the_whole_turkish_alphabet_folds_to_ascii() {
        assert_eq!(
            normalize("öÖüÜıIiİşŞçÇğĞ").as_deref(),
            Some("oouuiiiissccgg")
        );
        // Each pair on its own, so a failure says which letter broke.
        for (input, want) in [
            ("öÖ", "oo"),
            ("üÜ", "uu"),
            ("ıI", "ii"),
            ("iİ", "ii"),
            ("şŞ", "ss"),
            ("çÇ", "cc"),
            ("ğĞ", "gg"),
        ] {
            assert_eq!(normalize(input).as_deref(), Some(want), "pair {input:?}");
        }
    }

    #[test]
    fn case_differences_collapse_to_one_identity() {
        // The bug this exists to fix.
        let forms = ["Erhan", "erhan", "ERHAN", "  eRhAn "];
        let ids: Vec<_> = forms.iter().filter_map(|f| normalize(f)).collect();
        assert_eq!(ids.len(), forms.len());
        assert!(
            ids.windows(2).all(|w| w[0] == w[1]),
            "these must be one person, got {ids:?}"
        );
    }

    #[test]
    fn punctuation_is_dropped_and_diacritics_are_folded() {
        // The distinction that matters: dropping a diacritic instead of folding
        // it eats the letter, turning Şefik into efik.
        assert_eq!(normalize("Erhan!!!").as_deref(), Some("erhan"));
        assert_eq!(normalize("a#$%^&?~|{}[]=b").as_deref(), Some("ab"));
        assert_eq!(normalize("Şşğüöç").as_deref(), Some("ssguoc"));
    }

    #[test]
    fn nothing_usable_is_none_rather_than_empty() {
        // An empty identity would be a person nobody can address.
        for input in ["", "   ", "!!!", "---", "\u{200B}"] {
            assert_eq!(normalize(input), None, "normalize({input:?})");
        }
        assert_eq!(display_name("   "), None);
    }

    #[test]
    fn separators_never_double_or_dangle() {
        assert_eq!(normalize("a  b").as_deref(), Some("a_b"));
        assert_eq!(normalize("  a b  ").as_deref(), Some("a_b"));
        assert_eq!(normalize("a - b").as_deref(), Some("a_b"));
        assert_eq!(normalize("Erhan ").as_deref(), Some("erhan"));
    }

    #[test]
    fn normalizing_twice_changes_nothing() {
        // Reads normalize too, so this runs on values that are already
        // identities. It has to be a fixed point or a round trip through a URL
        // would drift.
        for input in ["Işıl Özyeğin", "Serdar Başaran", "Anne-Marie", "ERHAN"] {
            let once = normalize(input).unwrap();
            let twice = normalize(&once).unwrap();
            assert_eq!(once, twice, "normalize is not idempotent for {input:?}");
        }
    }

    #[test]
    fn display_name_preserves_what_was_typed() {
        // It tidies whitespace and nothing else: case, diacritics and
        // punctuation are what the reader sees.
        assert_eq!(
            display_name("Işıl Özyeğin").as_deref(),
            Some("Işıl Özyeğin")
        );
        assert_eq!(display_name("Anne-Marie").as_deref(), Some("Anne-Marie"));
        assert_eq!(display_name(&"x".repeat(70)).unwrap().chars().count(), 60);
    }
    fn people_db() -> rusqlite::Connection {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE people (name TEXT PRIMARY KEY, full_name TEXT NOT NULL);
             INSERT INTO people VALUES ('erhan_gundogan','Erhan Gündoğan'),
                                       ('ozgur_demirtas','Özgür'),
                                       ('ozgur_tamer','Özgür');",
        )
        .unwrap();
        c
    }

    #[test]
    fn resolve_matches_the_identity_and_the_display_name() {
        let c = people_db();
        assert_eq!(resolve_identities(&c, "erhan_gundogan").unwrap().len(), 1);
        assert!(resolve_identities(&c, "Erhan Gündoğan")
            .unwrap()
            .contains(&"erhan_gundogan".to_string()));
    }

    #[test]
    fn resolve_is_case_insensitive_for_non_ascii() {
        // SQLite's LOWER() cannot do this, which is why it happens in Rust.
        let c = people_db();
        for typed in ["Özgür", "özgür", "ÖZGÜR"] {
            let ids = resolve_identities(&c, typed).unwrap();
            assert!(ids.contains(&"ozgur_demirtas".to_string()), "{typed}");
            assert!(ids.contains(&"ozgur_tamer".to_string()), "{typed}");
        }
    }

    #[test]
    fn resolve_returns_both_people_sharing_a_display_name() {
        let c = people_db();
        let ids = resolve_identities(&c, "Özgür").unwrap();
        // Both people, plus the normalized query itself - which matches no
        // identity here but costs nothing and is what an unmigrated label
        // would be stored as.
        assert!(ids.contains(&"ozgur_demirtas".to_string()));
        assert!(ids.contains(&"ozgur_tamer".to_string()));
        assert!(ids.contains(&"ozgur".to_string()), "the normalized query");
    }

    #[test]
    fn resolve_without_a_people_table_falls_back_to_the_normalized_name() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        assert_eq!(resolve_identities(&c, "Erhan").unwrap(), vec!["erhan"]);
    }

    #[test]
    fn resolve_never_returns_duplicates() {
        let c = people_db();
        // The normalized query and a display match are the same identity here.
        let ids = resolve_identities(&c, "Erhan Gündoğan").unwrap();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "{ids:?}");
    }
    #[test]
    fn display_name_strips_bidi_and_control_characters() {
        // This lived only in `videre-api`'s sanitizer, so a display name
        // written by the migration kept an override character that the same
        // name typed into the UI would have lost. One idea, two behaviours,
        // differing on exactly the part that matters.
        assert_eq!(display_name("A\u{202E}lice").as_deref(), Some("Alice"));
        // Tab is a control character, so it is filtered out entirely rather
        // than collapsed to a space.
        assert_eq!(display_name("A\u{0007}li\tce").as_deref(), Some("Alice"));
        assert_eq!(display_name("\u{200B}").as_deref(), None);
    }

    #[test]
    fn display_name_keeps_joiners_that_real_scripts_need() {
        // ZWNJ and ZWJ are required for Persian and Indic text and for emoji
        // sequences, so they are not "invisible junk" to strip.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(display_name(family).as_deref(), Some(family));
        assert_eq!(display_name("\u{200C}a").as_deref(), Some("\u{200C}a"));
    }
}
