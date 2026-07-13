//! SigLIP model wrapper: load google/siglip-so400m-patch14-384, embed images and text.
//!
//! Weights are downloaded from HuggingFace Hub on first use and cached locally.
//! The real-model integration test is gated behind `--features real-model`.

use anyhow::{Context, Result};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::siglip;
use std::path::{Path, PathBuf};
use tokenizers::Tokenizer;

pub const MODEL_ID: &str = dupe_core::embeddings::DEFAULT_MODEL_ID;
pub const IMAGE_SIZE: usize = 384;

/// Maximum token sequence length for text queries.
const MAX_TEXT_LEN: usize = 64;
/// Pad token id for SigLIP (`</s>`, id 1).
const PAD_TOKEN_ID: u32 = 1;

pub struct Embedder {
    model: siglip::Model,
    tokenizer: Tokenizer,
    device: Device,
}

impl Embedder {
    /// Download (or use cached) SigLIP weights and build the embedder.
    pub fn load(device: Device) -> Result<Self> {
        let client = hf_hub::HFClientSync::new().context("init HF Hub client")?;
        let (owner, name) = MODEL_ID.split_once('/').expect("model id is owner/name");
        let repo = client.model(owner, name);

        eprintln!("Loading model {MODEL_ID} (downloads to hf-hub cache on first run)...");

        // Config
        let config_path = repo
            .download_file()
            .filename("config.json")
            .send()
            .context("fetch config.json")?;
        let config_str =
            std::fs::read_to_string(&config_path).context("read config.json")?;
        let config: siglip::Config =
            serde_json::from_str(&config_str).context("parse siglip config.json")?;

        // Tokenizer
        let tokenizer_path = repo
            .download_file()
            .filename("tokenizer.json")
            .send()
            .context("fetch tokenizer.json")?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("load tokenizer: {e}"))?;

        // Weights - try single file first, then sharded index
        let weight_paths = load_safetensor_paths(&repo)?;
        // Paging a multi-GB mmap in from disk can take minutes when the file
        // is cold and the process runs at background QoS (nohup/launchd/cron),
        // where macOS throttles disk I/O hard - print the size so a slow load
        // is distinguishable from a hang.
        let total_bytes: u64 = weight_paths
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        eprintln!(
            "Loading weights ({:.1} GB; cold first read can take minutes, longer at background priority)...",
            total_bytes as f64 / 1e9
        );
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&weight_paths, DType::F32, &device)
                .context("mmap safetensors")?
        };

        let model = siglip::Model::new(&config, vb).context("build siglip model")?;
        Ok(Self { model, tokenizer, device })
    }

    /// Embed a batch of `[3, IMAGE_SIZE, IMAGE_SIZE]` image tensors.
    /// Returns one L2-normalized vector per image.
    pub fn embed_images(&self, images: &[Tensor]) -> Result<Vec<Vec<f32>>> {
        if images.is_empty() {
            return Ok(vec![]);
        }
        // Stack into [B, 3, H, W]
        let batch = Tensor::stack(images, 0).context("stack image batch")?;
        let features = self
            .model
            .get_image_features(&batch)
            .context("image forward pass")?;
        // features: [B, embed_dim]
        let b = features.dim(0)?;
        let mut out = Vec::with_capacity(b);
        for i in 0..b {
            let row: Vec<f32> = features.get(i)?.to_vec1()?;
            let mut row = row;
            dupe_core::vectors::l2_normalize(&mut row);
            out.push(row);
        }
        Ok(out)
    }

    /// Tokenize `text`, pad/truncate to `MAX_TEXT_LEN`, run the text tower.
    /// Returns an L2-normalized embedding vector.
    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let lower = text.to_lowercase();
        let encoding = self
            .tokenizer
            .encode(lower.as_str(), true)
            .map_err(|e| anyhow::anyhow!("tokenize: {e}"))?;

        let mut ids: Vec<u32> = encoding.get_ids().to_vec();
        ids.truncate(MAX_TEXT_LEN);
        while ids.len() < MAX_TEXT_LEN {
            ids.push(PAD_TOKEN_ID);
        }

        let input_ids =
            Tensor::from_vec(ids, (1, MAX_TEXT_LEN), &self.device).context("build input_ids")?;
        let features = self
            .model
            .get_text_features(&input_ids)
            .context("text forward pass")?;
        // features: [1, embed_dim]
        let mut vec: Vec<f32> = features.get(0)?.to_vec1()?;
        dupe_core::vectors::l2_normalize(&mut vec);
        Ok(vec)
    }
}

/// Preprocess an image file and embed it with `embedder`.
pub fn embed_image_file(embedder: &Embedder, path: &Path) -> Result<Vec<f32>> {
    let t = crate::preprocess::image_to_tensor(path, IMAGE_SIZE, &embedder.device)
        .with_context(|| format!("preprocess {}", path.display()))?;
    embedder
        .embed_images(&[t])
        .context("embed image")?
        .into_iter()
        .next()
        .context("embed returned empty result")
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

type Repo = hf_hub::HFRepositorySync<hf_hub::RepoTypeModel>;

/// Return the list of safetensor paths for the model.
/// Tries the single-file layout first; falls back to a sharded index.
fn load_safetensor_paths(repo: &Repo) -> Result<Vec<PathBuf>> {
    match repo.download_file().filename("model.safetensors").send() {
        Ok(p) => Ok(vec![p]),
        Err(_) => load_sharded_safetensors(repo),
    }
}

/// Parse `model.safetensors.index.json` and download each unique shard.
fn load_sharded_safetensors(repo: &Repo) -> Result<Vec<PathBuf>> {
    let index_path = repo
        .download_file()
        .filename("model.safetensors.index.json")
        .send()
        .context("fetch model.safetensors.index.json")?;
    let index_str =
        std::fs::read_to_string(&index_path).context("read safetensors index")?;

    // The index JSON has shape: { "weight_map": { "tensor_name": "shard_file", ... } }
    let index: serde_json::Value =
        serde_json::from_str(&index_str).context("parse safetensors index")?;
    let weight_map = index
        .get("weight_map")
        .and_then(|v| v.as_object())
        .context("safetensors index missing weight_map")?;

    // Collect unique shard filenames (preserve insertion order via a vec+set).
    let mut seen = std::collections::HashSet::new();
    let mut shards: Vec<String> = Vec::new();
    for shard_file in weight_map.values() {
        let name = shard_file
            .as_str()
            .context("shard filename is not a string")?
            .to_string();
        if seen.insert(name.clone()) {
            shards.push(name);
        }
    }
    shards.sort(); // deterministic order

    let mut paths = Vec::with_capacity(shards.len());
    for shard in &shards {
        let p = repo
            .download_file()
            .filename(shard.clone())
            .send()
            .with_context(|| format!("fetch shard {shard}"))?;
        paths.push(p);
    }
    Ok(paths)
}

// ---------------------------------------------------------------------------
// Integration tests (real weights required)
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "real-model"))]
mod tests {
    use super::*;

    #[test]
    fn text_and_image_towers_agree_on_semantics() {
        let e = Embedder::load(crate::device::best_device()).unwrap();
        let red =
            embed_image_file(&e, std::path::Path::new("tests/fixtures/red_2x2.png")).unwrap();
        let q_red = e.embed_text("a solid red square").unwrap();
        let q_dog = e.embed_text("a photo of a dog").unwrap();
        let dot =
            |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        assert!(dot(&red, &q_red) > dot(&red, &q_dog));
        assert_eq!(red.len(), q_red.len());
    }
}
