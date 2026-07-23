/// Trim, collapse internal whitespace, and cap length (60 code points) so a
/// caller that bypasses UI sanitization can't stretch layout or bloat the DB.
/// Returns None when nothing usable remains. Filters control and bidi/
/// zero-width format characters but deliberately keeps U+200C (ZWNJ) and
/// U+200D (ZWJ), which are required for Persian/Indic text and emoji ZWJ
/// sequences. Not homoglyph-proof, and the cap truncates by code point.
pub fn sanitize_person_label(raw: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::sanitize_person_label;

    #[test]
    fn trims_collapses_and_caps() {
        assert_eq!(sanitize_person_label("  Alice   B  ").as_deref(), Some("Alice B"));
        assert_eq!(sanitize_person_label("   ").as_deref(), None);
        assert_eq!(sanitize_person_label(&"x".repeat(70)).unwrap().chars().count(), 60);
    }

    #[test]
    fn strips_bidi_override() {
        assert_eq!(sanitize_person_label("A\u{202E}lice").as_deref(), Some("Alice"));
    }

    #[test]
    fn keeps_zwj_emoji_sequences() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(sanitize_person_label(family).as_deref(), Some(family));
    }
}
