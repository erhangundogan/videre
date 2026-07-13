use anyhow::{Context, Result};
use std::path::PathBuf;

const REPO_OWNER: &str = "WePrompt";
const REPO_NAME: &str = "buffalo_l";

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
