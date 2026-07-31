use anyhow::{Context, Result};
use ort::session::Session;
use std::path::Path;
use std::path::PathBuf;

const REPO_OWNER: &str = "WePrompt";
const REPO_NAME: &str = "buffalo_l";

/// Builds an ORT `Session` for a face model. Previously ran with ORT's
/// default all-core intra-op thread pool; now takes an explicit
/// `intra_threads` cap since `run_face_pipeline` (pipeline.rs) runs multiple
/// worker threads concurrently, each with its own session - N sessions x
/// "every core" would oversubscribe the machine far worse than the old
/// single-session baseline. Pass a small number (e.g. 2) per worker so N
/// workers x intra_threads stays near the machine's actual core count. The
/// macOS CoreML execution provider was measured (2026-07-23) to give no
/// speedup for these InsightFace models (the SCRFD/ArcFace graphs don't
/// accelerate on CoreML, and it adds a multi-second per-process
/// model-compile cost), so it is intentionally not used. The dominant cost
/// of `videre faces` is SCRFD detection plus per-image loading (HEIC via
/// qlmanage) and, per the pipeline being fully serial until 2026-07-29, a
/// lack of concurrency - see
/// docs/superpowers/specs/2026-07-29-faces-pipeline-parallelization-design.md.
pub fn build_session(model_path: &Path, intra_threads: usize) -> Result<Session> {
    Session::builder()
        .context("create ort SessionBuilder")?
        .with_intra_threads(intra_threads)
        .map_err(|e| anyhow::anyhow!("set intra-op thread count: {e}"))?
        .commit_from_file(model_path)
        .context("load ONNX model")
}

/// Download (or return cached) SCRFD detector and ArcFace recognizer weights.
/// Uses hf-hub blocking API; downloads ~200 MB on first run into ~/.cache/huggingface/.
pub fn buffalo_l_paths() -> Result<(PathBuf, PathBuf)> {
    let client = hf_hub::HFClientSync::new().context("init HF Hub client")?;
    let repo = client.model(REPO_OWNER, REPO_NAME);
    let det = repo
        .download_file()
        .filename("det_10g.onnx")
        .send()
        .context("download det_10g.onnx")?;
    let rec = repo
        .download_file()
        .filename("w600k_r50.onnx")
        .send()
        .context("download w600k_r50.onnx")?;
    Ok((det, rec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_session_errors_on_nonexistent_model_path() {
        // Fails at commit_from_file (file doesn't exist) - no network or real
        // model weights needed, so this is a fast, deterministic error-path test.
        let result = build_session(Path::new("/nonexistent/model.onnx"), 1);
        assert!(result.is_err());
    }
}
