//! Read face regions from a sidecar and apply the names to detected faces.
//! Matching is by bounding-box overlap; applying goes through
//! `videre_api::faces::assign` so identity/display handling stays in one place.

use anyhow::Result;
use videre_core::face_match::{greedy_match, PixelBox, DEFAULT_IOU_THRESHOLD};
use videre_core::marks::XmpPrecedence;

/// Parse a `"x,y,w,h"` pixel bbox into a PixelBox. None on malformed input.
fn parse_bbox(s: &str) -> Option<PixelBox> {
    let n: Vec<f64> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    let [x, y, w, h] = n.as_slice() else {
        return None;
    };
    Some(PixelBox {
        x: *x,
        y: *y,
        w: *w,
        h: *h,
    })
}

/// Match `regions` (center-based normalized) to the detected faces of `hash`
/// using the image dimensions, and assign each matched region's name to its face
/// under `prec`. Returns the number of faces newly labeled.
///
/// `Db` precedence never overrides a face that is already confirmed; `File`
/// always assigns; `Newest` behaves as `Db` (the caller warns once).
pub fn apply_regions(
    conn: &rusqlite::Connection,
    hash: &str,
    regions: &[crate::xmp::read::ReadRegion],
    prec: XmpPrecedence,
) -> Result<usize> {
    if regions.is_empty() {
        return Ok(0);
    }
    // Image dimensions are required to denormalize regions to pixels.
    let dims: Option<(f64, f64)> = conn
        .query_row(
            "SELECT width, height FROM file_hashes
             WHERE hash = ?1 AND width IS NOT NULL AND height IS NOT NULL",
            [hash],
            |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)),
        )
        .ok();
    let Some((iw, ih)) = dims else {
        return Ok(0);
    };

    // Load this hash's faces: id, bbox, and whether already confirmed.
    let mut stmt =
        conn.prepare("SELECT id, bbox, COALESCE(confirmed, 0) FROM faces WHERE hash = ?1")?;
    let rows = stmt
        .query_map([hash], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(0);
    }
    let confirmed: std::collections::HashMap<i64, bool> =
        rows.iter().map(|(id, _, c)| (*id, *c != 0)).collect();
    let faces: Vec<(i64, PixelBox)> = rows
        .iter()
        .filter_map(|(id, bbox, _)| parse_bbox(bbox).map(|b| (*id, b)))
        .collect();

    // Denormalize each region (center-based normalized) to a top-left pixel box.
    let region_boxes: Vec<PixelBox> = regions
        .iter()
        .map(|r| PixelBox {
            x: (r.cx - r.w / 2.0) * iw,
            y: (r.cy - r.h / 2.0) * ih,
            w: r.w * iw,
            h: r.h * ih,
        })
        .collect();

    let matched = greedy_match(&region_boxes, &faces, DEFAULT_IOU_THRESHOLD);
    let mut applied = 0usize;
    for (ri, face) in matched.iter().enumerate() {
        let Some(face_id) = face else { continue };
        // Db (and Newest) never override a confirmed face; File always assigns.
        if !matches!(prec, XmpPrecedence::File) && *confirmed.get(face_id).unwrap_or(&false) {
            continue;
        }
        videre_api::assign(conn, &[*face_id], &regions[ri].name)
            .map_err(|e| anyhow::anyhow!("assign face {face_id}: {e:?}"))?;
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use videre_core::marks::XmpPrecedence;

    fn faces_schema(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE file_hashes (path TEXT, hash TEXT, width INTEGER, height INTEGER);
             CREATE TABLE people (name TEXT PRIMARY KEY, full_name TEXT NOT NULL);
             CREATE TABLE faces (id INTEGER PRIMARY KEY, hash TEXT NOT NULL, bbox TEXT NOT NULL,
                person_label TEXT, confirmed INTEGER DEFAULT 0);",
        )
        .unwrap();
    }

    fn region(name: &str, cx: f64, cy: f64, w: f64, h: f64) -> crate::xmp::read::ReadRegion {
        crate::xmp::read::ReadRegion {
            name: name.into(),
            cx,
            cy,
            w,
            h,
        }
    }

    #[test]
    fn applies_a_region_name_to_the_overlapping_face() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        faces_schema(&conn);
        // 1000x1000 image; one detected face at pixel box 400,300,150,200.
        conn.execute(
            "INSERT INTO file_hashes VALUES ('/p.jpg','h1',1000,1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (hash,bbox) VALUES ('h1','400,300,150,200')",
            [],
        )
        .unwrap();
        // A region centered on that same box.
        let regions = vec![region("Ayşe", 475.0 / 1000.0, 400.0 / 1000.0, 0.15, 0.20)];
        let n = apply_regions(&conn, "h1", &regions, XmpPrecedence::File).unwrap();
        assert_eq!(n, 1);
        let (label, confirmed): (String, i64) = conn
            .query_row(
                "SELECT person_label, confirmed FROM faces WHERE hash='h1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(label, "ayse"); // normalized identity
        assert_eq!(confirmed, 1);
        let full: String = conn
            .query_row("SELECT full_name FROM people WHERE name='ayse'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(full, "Ayşe"); // display name upserted
    }

    #[test]
    fn db_precedence_does_not_override_a_confirmed_face() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        faces_schema(&conn);
        conn.execute(
            "INSERT INTO file_hashes VALUES ('/p.jpg','h1',1000,1000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO faces (hash,bbox,person_label,confirmed) VALUES ('h1','400,300,150,200','mehmet',1)",
            [],
        )
        .unwrap();
        let regions = vec![region("Ayşe", 0.475, 0.40, 0.15, 0.20)];
        let n = apply_regions(&conn, "h1", &regions, XmpPrecedence::Db).unwrap();
        assert_eq!(n, 0); // db precedence leaves the confirmed label alone
        let label: String = conn
            .query_row("SELECT person_label FROM faces WHERE hash='h1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(label, "mehmet");
    }

    #[test]
    fn no_faces_or_no_dims_is_a_no_op() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        faces_schema(&conn);
        // dims present but no faces.
        conn.execute(
            "INSERT INTO file_hashes VALUES ('/p.jpg','h1',1000,1000)",
            [],
        )
        .unwrap();
        let regions = vec![region("Ayşe", 0.5, 0.5, 0.1, 0.1)];
        assert_eq!(
            apply_regions(&conn, "h1", &regions, XmpPrecedence::File).unwrap(),
            0
        );
    }
}
