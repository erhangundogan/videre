//! `videre gallery`: one local server for browsing a library.
//!
//! Every view is a route rather than a flag, so moving between them is a link
//! rather than a second command. That is only possible with a live backend:
//! face click-through and reverse-geocoded place names both need one, which is
//! why the old `report --faces` and `report --show-faces` were servers while
//! the other modes wrote files.
//!
//! Rendering a set a command just produced is the other half, and stays static:
//! see `dedupe --html` and `search --html`.

mod server;

use std::path::PathBuf;

#[derive(clap::Args)]
pub struct GalleryArgs {
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,

    /// Embedding model backing the in-page similarity search
    /// (default: 'videre config set model', else the built-in default).
    #[arg(long, value_parser = super::parse_model_id)]
    model: Option<String>,

    /// Port to listen on
    #[arg(long, default_value_t = 7878)]
    port: u16,

    /// Open the gallery in your browser once the server is listening
    #[arg(long)]
    browse: bool,
}

pub fn run(args: GalleryArgs) -> anyhow::Result<()> {
    let db = super::resolve_reader_db(args.db.clone())?;
    if !db.exists() {
        eprintln!("Error: {:?} does not exist", db);
        std::process::exit(1);
    }
    server::serve_gallery(
        &db,
        videre_core::embeddings::resolve_model_id(args.model.as_deref())?,
        args.port,
        args.browse,
    )
}
