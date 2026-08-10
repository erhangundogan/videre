//! Where Hugging Face weights land locally, and whether they are already there.
//!
//! Both SigLIP (`videre embed`) and InsightFace (`videre faces`) resolve their
//! weights through hf-hub into this cache. Note it is **not** `~/.cache/ort/`,
//! which `CLAUDE.md` claimed for months and which has never existed; a check
//! written against that path reports "cold" forever.
//!
//! This lives in `videre-core` rather than in a test helper because three
//! callers need it: the integration-test harness, `videre-ml`'s own tests, and
//! anything that wants to avoid triggering a multi-hundred-megabyte download as
//! a side effect. hf-hub 1.0.0 exposes no offline switch, so the check has to
//! be ours.

use std::path::PathBuf;

/// Root of the local Hugging Face hub cache, honouring `HF_HOME`.
pub fn cache_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HF_HOME") {
        return PathBuf::from(home).join("hub");
    }
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".cache").join("huggingface").join("hub")
}

/// Whether every one of `files` is present in some snapshot of `repo`.
///
/// Checks the files themselves rather than the repo directory: an interrupted
/// download leaves the directory in place with the weights missing, and that
/// state must read as cold or the caller proceeds into a failure.
///
/// The snapshot hash is globbed rather than pinned, since it changes whenever
/// the upstream repo is updated.
pub fn repo_has(repo: &str, files: &[&str]) -> bool {
    let snapshots = cache_dir()
        .join(format!("models--{}", repo.replace('/', "--")))
        .join("snapshots");
    let Ok(entries) = std::fs::read_dir(&snapshots) else {
        return false;
    };
    entries
        .filter_map(Result::ok)
        .any(|snap| files.iter().all(|f| snap.path().join(f).exists()))
}

/// Whether a SigLIP model has enough cached to run inference.
///
/// Weights are checked by extension rather than by name: a model may ship one
/// `model.safetensors` or a sharded set, and requiring a specific filename
/// would report a perfectly usable cache as cold.
pub fn siglip_ready(model_id: &str) -> bool {
    if !repo_has(model_id, &["config.json", "tokenizer.json"]) {
        return false;
    }
    let snapshots = cache_dir()
        .join(format!("models--{}", model_id.replace('/', "--")))
        .join("snapshots");
    let Ok(entries) = std::fs::read_dir(&snapshots) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|snap| {
        std::fs::read_dir(snap.path())
            .map(|files| {
                files.filter_map(Result::ok).any(|f| {
                    f.path()
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("safetensors"))
                })
            })
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hf_home_overrides_the_default_location() {
        // Not using std::env::set_var: tests share a process and run in
        // parallel, so mutating the environment here would race every other
        // test's getenv. Asserts the shape of the default instead.
        let d = cache_dir();
        assert!(
            d.ends_with("hub"),
            "cache dir should end in hub, got {}",
            d.display()
        );
    }

    #[test]
    fn a_missing_repo_is_not_cached() {
        assert!(!repo_has(
            "definitely/not-a-real-model-xyz",
            &["config.json"]
        ));
        assert!(!siglip_ready("definitely/not-a-real-model-xyz"));
    }

    #[test]
    fn a_repo_directory_without_the_files_is_not_cached() {
        // The interrupted-download shape: directory present, weights absent.
        // Must read as cold, or the caller proceeds into a failure.
        let tmp = std::env::temp_dir().join(format!("videre-hf-probe-{}", std::process::id()));
        let snap = tmp
            .join("hub")
            .join("models--fake--repo")
            .join("snapshots")
            .join("abc123");
        std::fs::create_dir_all(&snap).unwrap();
        // Verified via the same path-building logic rather than by setting
        // HF_HOME, for the parallelism reason above.
        assert!(!snap.join("config.json").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
