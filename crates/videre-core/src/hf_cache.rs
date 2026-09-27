//! Where Hugging Face weights land locally.
//!
//! Both SigLIP (`videre embed`) and InsightFace (`videre faces`) resolve their
//! weights through hf-hub into this cache. Note it is **not** `~/.cache/ort/`,
//! which `CLAUDE.md` claimed for months and which has never existed.
//!
//! The opt-in model tests pass this path to model-backed children, so it must
//! resolve exactly as the pinned hf-hub loader does, or a child would download
//! into a different cache than the one the test process shares.

use std::path::PathBuf;

/// Root of the local Hugging Face hub cache, using the pinned loader's order.
pub fn cache_dir() -> PathBuf {
    cache_dir_from(|key| std::env::var(key).ok())
}

/// Resolve the cache without mutating process environment in parallel tests.
pub fn cache_dir_from(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(cache) = get("HF_HUB_CACHE") {
        return PathBuf::from(cache);
    }
    if let Some(cache) = get("HUGGINGFACE_HUB_CACHE") {
        return PathBuf::from(cache);
    }
    if let Some(home) = get("HF_HOME") {
        return PathBuf::from(home).join("hub");
    }
    if let Some(xdg) = get("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("huggingface/hub");
    }
    let home = get("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".cache").join("huggingface").join("hub")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_pairs(pairs: &[(&str, &str)]) -> PathBuf {
        cache_dir_from(|key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn hub_cache_follows_hf_hub_environment_precedence() {
        assert_eq!(
            from_pairs(&[
                ("HF_HUB_CACHE", "/one"),
                ("HUGGINGFACE_HUB_CACHE", "/two"),
                ("HF_HOME", "/three"),
                ("XDG_CACHE_HOME", "/four"),
                ("HOME", "/five"),
            ]),
            PathBuf::from("/one")
        );
        assert_eq!(
            from_pairs(&[
                ("HUGGINGFACE_HUB_CACHE", "/two"),
                ("HF_HOME", "/three"),
                ("XDG_CACHE_HOME", "/four"),
                ("HOME", "/five"),
            ]),
            PathBuf::from("/two")
        );
        assert_eq!(
            from_pairs(&[
                ("HF_HOME", "/three"),
                ("XDG_CACHE_HOME", "/four"),
                ("HOME", "/five"),
            ]),
            PathBuf::from("/three/hub")
        );
        assert_eq!(
            from_pairs(&[("XDG_CACHE_HOME", "/four"), ("HOME", "/five")]),
            PathBuf::from("/four/huggingface/hub")
        );
        assert_eq!(
            from_pairs(&[("HOME", "/five")]),
            PathBuf::from("/five/.cache/huggingface/hub")
        );
        assert_eq!(
            from_pairs(&[]),
            PathBuf::from("/tmp/.cache/huggingface/hub")
        );
    }
}
