//! `videre export --xmp`: write videre's owned labels to `.xmp` sidecars beside
//! each selected photo, merging into any existing sidecar so foreign data is
//! preserved. The deliberate-handoff surface; the same shared writer also runs
//! from the watch export stage.

use crate::xmp::model::{Area, OwnedXmp, Region};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use videre_core::selection::SelectionCtx;

#[derive(clap::Args)]
pub struct ExportArgs {
    #[command(flatten)]
    media: super::selection_args::MediaArgs,
    #[command(flatten)]
    dates: super::selection_args::DateArgs,
    #[command(flatten)]
    place: super::selection_args::PlaceArgs,
    #[command(flatten)]
    people: super::selection_args::PeopleArgs,
    #[command(flatten)]
    paths: super::selection_args::PathArgs,

    /// Write XMP sidecars (currently the only export format)
    #[arg(long)]
    xmp: bool,
    /// Show what would be written, write nothing
    #[arg(long)]
    dry_run: bool,
    /// No summary output
    #[arg(long)]
    silent: bool,
    /// SQLite database (default: resolved from ~/.videre; see 'videre config')
    #[arg(long)]
    db: Option<PathBuf>,
}

pub fn run(args: ExportArgs) -> Result<()> {
    if !args.xmp {
        bail!("nothing to export: pass --xmp");
    }
    let db = super::resolve_reader_db(args.db.clone())?;
    let conn = videre_core::db::open_wal(&db).with_context(|| format!("open {}", db.display()))?;

    export_selection(&conn, &args)
}

/// Resolve the selection and export sidecars for it. Split from `run` so the
/// watch stage can drive an export over an already-open connection.
fn export_selection(conn: &rusqlite::Connection, args: &ExportArgs) -> Result<()> {
    // Ensure the optional tables/columns exist so gathering never hits a missing
    // table on a library that has not run faces/classify/locations. All idempotent.
    let _ = videre_core::face_db::create_faces_table(conn);
    let _ = videre_core::classify::ensure_classifications_table(conn);
    let _ = videre_core::location_cluster::ensure_location_clusters_table(conn);
    videre_core::location_cluster::ensure_location_cluster_id_column(conn);

    let sel = super::selection_args::row_selection(
        Some(&args.media),
        Some(&args.dates),
        Some(&args.place),
        Some(&args.people),
        Some(&args.paths),
    )?;
    let resolved = sel.resolve(conn, &SelectionCtx::default())?;
    let hashes: Vec<String> = match resolved.hashes {
        Some(h) => h.into_iter().collect(),
        None => {
            let mut stmt = conn.prepare("SELECT DISTINCT hash FROM file_hashes")?;
            let rows: Vec<String> = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            rows
        }
    };
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))?;
    let faces = videre_core::face_db::labeled_faces_by_hash(conn)?;

    let mut written = 0usize;
    for hash in &hashes {
        let m = videre_core::marks::get(conn, hash)?;
        let g = videre_core::xmp_gather::gather_for_hash(conn, hash)?;
        let regions = faces
            .get(hash)
            .map(|fs| {
                fs.iter()
                    .filter_map(|(_, name, bbox)| {
                        let (w, h) = g.dims?;
                        Some(Region {
                            name: name.clone(),
                            area: Area::from_pixel_bbox(bbox, w, h)?,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let owned = OwnedXmp {
            rating: m.rating,
            label: m.label.clone(),
            location: g.location.clone(),
            keywords: g.category.clone().into_iter().collect(),
            regions,
            applied_dims: g.dims,
        };
        if owned.is_empty() {
            continue;
        }
        // One hash can map to several paths (duplicates); write beside each.
        let mut stmt = conn.prepare("SELECT path FROM file_hashes WHERE hash = ?1")?;
        let paths = stmt.query_map([hash], |r| r.get::<_, String>(0))?;
        for p in paths {
            let path = PathBuf::from(p?);
            if args.dry_run {
                eprintln!(
                    "would write {}",
                    crate::xmp::write::sidecar_path(&path).display()
                );
            } else if crate::xmp::write::write_sidecar(&path, &owned)? {
                written += 1;
            }
        }
    }
    if !args.silent && !args.dry_run {
        eprintln!(
            "Wrote {written} sidecar(s) for {} of {} file(s)",
            hashes.len(),
            total
        );
    }
    Ok(())
}
