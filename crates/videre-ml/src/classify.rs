//! Zero-shot classification of already-computed image embeddings against a
//! fixed set of category prompts, reusing the SigLIP text tower `videre
//! embed`/`videre search` already use, with no new model and no re-embedding.
//! See docs/superpowers/specs/2026-07-29-screenshot-document-classification-design.md.

/// Category name used when no prompt's similarity clearly wins.
pub const UNKNOWN_CATEGORY: &str = "unknown";

/// (category name, zero-shot prompt caption). Not exposed as a CLI flag;
/// tune here if real-world results look off. SigLIP embeds full descriptive
/// captions better than bare single-word labels.
pub const CATEGORY_PROMPTS: &[(&str, &str)] = &[
    ("photo", "a photo of a person, place, or thing"),
    ("screenshot", "a screenshot of a phone or computer screen"),
    ("document", "a photo of a document, receipt, or piece of paper"),
    ("meme", "a meme image with text captions overlaid on a picture"),
];

/// Picks the winning category from per-prompt similarity scores, or
/// `UNKNOWN_CATEGORY` if the top two scores are not clearly separated (the
/// gap must be strictly greater than `margin` to accept the top pick, a
/// gap exactly equal to `margin` falls back to unknown).
///
/// Category names are `&'static str` (always `CATEGORY_PROMPTS` entries or
/// `UNKNOWN_CATEGORY`), not tied to `scores`'s borrow, so callers can hold
/// the returned category across loop iterations without lifetime issues.
///
/// Panics if `scores` is empty, callers always pass one score per
/// `CATEGORY_PROMPTS` entry, which is never empty.
pub fn classify_from_scores(scores: &[(&'static str, f32)], margin: f32) -> (&'static str, f32) {
    assert!(!scores.is_empty(), "classify_from_scores requires at least one score");
    let mut sorted: Vec<(&'static str, f32)> = scores.to_vec();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (top_category, top_score) = sorted[0];
    if sorted.len() == 1 {
        return (top_category, top_score);
    }
    let (_, second_score) = sorted[1];
    if top_score - second_score > margin {
        (top_category, top_score)
    } else {
        (UNKNOWN_CATEGORY, top_score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_winner_returns_that_category_with_its_score() {
        let scores = [("photo", 0.9), ("screenshot", 0.3), ("document", 0.2), ("meme", 0.1)];
        let (cat, score) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, "photo");
        assert_eq!(score, 0.9);
    }

    #[test]
    fn top_two_within_margin_falls_back_to_unknown() {
        let scores = [("photo", 0.52), ("screenshot", 0.50), ("document", 0.1), ("meme", 0.05)];
        let (cat, score) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, UNKNOWN_CATEGORY);
        assert_eq!(score, 0.52);
    }

    #[test]
    fn gap_exactly_equal_to_margin_falls_back_to_unknown() {
        // Use values with exact binary representations to avoid fp precision issues:
        // 0.625 = 0.101 (binary), 0.5 = 0.1 (binary), 0.125 = 0.001 (binary)
        let scores = [("photo", 0.625), ("screenshot", 0.5)];
        let (cat, score) = classify_from_scores(&scores, 0.125);
        assert_eq!(cat, UNKNOWN_CATEGORY);
        assert_eq!(score, 0.625);
    }

    #[test]
    fn gap_just_over_margin_accepts_top_pick() {
        let scores = [("photo", 0.551), ("screenshot", 0.50)];
        let (cat, _) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, "photo");
    }

    #[test]
    fn single_entry_returns_that_entry_without_panicking() {
        let scores = [("photo", 0.42)];
        let (cat, score) = classify_from_scores(&scores, 0.05);
        assert_eq!(cat, "photo");
        assert_eq!(score, 0.42);
    }

    #[test]
    #[should_panic]
    fn empty_scores_panics() {
        let scores: [(&'static str, f32); 0] = [];
        classify_from_scores(&scores, 0.05);
    }

    #[test]
    fn category_prompts_has_four_entries() {
        assert_eq!(CATEGORY_PROMPTS.len(), 4);
    }
}
