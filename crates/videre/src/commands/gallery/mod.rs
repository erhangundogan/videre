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

mod events;
mod learning;
mod rotate;
mod server;

use crate::command_context::CommandContext;

#[derive(clap::Args)]
pub struct GalleryArgs {
    /// Embedding model backing the in-page similarity search
    /// (default: 'videre config set model', else the built-in default).
    #[arg(long, value_parser = super::parse_model_id)]
    model: Option<String>,

    /// Port to listen on. Without the flag: start at 7878 and advance to the
    /// next free port when taken. With the flag: use exactly this port.
    #[arg(long)]
    port: Option<u16>,

    /// Open the gallery in your browser once the server is listening
    #[arg(long)]
    browse: bool,
}

// Kept fail-closed in main's dispatch until C8 binds the server's image, cache
// and request paths to the context; the signature and context threading are in
// place so C8 only flips the dispatch arm.
pub fn run(args: GalleryArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    if !ctx.library.paths.db.exists() {
        anyhow::bail!("{:?} does not exist", ctx.library.paths.db);
    }
    // The server's long-lived connection is opened by the generic WAL
    // helper; run the versioned library preparation under its own upgrade
    // locks before that connection can serve reads or write labels.
    drop(videre_core::library_db::open_existing(&ctx.library)?);
    let model_id = videre_core::embeddings::resolve_model_id_from(
        &ctx.library.settings,
        args.model.as_deref(),
    )?;
    server::serve_gallery(ctx, model_id, args.port, args.browse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Wrap {
        #[command(flatten)]
        args: GalleryArgs,
    }

    #[test]
    fn port_is_none_without_the_flag_and_some_with_it() {
        // None drives the 7878-with-fallback default; Some pins an exact port.
        assert_eq!(Wrap::parse_from(["gallery"]).args.port, None);
        assert_eq!(
            Wrap::parse_from(["gallery", "--port", "9000"]).args.port,
            Some(9000)
        );
    }
}
