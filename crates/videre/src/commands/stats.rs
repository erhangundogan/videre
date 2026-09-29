use crate::command_context::CommandContext;
use videre::types::{ErrorJson, StatsJson, SCHEMA_VERSION};

#[derive(clap::Args)]
pub struct StatsArgs {
    /// Emit a single JSON object on stdout instead of human-readable text
    #[arg(long)]
    json: bool,
    /// List every file whose extension conflicts with its detected MIME
    #[arg(long)]
    mismatched: bool,
}

pub fn run(args: StatsArgs, ctx: &CommandContext) -> anyhow::Result<()> {
    if args.json {
        match run_json(ctx, args.mismatched) {
            Ok(doc) => {
                println!("{}", serde_json::to_string(&doc)?);
                Ok(())
            }
            Err(e) => {
                println!("{}", serde_json::to_string(&ErrorJson::from_err(&e))?);
                Err(crate::exit::Exit::shown(e).into())
            }
        }
    } else {
        run_text(ctx, args.mismatched)
    }
}

fn run_text(ctx: &CommandContext, all_mismatches: bool) -> anyhow::Result<()> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let library = videre_core::library_stats::compute_full_in(&conn, &ctx.library)?;

    println!(
        "Library: {} file(s) ({}), {} photo(s), {} video(s)",
        library.total_files,
        videre_core::disk::human_bytes(library.total_size_bytes.max(0) as u64),
        library.total_photos,
        library.total_videos,
    );
    println!(
        "Duplicates: {} group(s), {} file(s), {} wasted",
        library.duplicate_group_count,
        library.duplicate_file_count,
        videre_core::disk::human_bytes(library.wasted_bytes.max(0) as u64),
    );
    println!(
        "Faces: {} detected, {} people named",
        library.faces_detected, library.people_named
    );
    println!(
        "Marks: {} rated, {} picked, {} labelled, {} liked",
        library.marks.rated, library.marks.picked, library.marks.labelled, library.marks.liked
    );
    println!();
    println!("Embeddings:");
    if library.embeddings.is_empty() {
        println!("  none; run 'videre embed' to create some");
    } else {
        for e in &library.embeddings {
            println!(
                "  {:38} {:>8} {:>5}-dim {:>10}",
                e.model_id,
                e.count,
                e.dims,
                videre_core::disk::human_bytes(e.size_bytes.max(0) as u64),
            );
        }
    }
    println!();
    println!("By type:");
    let types = videre_core::library_stats::by_type(&conn, 12)?;
    if types.is_empty() {
        println!("  nothing scanned yet");
    } else {
        for ty in &types {
            println!(
                "  {:8} {:24} {:>8} {:>10}",
                ty.ext,
                ty.mime,
                ty.files,
                videre_core::disk::human_bytes(ty.bytes.max(0) as u64),
            );
        }
    }

    let limit = if all_mismatches { None } else { Some(10) };
    let mismatches = videre_core::library_stats::mismatched_files(&conn, limit)?;
    println!();
    println!("Mismatched files: {}", mismatches.count);
    println!("  A mismatch means the filename and last scanned content disagree; verify before renaming.");
    for file in &mismatches.files {
        println!("  {}  {}", file.mime, escape_path_controls(&file.path));
    }
    if mismatches.truncated {
        println!(
            "  ... and {} more",
            mismatches.count - mismatches.files.len()
        );
    }

    println!();
    println!("Disk use:");
    // Every location is derived from the selected library context, so a run
    // reports only that library's own database, embeddings and locks plus the
    // caches it uses, never another library's vectors or thumbnails.
    let usage = videre_core::disk::usage_in(&ctx.library);
    if usage.is_empty() {
        println!("  nothing stored yet");
    } else {
        let total: u64 = usage.iter().map(|u| u.bytes).sum();
        let rebuildable: u64 = usage
            .iter()
            .filter(|u| u.rebuildable)
            .map(|u| u.bytes)
            .sum();
        for u in &usage {
            println!(
                "  {:18} {:>10}  {}",
                u.label,
                videre_core::disk::human_bytes(u.bytes),
                if u.rebuildable { "(rebuildable)" } else { "" },
            );
        }
        println!(
            "  {:18} {:>10}  ({} of it rebuildable)",
            "total",
            videre_core::disk::human_bytes(total),
            videre_core::disk::human_bytes(rebuildable),
        );
    }

    Ok(())
}

/// A path as the user should read it: every printable character as-is,
/// including combining marks, so a decomposed Turkish name stays readable,
/// but control characters and invisible direction or width characters
/// escaped, so a filename can neither drive the terminal nor display
/// reordered (a right-to-left override can make `gnp.jpg` read as `jpg.png`).
fn escape_path_controls(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch.is_control() || is_invisible_format(ch) {
            escaped.extend(ch.escape_default());
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

/// Bidirectional overrides, isolates and marks, and zero-width characters.
fn is_invisible_format(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}'
    )
}

fn run_json(ctx: &CommandContext, all_mismatches: bool) -> anyhow::Result<StatsJson> {
    let conn = videre_core::library_db::open_existing(&ctx.library)?;
    let _activity = videre_core::library_locks::try_activity(
        &ctx.library,
        videre_core::library_locks::ActivityMode::Shared,
    )?;
    let library = videre_core::library_stats::compute_full_in(&conn, &ctx.library)?;
    let limit = if all_mismatches { None } else { Some(10) };
    Ok(StatsJson {
        schema_version: SCHEMA_VERSION,
        library,
        by_type: videre_core::library_stats::by_type(&conn, usize::MAX)?,
        mismatches: videre_core::library_stats::mismatched_files(&conn, limit)?,
        disk_use: videre_core::disk::usage_in(&ctx.library),
    })
}
