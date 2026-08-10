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

/// Input resolution for `model_id`.
///
/// Taken from the trailing `-<size>` in the HF model id (`...-patch14-224` ->
/// 224) rather than `config.json`, because several published SigLIP configs
/// omit `vision_config.image_size` and rely on library defaults. Falls back to
/// `IMAGE_SIZE` when the id has no recognizable suffix.
///
/// Takes the id as an argument rather than reading a process-global: the
/// caller resolves it once via `videre_core::embeddings::resolve_model_id`,
/// so the weights loaded and the database written can never disagree about
/// which model is in play.
pub fn image_size_for(model_id: &str) -> usize {
    model_id
        .rsplit('-')
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n >= 64 && *n <= 1024)
        .unwrap_or(IMAGE_SIZE)
}

/// Largest number of images `embed_images` may be given in one call.
///
/// Above a per-model threshold, candle's **Metal** backend silently returns
/// embeddings that do not match a one-image-at-a-time baseline: no error, no
/// NaN, just wrong vectors. Every *full* batch at or above the threshold is
/// affected while the trailing partial batch is correct, which is what makes
/// the failure so easy to miss.
///
/// Measured on Metal 2026-08-09 (full tables in
/// `docs/superpowers/2026-08-04-embed-batch-corruption-investigation.md`):
///
/// | model | last clean | first corrupt |
/// |---|---|---|
/// | `siglip-so400m-patch14-384` | 126 | 127 |
/// | `siglip-base-patch16-224` | 891 | 892 |
///
/// **CPU is unaffected**: the same batch that gives 0.670 cosine on Metal is
/// bit-identical (1.000000) on `Device::Cpu`, so this is a candle Metal-backend
/// defect rather than anything in videre or the models. Linux is therefore not
/// exposed.
///
/// 96 is below both measured boundaries. It stays a single constant rather
/// than becoming `max_safe_batch(model_id)` for two measured reasons:
///
/// 1. **No formula predicts the boundary.** Three were tested. Threshold
///    proportional to patches x hidden dims predicted base at 669 (clean at
///    768). The attention tensor crossing 2^32 bytes predicted so400m's 126/127
///    exactly but base at 2329 (already corrupt). Largest of attention or MLP
///    predicted 1783 (already corrupt). One exact fit against two misses is not
///    something to extrapolate to an unmeasured model.
/// 2. **A larger batch buys nothing.** Per-image time is flat and then worsens:
///    base-224 runs 31.0ms/image at 96 and 39.1ms at 768, so400m 510ms at 96
///    and 500ms at 120. The original "4x speedup" that prompted all this was
///    the corrupt path skipping work, not a real gain.
///
/// So there is no upside to raising it, and `verify_large_batch` below guards
/// the case this constant cannot: a model whose boundary sits *below* 96.
///
/// Anyone tempted to raise it anyway: checking output for zero/NaN vectors is
/// NOT sufficient. `--batch 256` yields no all-zero vectors and is still fully
/// corrupt. Only a cosine comparison against a smaller-batch baseline detects
/// it; see `batched_embeddings_match_one_at_a_time`.
pub const MAX_SAFE_BATCH: usize = 96;

/// Fraction of `MAX_SAFE_BATCH` at or above which a batch is self-checked.
///
/// The check costs one extra forward pass, so it is worth ~1/N of a batch of
/// N. Applying it only to large batches keeps that cost off the small-batch
/// path, where the defect has never been observed on any model.
const VERIFY_ABOVE: usize = MAX_SAFE_BATCH / 2;

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
    ///
    /// `model_id` is resolved by the caller via
    /// `videre_core::embeddings::resolve_model_id`, so the weights loaded here
    /// always match the database the caller will read or write.
    pub fn load(device: Device, model_id: &str) -> Result<Self> {
        let client = hf_hub::HFClientSync::new().context("init HF Hub client")?;
        if model_id != MODEL_ID {
            eprintln!("Using model {model_id} at {}px.", image_size_for(model_id));
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
        let config_str = std::fs::read_to_string(&config_path).context("read config.json")?;
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
        Ok(Self {
            model,
            tokenizer,
            device,
            dtype,
        })
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
        self.verify_large_batch(images, &out)?;
        Ok(out)
    }

    /// Check a large batch against a single-image re-run, and refuse the whole
    /// batch if they disagree.
    ///
    /// `MAX_SAFE_BATCH` is below the boundary of every model measured so far,
    /// but no formula predicts that boundary (see its doc comment), so nothing
    /// guarantees it for a model nobody has run. This is the guard for that
    /// case, and it needs no formula: it re-embeds one image on its own and
    /// compares.
    ///
    /// Returning an error rather than warning is deliberate. The caller must
    /// write nothing, because a corrupt embedding is worse than a missing one:
    /// `videre search` and `videre classify` consume it silently forever after,
    /// and the failure leaves no trace to find it by. A missing embedding is
    /// simply recomputed on the next run.
    ///
    /// Costs one extra forward pass per checked batch, so ~1% at batch 96.
    /// Only batches at or above `VERIFY_ABOVE` are checked; a single-image
    /// re-run is far below that, so this cannot recurse.
    fn verify_large_batch(&self, images: &[Tensor], out: &[Vec<f32>]) -> Result<()> {
        if images.len() < VERIFY_ABOVE {
            return Ok(());
        }
        // Index 0: on every corrupt batch observed, the whole full batch is
        // affected rather than a scattered subset, so any index detects it.
        let single = self.embed_images_unchecked(std::slice::from_ref(&images[0]))?;
        let cos = videre_core::vectors::cosine(&out[0], &single[0]);
        if cos > 0.99 {
            return Ok(());
        }
        anyhow::bail!(
            "batch of {} produced embeddings that disagree with a single-image re-run \
             (cosine {cos:.6}); refusing to write. This is a known candle Metal-backend \
             defect above a per-model batch size. Re-run with a smaller --batch (the \
             default is safe on every model measured so far).",
            images.len()
        )
    }

    /// `embed_images` without the self-check, so the check itself cannot
    /// recurse into it.
    fn embed_images_unchecked(&self, images: &[Tensor]) -> Result<Vec<Vec<f32>>> {
        let batch = Tensor::stack(images, 0)
            .context("stack image batch")?
            .to_dtype(self.dtype)
            .context("cast image batch to model dtype")?;
        let features = self
            .model
            .get_image_features(&batch)
            .context("image forward pass")?;
        let b = features.dim(0)?;
        let mut out = Vec::with_capacity(b);
        for i in 0..b {
            let mut row: Vec<f32> = features.get(i)?.to_dtype(DType::F32)?.to_vec1()?;
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
    let index_str = std::fs::read_to_string(&index_path).context("read safetensors index")?;
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
            vec![
                "model-00001-of-00002.safetensors",
                "model-00002-of-00002.safetensors"
            ]
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
    #[test]
    fn image_size_for_reads_the_trailing_suffix() {
        assert_eq!(image_size_for("google/siglip2-base-patch16-384"), 384);
        assert_eq!(image_size_for("google/siglip-base-patch16-224"), 224);
        assert_eq!(image_size_for("google/siglip-so400m-patch14-384"), 384);
    }

    #[test]
    fn image_size_for_falls_back_when_there_is_no_recognizable_suffix() {
        assert_eq!(image_size_for("owner/no-size-here"), IMAGE_SIZE);
        assert_eq!(image_size_for("owner/model-99999"), IMAGE_SIZE);
    }

    use super::*;

    #[test]
    fn text_and_image_towers_agree_on_semantics() {
        let e = Embedder::load(crate::device::best_device(), MODEL_ID).unwrap();
        let red = embed_image_file(&e, std::path::Path::new("tests/fixtures/red_2x2.png")).unwrap();
        let q_red = e.embed_text("a solid red square").unwrap();
        let q_dog = e.embed_text("a photo of a dog").unwrap();
        let dot = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        assert!(dot(&red, &q_red) > dot(&red, &q_dog));
        assert_eq!(red.len(), q_red.len());
    }
}

#[cfg(test)]
mod batch_correctness_tests {
    use super::*;

    /// How to fill the synthetic images a sweep embeds.
    ///
    /// This distinction is load-bearing. The original corruption was
    /// reproduced with 297 *real* files; the first version of this test used
    /// only `Uniform`, where every pixel of an image is the same value. If a
    /// uniform tensor does not drive the same kernels as real image data, a
    /// sweep built on it can report "clean" at a genuinely corrupt batch size,
    /// which would make it worse than no test at all.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Content {
        /// Every pixel of image `i` is the same value, distinct per image.
        Uniform,
        /// Deterministic pseudo-random per pixel, seeded per image.
        Varied,
    }

    /// `n` synthetic images, each distinct, so a bug returning one vector
    /// repeated (or a shifted buffer) cannot pass by accident.
    pub(crate) fn make_images(
        embedder: &Embedder,
        n: usize,
        size: usize,
        content: Content,
    ) -> Vec<Tensor> {
        (0..n)
            .map(|i| match content {
                Content::Uniform => {
                    let v = (i as f32 / n as f32) * 2.0 - 1.0;
                    Tensor::full(v, (3, size, size), &embedder.device).unwrap()
                }
                Content::Varied => {
                    // xorshift, so this needs no rng dependency and is
                    // reproducible across machines and runs.
                    let mut s = (i as u32).wrapping_mul(2_654_435_761).max(1);
                    let data: Vec<f32> = (0..3 * size * size)
                        .map(|_| {
                            s ^= s << 13;
                            s ^= s >> 17;
                            s ^= s << 5;
                            (s as f32 / u32::MAX as f32) * 2.0 - 1.0
                        })
                        .collect();
                    Tensor::from_vec(data, (3, size, size), &embedder.device).unwrap()
                }
            })
            .collect()
    }

    /// Worst cosine between each batched embedding and a one-at-a-time
    /// baseline, plus the index where it occurred.
    ///
    /// This comparison is the only thing that detects the corruption: it
    /// raises no error, produces no NaN, and at batch 256 not even a zero
    /// vector, just plausible garbage. Uses `videre_core::vectors::cosine`
    /// rather than an inline loop so this check and the product cannot
    /// disagree about what similarity means.
    pub(crate) fn worst_cosine_vs_singles(embedder: &Embedder, images: &[Tensor]) -> (f32, usize) {
        worst_cosine_sampled(embedder, images, 1)
    }

    /// As above, but only re-embedding every `stride`-th image singly.
    ///
    /// The baseline is the expensive half: one forward pass per image checked.
    /// On CPU with a 3.5GB model that is minutes per batch, so a full sweep
    /// becomes hours. Sampling is sound here because the corruption is not
    /// sporadic: it takes out essentially every vector in an affected batch
    /// (254 of 254 full-batch entries at `--batch 127`, 256 of 256 at 128), so
    /// a handful of probes either all agree or all disagree.
    ///
    /// Use `stride = 1` whenever the run is cheap enough; this exists to make
    /// the CPU comparison possible at all, not to speed up the Metal one.
    pub(crate) fn worst_cosine_sampled(
        embedder: &Embedder,
        images: &[Tensor],
        stride: usize,
    ) -> (f32, usize) {
        let batched = embedder.embed_images(images).unwrap();
        assert_eq!(batched.len(), images.len(), "one embedding per input");

        let mut worst = f32::MAX;
        let mut worst_at = 0usize;
        for (i, img) in images.iter().enumerate().step_by(stride.max(1)) {
            let single = embedder.embed_images(std::slice::from_ref(img)).unwrap();
            let cos = videre_core::vectors::cosine(&batched[i], &single[0]);
            if cos < worst {
                worst = cos;
                worst_at = i;
            }
        }
        (worst, worst_at)
    }

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
        let embedder = Embedder::load(crate::device::best_device(), MODEL_ID).unwrap();
        let images = make_images(&embedder, MAX_SAFE_BATCH, IMAGE_SIZE, Content::Varied);
        let (worst, worst_at) = worst_cosine_vs_singles(&embedder, &images);

        println!("worst cosine over {MAX_SAFE_BATCH} images: {worst:.6} (index {worst_at})");
        assert!(
            worst > 0.99,
            "batch of {MAX_SAFE_BATCH} disagrees with one-at-a-time embedding (worst cosine \
             {worst:.6} at index {worst_at}); MAX_SAFE_BATCH is too high for this machine"
        );
    }

    /// **Validates the instrument, not the product.**
    ///
    /// Every measurement in
    /// `docs/superpowers/plans/2026-08-09-embed-batch-corruption.md` depends on
    /// `worst_cosine_vs_singles` actually being able to see the corruption. So
    /// this runs the configuration measured corrupt on 2026-08-04 (Metal,
    /// `siglip-so400m-patch14-384`, batch 128) and asserts it is detected as
    /// corrupt.
    ///
    /// A failure here does **not** mean the bug is fixed. It means the
    /// synthetic inputs do not reproduce what 297 real files did, and that
    /// every sweep built on them is worthless. In that case, reproduce with
    /// real files instead, as the original investigation did.
    ///
    /// Reports both input modes, since the first version of the test above
    /// used `Uniform` only and nothing ever checked that uniform tensors can
    /// trigger this at all.
    ///
    /// ```text
    /// cargo test -p videre-ml --release instrument_detects_the_known_bad_configuration -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn instrument_detects_the_known_bad_configuration() {
        const KNOWN_BAD_MODEL: &str = "google/siglip-so400m-patch14-384";
        const KNOWN_BAD_BATCH: usize = 128;

        let embedder = Embedder::load(crate::device::best_device(), KNOWN_BAD_MODEL).unwrap();
        let size = image_size_for(KNOWN_BAD_MODEL);

        let mut detected_by = Vec::new();
        for content in [Content::Uniform, Content::Varied] {
            let images = make_images(&embedder, KNOWN_BAD_BATCH, size, content);
            let (worst, at) = worst_cosine_vs_singles(&embedder, &images);
            println!("{content:?}: worst cosine {worst:.6} at index {at}");
            if worst <= 0.99 {
                detected_by.push(content);
            }
        }

        assert!(
            !detected_by.is_empty(),
            "the instrument did NOT reproduce the corruption at batch {KNOWN_BAD_BATCH} on \
             {KNOWN_BAD_MODEL}, which was measured corrupt on 2026-08-04. Either the defect is \
             gone (check candle's version) or synthetic tensors cannot trigger it, in which case \
             every sweep built on this helper is invalid and real files must be used instead."
        );
        println!("instrument validated; detected by: {detected_by:?}");
    }

    /// Where does a model's batch threshold sit?
    ///
    /// Walks a ladder of batch sizes and prints clean/corrupt for each, so the
    /// boundary can be read off directly. Stops early once a size is corrupt:
    /// the phenomenon is monotonic (everything at or above the threshold is
    /// affected), and the sizes above it are the expensive ones to run.
    fn report_boundary(model_id: &str, ladder: &[usize], stride: usize) {
        let embedder = Embedder::load(crate::device::best_device(), model_id).unwrap();
        let size = image_size_for(model_id);
        println!("\n=== {model_id} ({size}px) ===");
        let mut last_clean = 0usize;
        for &n in ladder {
            let images = make_images(&embedder, n, size, Content::Varied);
            let started = std::time::Instant::now();
            let (worst, at) = worst_cosine_sampled(&embedder, &images, stride);
            let verdict = if worst > 0.99 { "clean" } else { "CORRUPT" };
            println!(
                "  batch {n:>4}: worst cosine {worst:.6} at {at:>4}  {verdict}  ({:?})",
                started.elapsed()
            );
            if worst > 0.99 {
                last_clean = n;
            } else {
                println!("  -> boundary between {last_clean} and {n}");
                return;
            }
        }
        println!("  -> no corruption found up to {}", ladder.last().unwrap());
    }

    /// Is the threshold a count of images, or a budget of bytes?
    ///
    /// This decides whether `MAX_SAFE_BATCH` can legitimately be one constant.
    /// Every candidate root cause (a Metal buffer or threadgroup limit,
    /// unified-memory pressure) scales with bytes in flight, and bytes per
    /// image differ sharply between these two models: 384px/1152-dim against
    /// 224px/768-dim is roughly 3x the activation footprint.
    ///
    /// If the smaller model's boundary sits ~3x higher, the limit is bytes and
    /// a single constant is wrong for any model it was not measured on, which
    /// today includes the default. If both break at the same count, a constant
    /// is defensible.
    ///
    /// The constant was measured only on so400m, before the project gained
    /// per-model databases and `videre config set model`.
    ///
    /// ```text
    /// cargo test -p videre-ml --release threshold_by_model_size -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn threshold_by_model_size() {
        const STRIDE: usize = 8;
        report_boundary(
            "google/siglip-so400m-patch14-384",
            &[96, 120, 127, 128, 160],
            STRIDE,
        );
        report_boundary(
            "google/siglip-base-patch16-224",
            &[96, 128, 192, 256, 384, 512, 768],
            STRIDE,
        );
    }

    /// Does the boundary sit exactly where a 32-bit byte offset would overflow?
    ///
    /// The attention scores tensor is the largest intermediate in the forward
    /// pass: `batch * heads * patches^2 * 4` bytes in f32. Evaluated at the
    /// measured boundaries:
    ///
    /// | model | bytes/image | clean | corrupt |
    /// |---|---|---|---|
    /// | so400m-384 | 34,012,224 | 120 -> 3.801 GB | 127 -> 4.023 GB |
    ///
    /// 2^32 bytes is 4.000 GiB, which falls **inside** that interval. If a
    /// 32-bit offset is overflowing somewhere in candle's Metal backend, the
    /// boundary is exactly `floor(2^32 / bytes_per_image)`: 126 clean, 127
    /// corrupt for so400m.
    ///
    /// That is a one-image-resolution prediction, so it is a real test rather
    /// than a fit. If 126 is clean and 127 is corrupt, the mechanism is
    /// identified and `max_safe_batch` can be computed exactly for any model.
    /// If 126 is corrupt, the hypothesis is wrong and the boundary is
    /// something else near the same magnitude.
    ///
    /// ```text
    /// cargo test -p videre-ml --release thirty_two_bit_offset_boundary -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn thirty_two_bit_offset_boundary() {
        // (model, heads, patches). Patch counts: (384/14)^2 and (224/16)^2.
        // base-224's config.json only overrides patch_size, so heads/hidden
        // come from candle's SigLIP defaults.
        // (model, heads, patches, mlp intermediate). The boundary is set by
        // whichever single intermediate crosses 2^32 bytes first, which is
        // attention for so400m and the MLP for base.
        let cases = [
            (
                "google/siglip-so400m-patch14-384",
                16usize,
                729usize,
                4304usize,
            ),
            ("google/siglip-base-patch16-224", 12, 196, 3072),
        ];

        for (model, heads, patches, inter) in cases {
            let attn = heads * patches * patches * 4;
            let mlp = patches * inter * 4;
            let bytes_per_image = attn.max(mlp);
            let predicted_last_clean = (1usize << 32) / bytes_per_image;
            let embedder = Embedder::load(crate::device::best_device(), model).unwrap();
            let size = image_size_for(model);
            println!("\n=== {model} ===");
            println!(
                "  {bytes_per_image} B/image -> predicted last clean batch {predicted_last_clean}"
            );

            for n in [predicted_last_clean, predicted_last_clean + 1] {
                let images = make_images(&embedder, n, size, Content::Varied);
                let (worst, at) = worst_cosine_sampled(&embedder, &images, 8);
                let gib = (n * bytes_per_image) as f64 / (1024.0 * 1024.0 * 1024.0);
                println!(
                    "  batch {n}: attention {gib:.4} GiB, worst cosine {worst:.6} at {at}  {}",
                    if worst > 0.99 { "clean" } else { "CORRUPT" }
                );
            }
            println!(
                "  hypothesis holds if {predicted_last_clean} is clean and {} is CORRUPT",
                predicted_last_clean + 1
            );
        }
    }

    /// Binary-search a model's exact batch boundary.
    ///
    /// Used after two byte-budget predictions for `base-224` were falsified
    /// (2329 from the attention tensor, 1783 from the MLP intermediate; both
    /// were already corrupt). The so400m boundary matches
    /// `2^32 / attention_bytes` to one image, but one exact fit and two misses
    /// is not a law, so the remaining model gets measured rather than derived.
    ///
    /// ```text
    /// cargo test -p videre-ml --release exact_boundary_for_the_default_model -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn exact_boundary_for_the_default_model() {
        const MODEL: &str = "google/siglip-base-patch16-224";
        let embedder = Embedder::load(crate::device::best_device(), MODEL).unwrap();
        let size = image_size_for(MODEL);

        // Known from the ladder sweep: 768 clean, 1783 corrupt.
        let (mut lo, mut hi) = (768usize, 1783usize);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            let images = make_images(&embedder, mid, size, Content::Varied);
            let (worst, _) = worst_cosine_sampled(&embedder, &images, 8);
            let clean = worst > 0.99;
            println!(
                "  batch {mid}: worst cosine {worst:.6}  {}",
                if clean { "clean" } else { "CORRUPT" }
            );
            if clean {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        println!("  exact boundary for {MODEL}: {lo} clean, {hi} CORRUPT");
    }

    /// **Not ignored**: this is the permanent guard, and it runs in CI.
    ///
    /// Uses `Device::Cpu` deliberately. The defect is in candle's Metal
    /// backend (CPU was bit-identical where Metal gave 0.670), so on Linux CI
    /// this is green and guards the CPU path against the defect ever spreading
    /// there. If it ever fails, the bug is materially worse than currently
    /// believed, since Linux would be affected too.
    ///
    /// Batch is `VERIFY_ABOVE`, the smallest size the self-check applies to, so
    /// this also proves the guard does not reject a correct batch. Kept modest
    /// because CPU inference is slow: this pays one forward pass per image
    /// sampled, not per image in the batch.
    ///
    /// Skips loudly when weights are not cached, matching the policy that
    /// tests never download; CI warms the cache in an explicit step.
    #[test]
    fn cpu_batch_matches_single_image_baseline() {
        if !videre_core::hf_cache::siglip_ready(MODEL_ID) {
            eprintln!(
                "SKIP cpu_batch_matches_single_image_baseline: {MODEL_ID} weights are not in {}. \
                 Run `videre embed` once to populate it; tests never download.",
                videre_core::hf_cache::cache_dir().display()
            );
            return;
        }

        let embedder = Embedder::load(candle_core::Device::Cpu, MODEL_ID).unwrap();
        let size = image_size_for(MODEL_ID);
        let images = make_images(&embedder, VERIFY_ABOVE, size, Content::Varied);
        // Stride so the baseline costs a handful of passes, not VERIFY_ABOVE.
        let (worst, at) = worst_cosine_sampled(&embedder, &images, VERIFY_ABOVE / 4);

        assert!(
            worst > 0.99,
            "CPU batch of {VERIFY_ABOVE} disagrees with a one-at-a-time baseline (worst cosine \
             {worst:.6} at index {at}). The batch corruption was Metal-only when measured on \
             2026-08-09; this failing means it now affects CPU, and therefore Linux."
        );
    }

    /// The guard turns silent corruption into a hard error.
    ///
    /// End-to-end proof rather than a unit test of the comparison: runs the
    /// configuration measured corrupt (so400m at 127, above `MAX_SAFE_BATCH`
    /// so the CLI would never reach it, but the library API allows it) and
    /// asserts `embed_images` now returns `Err` instead of plausible garbage.
    ///
    /// Also checks the safe path still works, since a guard that rejected
    /// everything would pass the first assertion and be useless.
    ///
    /// ```text
    /// cargo test -p videre-ml --release guard_rejects_a_corrupt_batch -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn guard_rejects_a_corrupt_batch() {
        const MODEL: &str = "google/siglip-so400m-patch14-384";
        let embedder = Embedder::load(crate::device::best_device(), MODEL).unwrap();
        let size = image_size_for(MODEL);

        let corrupt = make_images(&embedder, 127, size, Content::Varied);
        let err = embedder
            .embed_images(&corrupt)
            .expect_err("batch 127 is measured corrupt; the guard must reject it");
        let msg = format!("{err:#}");
        println!("rejected as expected: {msg}");
        assert!(msg.contains("refusing to write"), "{msg}");
        assert!(
            msg.contains("127"),
            "error should name the batch size: {msg}"
        );

        let safe = make_images(&embedder, MAX_SAFE_BATCH, size, Content::Varied);
        let started = std::time::Instant::now();
        let ok = embedder
            .embed_images(&safe)
            .expect("a batch at MAX_SAFE_BATCH must still succeed");
        assert_eq!(ok.len(), MAX_SAFE_BATCH);
        println!(
            "batch {MAX_SAFE_BATCH} still succeeds in {:?} (includes the extra verification pass)",
            started.elapsed()
        );
    }

    /// What the guard costs on the path it actually runs on.
    ///
    /// One extra forward pass per checked batch, so it should be ~1/N. Printed
    /// rather than asserted, since a threshold here would be a flaky timing
    /// test; the number is the point.
    ///
    /// ```text
    /// cargo test -p videre-ml --release guard_overhead -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn guard_overhead() {
        let embedder = Embedder::load(crate::device::best_device(), MODEL_ID).unwrap();
        let size = image_size_for(MODEL_ID);
        let images = make_images(&embedder, MAX_SAFE_BATCH, size, Content::Varied);

        // Warm up, so the first run's lazy allocation is not counted.
        let _ = embedder.embed_images_unchecked(&images).unwrap();

        let t0 = std::time::Instant::now();
        let _ = embedder.embed_images_unchecked(&images).unwrap();
        let without = t0.elapsed();

        let t1 = std::time::Instant::now();
        let _ = embedder.embed_images(&images).unwrap();
        let with = t1.elapsed();

        println!(
            "batch {MAX_SAFE_BATCH}: {without:?} unchecked, {with:?} checked ({:.1}% overhead)",
            (with.as_secs_f64() / without.as_secs_f64() - 1.0) * 100.0
        );
    }

    /// Is the corruption Metal-specific?
    ///
    /// Runs the known-bad configuration on `Device::Cpu` instead of
    /// `best_device()`. `Embedder::load` already takes a device, so this needs
    /// no production change.
    ///
    /// A clean CPU result where Metal is corrupt puts the defect in candle's
    /// Metal backend and makes this an upstream bug. A corrupt CPU result
    /// makes it far more serious than believed, since Linux would be affected
    /// too.
    ///
    /// Prints rather than asserts a direction: this is a measurement, and
    /// which way it comes out is the finding. It only asserts the run
    /// completed and produced finite numbers.
    ///
    /// ```text
    /// cargo test -p videre-ml --release cpu_result_at_the_known_bad_batch -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn cpu_result_at_the_known_bad_batch() {
        const KNOWN_BAD_MODEL: &str = "google/siglip-so400m-patch14-384";
        const KNOWN_BAD_BATCH: usize = 128;
        // Every 16th image: ~8 baseline passes instead of 128. See
        // worst_cosine_sampled for why sampling is sound for this defect.
        const STRIDE: usize = 16;

        let embedder = Embedder::load(candle_core::Device::Cpu, KNOWN_BAD_MODEL).unwrap();
        let size = image_size_for(KNOWN_BAD_MODEL);
        let images = make_images(&embedder, KNOWN_BAD_BATCH, size, Content::Varied);

        let started = std::time::Instant::now();
        let (worst, at) = worst_cosine_sampled(&embedder, &images, STRIDE);
        let elapsed = started.elapsed();

        println!(
            "CPU / {KNOWN_BAD_MODEL} / batch {KNOWN_BAD_BATCH}: worst cosine {worst:.6} at index \
             {at}, {elapsed:?}"
        );
        println!(
            "{}",
            if worst > 0.99 {
                "CPU is CLEAN at a batch size Metal corrupts -> Metal-backend defect"
            } else {
                "CPU is ALSO CORRUPT -> not Metal-specific, affects Linux too"
            }
        );
        assert!(worst.is_finite(), "comparison produced a non-finite cosine");
    }
}
