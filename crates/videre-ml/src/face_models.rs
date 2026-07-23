use anyhow::{Context, Result};
use ort::session::Session;
use std::path::Path;
use std::path::PathBuf;

const REPO_OWNER: &str = "WePrompt";
const REPO_NAME: &str = "buffalo_l";

/// Builds an ORT `Session` for a face model. ORT runs on CPU with all cores by
/// default (its intra-op thread pool), which is where face detection/embedding
/// runs. The macOS CoreML execution provider was measured (2026-07-23) to give
/// no speedup for these InsightFace models - the SCRFD/ArcFace graphs don't
/// accelerate on CoreML, and it adds a multi-second per-process model-compile
/// cost - so it is intentionally not used. The dominant cost of `videre faces`
/// is SCRFD detection plus per-image loading (HEIC via qlmanage), not something
/// an ONNX execution provider changes.
pub fn build_session(model_path: &Path) -> Result<Session> {
    Session::builder()
        .context("create ort SessionBuilder")?
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
