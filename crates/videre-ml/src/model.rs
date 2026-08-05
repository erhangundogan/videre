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

pub const MODEL_ID: &str = videre_core::embeddings::DEFAULT_MODEL_ID;
pub const IMAGE_SIZE: usize = 384;

/// Model actually used, overridable with `VIDERE_EMBED_MODEL=<hf model id>`.
///
/// EXPERIMENTAL benchmarking scaffold (2026-08-04) for comparing SigLIP
/// variants. Embeddings are tagged with this id in the `embeddings` table, so
/// switching models makes `pending_images` treat the whole library as
/// unembedded, i.e. changing this means a full re-embed, by design.
pub fn configured_model_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| std::env::var("VIDERE_EMBED_MODEL").unwrap_or_else(|_| MODEL_ID.to_string()))
}

/// Input resolution for `configured_model_id()`.
///
/// Taken from the trailing `-<size>` in the HF model id (`...-patch14-224` ->
/// 224) rather than `config.json`, because several published SigLIP configs
/// omit `vision_config.image_size` and rely on library defaults. Falls back to
/// `IMAGE_SIZE` when the id has no recognizable suffix.
pub fn configured_image_size() -> usize {
    configured_model_id()
        .rsplit('-')
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n >= 64 && *n <= 1024)
        .unwrap_or(IMAGE_SIZE)
}

/// Largest number of images `embed_images` may be given in one call.
///
/// Above a threshold measured between 121 and 127 (120 verified clean, 127
/// verified corrupt), this path silently returns embeddings that do not match
/// a one-image-at-a-time baseline, no error, no NaN, just wrong vectors.
/// Every *full* batch at or above the threshold is affected; a trailing
/// partial batch is always correct, which is what makes the failure so easy to
/// miss. Measured 2026-08-04 on macOS/Metal with `siglip-so400m-patch14-384`;
/// full reproduction in
/// `docs/superpowers/2026-08-04-embed-batch-corruption-investigation.md`.
///
/// This lives here rather than in the CLI because it is a property of this
/// inference path, not of any one caller. Anyone tempted to raise it: checking
/// output for zero/NaN vectors is NOT sufficient, `--batch 256` yields zero
/// all-zero vectors and is still fully corrupt. Only a cosine comparison
/// against a small-batch baseline detects it (see the ignored
/// `batched_embeddings_match_one_at_a_time` test below). 96 keeps deliberate
/// headroom below the observed boundary, since the exact threshold may shift
/// with available unified memory.
pub const MAX_SAFE_BATCH: usize = 96;

/// Maximum token sequence length for text queries.
const MAX_TEXT_LEN: usize = 64;
/// Pad token id for SigLIP (`</s>`, id 1).
const PAD_TOKEN_ID: u32 = 1;

pub struct Embedder {
    model: siglip::Model,
    tokenizer: Tokenizer,
    device: Device,
    dtype: DType,
}

/// Inference precision, overridable with `VIDERE_EMBED_DTYPE=f16|f32`.
///
/// Opt-in, default `F32`, the precision every existing embedding was computed
/// at. `f16` measured 2026-08-04 on macOS/Metal: ~11% faster on pure jpg/png,
/// ~7% on a realistic mix once HEIC/video decode dilutes it, with no memory
/// saving (6.60GB peak either way, the F32 safetensors are still mmap'd and
/// converted). Output quality is effectively unchanged: over 190 images the
/// worst f16-vs-f32 cosine similarity was 0.999794, median 0.99999, which is
/// within the noise of the f16 *storage* quantization applied to every
/// embedding anyway (`vectors::to_f16_bytes`).
///
/// Left opt-in rather than made default because 7% did not justify perturbing
/// a library whose embeddings were all computed at F32, not because mixing is
/// unsafe, which the cosine numbers above rule out.
fn configured_dtype() -> DType {
    match std::env::var("VIDERE_EMBED_DTYPE").as_deref() {
        Ok("f16") => DType::F16,
        Ok("f32") | Err(_) => DType::F32,
        Ok(other) => {
            eprintln!("warning: unknown VIDERE_EMBED_DTYPE={other:?}; using f32");
            DType::F32
        }
    }
}

impl Embedder {
    /// Download (or use cached) SigLIP weights and build the embedder.
    pub fn load(device: Device) -> Result<Self> {
        let client = hf_hub::HFClientSync::new().context("init HF Hub client")?;
        let model_id = configured_model_id();
        if model_id != MODEL_ID {
            eprintln!("Using model {model_id} at {}px (VIDERE_EMBED_MODEL).", configured_image_size());
        }
        let (owner, name) = model_id.split_once('/').expect("model id is owner/name");
        let repo = client.model(owner, name);

        eprintln!("Loading model {model_id} (downloads to hf-hub cache on first run)...");

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

        // Weights, try single file first, then sharded index
        let weight_paths = load_safetensor_paths(&repo)?;
        // Paging a multi-GB mmap in from disk can take minutes when the file
        // is cold and the process runs at background QoS (nohup/launchd/cron),
        // where macOS throttles disk I/O hard, print the size so a slow load
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
        let dtype = configured_dtype();
        if dtype != DType::F32 {
            eprintln!("Using {dtype:?} inference precision (VIDERE_EMBED_DTYPE).");
        }
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&weight_paths, dtype, &device)
                .context("mmap safetensors")?
        };

        let model = siglip::Model::new(&config, vb).context("build siglip model")?;
        Ok(Self { model, tokenizer, device, dtype })
    }

    /// Embed a batch of `[3, IMAGE_SIZE, IMAGE_SIZE]` image tensors.
    /// Returns one L2-normalized vector per image.
    pub fn embed_images(&self, images: &[Tensor]) -> Result<Vec<Vec<f32>>> {
        if images.is_empty() {
            return Ok(vec![]);
        }
        // Stack into [B, 3, H, W]
        let batch = Tensor::stack(images, 0)
            .context("stack image batch")?
            .to_dtype(self.dtype)
            .context("cast image batch to model dtype")?;
        let features = self
            .model
            .get_image_features(&batch)
            .context("image forward pass")?;
        // features: [B, embed_dim]
        let b = features.dim(0)?;
        let mut out = Vec::with_capacity(b);
        for i in 0..b {
            let row: Vec<f32> = features.get(i)?.to_dtype(DType::F32)?.to_vec1()?;
            let mut row = row;
            videre_core::vectors::l2_normalize(&mut row);
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
        let mut vec: Vec<f32> = features.get(0)?.to_dtype(DType::F32)?.to_vec1()?;
        videre_core::vectors::l2_normalize(&mut vec);
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

/// Parse `model.safetensors.index.json`'s `weight_map` into a deduped,
/// deterministically-sorted list of shard filenames. Pure JSON parsing, split
/// out from `load_sharded_safetensors` specifically so it's unit-testable
/// without a real HF Hub repo.
fn shard_names_from_index(index_str: &str) -> Result<Vec<String>> {
    // The index JSON has shape: { "weight_map": { "tensor_name": "shard_file", ... } }
    let index: serde_json::Value =
        serde_json::from_str(index_str).context("parse safetensors index")?;
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
    Ok(shards)
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
    let shards = shard_names_from_index(&index_str)?;

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

#[cfg(test)]
mod index_parsing_tests {
    use super::shard_names_from_index;

    #[test]
    fn shard_names_from_index_dedupes_and_sorts() {
        let index = r#"{
            "weight_map": {
                "layer1.weight": "model-00002-of-00002.safetensors",
                "layer2.weight": "model-00001-of-00002.safetensors",
                "layer3.weight": "model-00002-of-00002.safetensors"
            }
        }"#;
        let shards = shard_names_from_index(index).unwrap();
        assert_eq!(
            shards,
            vec!["model-00001-of-00002.safetensors", "model-00002-of-00002.safetensors"]
        );
    }

    #[test]
    fn shard_names_from_index_errors_without_weight_map() {
        let result = shard_names_from_index(r#"{"not_weight_map": {}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn shard_names_from_index_errors_on_malformed_json() {
        let result = shard_names_from_index("not json");
        assert!(result.is_err());
    }
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

#[cfg(test)]
mod batch_correctness_tests {
    use super::*;

    /// Empirical proof that `MAX_SAFE_BATCH` is actually safe: embedding N
    /// images in one call must match embedding them one at a time.
    ///
    /// `#[ignore]`d because it loads the real 3.5GB model and runs
    /// `MAX_SAFE_BATCH` forward passes (tens of seconds), which is too slow for
    /// every `cargo test`. The cheap automated guard is
    /// `safe_batch_maximum_stays_below_the_measured_corruption_threshold` in
    /// the CLI's embed module; this is the expensive proof to run **whenever
    /// `MAX_SAFE_BATCH` is changed**, on the machine it is being changed for:
    ///
    /// ```text
    /// cargo test -p videre-ml --release batched_embeddings_match_one_at_a_time -- --ignored --nocapture
    /// ```
    ///
    /// Raise `MAX_SAFE_BATCH` past the real threshold and this fails with
    /// cosine similarities far below 1.0, which is exactly the silent
    /// corruption it exists to catch. Note the failure mode is NOT zeros or
    /// NaNs (batch 256 produces neither and is still wrong), so comparing
    /// against the one-at-a-time baseline is the only reliable check.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn batched_embeddings_match_one_at_a_time() {
        let embedder = Embedder::load(crate::device::best_device()).unwrap();

        // Distinct inputs, so a bug that returns one vector repeated (or a
        // shifted/garbage buffer) cannot pass by accident.
        let n = MAX_SAFE_BATCH;
        let images: Vec<Tensor> = (0..n)
            .map(|i| {
                let v = (i as f32 / n as f32) * 2.0 - 1.0;
                Tensor::full(v, (3, IMAGE_SIZE, IMAGE_SIZE), &embedder.device).unwrap()
            })
            .collect();

        let batched = embedder.embed_images(&images).unwrap();
        assert_eq!(batched.len(), n, "one embedding per input expected");

        let mut worst = f32::MAX;
        let mut worst_at = 0usize;
        for (i, img) in images.iter().enumerate() {
            let single = embedder.embed_images(std::slice::from_ref(img)).unwrap();
            let (a, b) = (&batched[i], &single[0]);
            let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
            let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!(na > 0.0 && nb > 0.0, "zero-norm embedding at index {i}");
            let cos = dot / (na * nb);
            if cos < worst {
                worst = cos;
                worst_at = i;
            }
        }

        println!("worst cosine over {n} images: {worst:.6} (index {worst_at})");
        assert!(
            worst > 0.99,
            "batch of {n} disagrees with one-at-a-time embedding (worst cosine {worst:.6} at \
             index {worst_at}); MAX_SAFE_BATCH is too high for this machine"
        );
    }
}
