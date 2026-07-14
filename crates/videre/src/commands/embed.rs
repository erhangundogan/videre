use anyhow::{Context, Result};
use videre_core::{embeddings, vectors};
use videre_ml::{device, model, preprocess};
use rayon::prelude::*;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct EmbedArgs {
    /// SQLite database produced by: videre dedupe --output-sqlite <db>
    db: PathBuf,

    /// Inference batch size
    #[arg(long, default_value_t = 32)]
    batch: usize,

    /// Rows written per transaction (resume granularity)
    #[arg(long, default_value_t = 500)]
    chunk: usize,

    /// Suppress progress output on stderr (errors always shown)
    #[arg(long)]
    silent: bool,
}

pub fn run(args: EmbedArgs) -> Result<()> {
    let conn = videre_core::db::open_wal(&args.db)
        .with_context(|| format!("open {}", args.db.display()))?;
    embeddings::ensure_embeddings_table(&conn)?;

    let pending = embeddings::pending_images(&conn, model::MODEL_ID)?;
    if pending.is_empty() {
        if !args.silent {
            eprintln!("Nothing to embed: all hashes already have embeddings.");
        }
        return Ok(());
    }
    if !args.silent {
        eprintln!("{} image(s) to embed", pending.len());
    }

    let dev = device::best_device();
    let embedder = model::Embedder::load(dev.clone())?;

    let mut done = 0usize;
    let mut failed = 0usize;
    for chunk in pending.chunks(args.chunk) {
        // Decode in parallel; None = unreadable, logged and skipped.
        let decoded: Vec<Option<(String, candle_core::Tensor)>> = chunk
            .par_iter()
            .map(|p| {
                match preprocess::image_to_tensor(
                    std::path::Path::new(&p.path),
                    model::IMAGE_SIZE,
                    &candle_core::Device::Cpu, // decode on CPU, move to device in batch
                ) {
                    Ok(t) => Some((p.hash.clone(), t)),
                    Err(e) => {
                        eprintln!("skip {}: {e:#}", p.path);
                        None
                    }
                }
            })
            .collect();
        let decoded: Vec<(String, candle_core::Tensor)> =
            decoded.into_iter().flatten().collect();
        failed += chunk.len() - decoded.len();

        let mut rows: Vec<(String, Vec<u8>)> = Vec::with_capacity(decoded.len());
        for batch in decoded.chunks(args.batch) {
            let tensors: Vec<candle_core::Tensor> = batch
                .iter()
                .map(|(_, t)| t.to_device(&dev))
                .collect::<candle_core::Result<_>>()?;
            let vecs = embedder.embed_images(&tensors)?;
            for ((hash, _), v) in batch.iter().zip(vecs) {
                rows.push((hash.clone(), vectors::to_f16_bytes(&v)));
            }
        }

        embeddings::insert_embeddings(&conn, model::MODEL_ID, &rows)?;
        done += rows.len();
        if !args.silent {
            eprintln!("embedded {done}/{} ({failed} skipped)", pending.len());
        }
    }

    if !args.silent {
        eprintln!("Done: {done} embedded, {failed} skipped.");
    }
    Ok(())
}
