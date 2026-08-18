/// The display form of a person's name.
///
/// Thin alias for `videre_core::person::display_name`, which is the single
/// implementation. This existed first and diverged from it: the core copy
/// did not filter bidi characters, so the same name kept an override when
/// written by the migration and lost it when typed into the UI.
pub fn sanitize_person_label(raw: &str) -> Option<String> {
    videre_core::person::display_name(raw)
}

#[cfg(test)]
mod tests {
    use super::sanitize_person_label;

    #[test]
    fn trims_collapses_and_caps() {
        assert_eq!(
            sanitize_person_label("  Alice   B  ").as_deref(),
            Some("Alice B")
        );
        assert_eq!(sanitize_person_label("   ").as_deref(), None);
        assert_eq!(
            sanitize_person_label(&"x".repeat(70))
                .unwrap()
                .chars()
                .count(),
            60
        );
    }

    #[test]
    fn strips_bidi_override() {
        assert_eq!(
            sanitize_person_label("A\u{202E}lice").as_deref(),
            Some("Alice")
        );
    }

    #[test]
    fn keeps_zwj_emoji_sequences() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(sanitize_person_label(family).as_deref(), Some(family));
    }

    #[test]
    fn strips_control_chars() {
        assert_eq!(
            sanitize_person_label("A\u{0007}li\tce").as_deref(),
            Some("Alice")
        );
    }
}
