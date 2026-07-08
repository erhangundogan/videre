use anyhow::{Context, Result};
use clap::Parser;
use dupe_core::{embeddings, vectors};
use dupe_ml::{device, model, search};
use rusqlite::Connection;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dupe-search", about = "Semantic image search over a dupe SQLite database")]
struct Args {
    /// SQLite database with embeddings (run dupe-embed first)
    db: PathBuf,

    /// Text query, e.g. "sunset on beach" (omit when using --image)
    query: Option<String>,

    /// Search by example image instead of text
    #[arg(long, conflicts_with = "query")]
    image: Option<PathBuf>,

    /// Number of results
    #[arg(short = 'k', long, default_value_t = 20)]
    top_k: usize,

    /// Prepend the cosine score to each output line
    #[arg(long)]
    scores: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let conn = Connection::open(&args.db)
        .with_context(|| format!("open {}", args.db.display()))?;

    let corpus_raw = embeddings::load_embeddings(&conn, model::MODEL_ID)?;
    anyhow::ensure!(
        !corpus_raw.is_empty(),
        "no embeddings found in {} for model {}; run dupe-embed first",
        args.db.display(),
        model::MODEL_ID
    );
    let corpus: Vec<(String, Vec<f32>)> = corpus_raw
        .into_iter()
        .map(|(hash, blob)| (hash, vectors::from_f16_bytes(&blob)))
        .collect();

    let embedder = model::Embedder::load(device::best_device())?;
    let query_vec = match (&args.query, &args.image) {
        (Some(text), None) => embedder.embed_text(text)?,
        (None, Some(img)) => model::embed_image_file(&embedder, img)?,
        _ => anyhow::bail!("provide either a text query or --image <path>"),
    };

    let hits = search::top_k(&query_vec, &corpus, args.top_k);
    for (hash, score) in hits {
        for path in embeddings::paths_for_hash(&conn, &hash)? {
            if args.scores {
                println!("{score:.4}\t{path}");
            } else {
                println!("{path}");
            }
        }
    }
    Ok(())
}
