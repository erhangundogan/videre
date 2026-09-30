//! Turning a HEIC a quarter turn in place.
//!
//! A HEIC's displayed rotation is the HEIF `irot` item property, which
//! QuickLook applies; the EXIF Orientation tag beside it is only a copy for
//! applications that read EXIF. So a turn changes the angle byte of every
//! `irot` box that belongs to the primary image or to an item linked to it
//! (an iPhone's thumbnail and depth or gain-map images share the primary's
//! box), and the matching Orientation value. Both edits are the same size as
//! what they replace: no box grows, no `iloc` offset moves, and no content
//! byte changes, so the content key stays and only `meta_hash` changes.
//!
//! A file whose primary has no `irot` is refused rather than given one:
//! inserting a property grows `meta` and shifts every item offset.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::ops::Range;

use crate::bmff::{self, read_at};

/// Why a HEIC cannot be turned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The primary image has no `irot` property to change.
    NoRotation,
    /// The file does not parse as HEIF.
    Malformed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NoRotation => write!(f, "this HEIC has no rotation property to change"),
            Error::Malformed => write!(f, "this file does not parse as HEIC"),
        }
    }
}

impl std::error::Error for Error {}

/// A HEIC's rotation and the bytes a turn rewrites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeifRotation {
    /// The primary image's `irot`: counter-clockwise quarter turns, 0 to 3.
    pub angle: u8,
    /// The primary image's size as displayed: `ispe` with `angle` applied.
    pub display: (u32, u32),
    /// File offsets of the angle byte of every `irot` a turn changes, each
    /// once, however many items share it.
    pub irot_at: Vec<u64>,
    /// File offset of the EXIF Orientation value, and whether its TIFF data
    /// is big-endian. `None` when there is no Orientation tag to keep in step.
    pub orientation_at: Option<(u64, bool)>,
}

/// The EXIF Orientation that displays like an `irot` of `angle`.
pub fn orientation_for(angle: u8) -> u16 {
    match angle & 3 {
        0 => 1,
        1 => 8,
        2 => 3,
        _ => 6,
    }
}

/// `angle` turned a quarter clockwise, or counter-clockwise when `ccw`.
/// `irot` counts counter-clockwise, so clockwise is three steps on.
pub fn turned(angle: u8, ccw: bool) -> u8 {
    if ccw {
        (angle + 1) % 4
    } else {
        (angle + 3) % 4
    }
}

/// Read `bytes` as HEIF: the primary's angle, its displayed size, and where a
/// turn writes.
pub fn read(bytes: &[u8]) -> Result<HeifRotation, Error> {
    parse(bytes).ok_or(Error::Malformed)?
}

/// `bytes` turned a quarter clockwise, or counter-clockwise when `ccw`, as a
/// same-length copy. Each `irot` turns by one step from its own angle, so an
/// item stored at a different angle from the primary keeps its difference.
pub fn rotated(bytes: &[u8], ccw: bool) -> Result<Vec<u8>, Error> {
    let r = read(bytes)?;
    let mut out = bytes.to_vec();
    for &at in &r.irot_at {
        let b = &mut out[at as usize];
        *b = (*b & !3) | turned(*b & 3, ccw);
    }
    if let Some((at, big)) = r.orientation_at {
        let value = orientation_for(turned(r.angle, ccw));
        let v = if big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        out[at as usize..at as usize + 2].copy_from_slice(&v);
    }
    Ok(out)
}

/// `None` for a file that does not parse; `Some(Err(NoRotation))` for one
/// that parses but whose primary has no `irot`.
fn parse(bytes: &[u8]) -> Option<Result<HeifRotation, Error>> {
    let len = bytes.len() as u64;
    let f = &mut Cursor::new(bytes);
    let children = bmff::meta_children(f, len)?;
    let types = bmff::item_types(f, &children, len)?;

    let pitm = bmff::find(&children, b"pitm")?;
    let head = read_at(f, pitm.payload, 8.min(pitm.end - pitm.payload), len)?;
    let primary = match head.first()? {
        0 => u16::from_be_bytes(head.get(4..6)?.try_into().ok()?) as u32,
        _ => u32::from_be_bytes(head.get(4..8)?.try_into().ok()?),
    };
    let related = related_items(f, &children, primary, len)?;

    // Properties are numbered from 1 in ipco order.
    let iprp = bmff::find(&children, b"iprp")?;
    let iprp_children = bmff::boxes(f, iprp.payload..iprp.end, len)?;
    let ipco = bmff::find(&iprp_children, b"ipco")?;
    let properties = bmff::boxes(f, ipco.payload..ipco.end, len)?;
    let associations = associations(f, &iprp_children, len)?;
    let property_of = |item: u32, kind: &[u8; 4]| -> Option<&bmff::Bmff> {
        associations
            .get(&item)?
            .iter()
            .filter_map(|&i| properties.get(i.checked_sub(1)? as usize))
            .find(|p| &p.kind == kind)
    };

    let Some(irot) = property_of(primary, b"irot") else {
        return Some(Err(Error::NoRotation));
    };
    if irot.payload >= irot.end {
        return None;
    }
    let angle = bytes[irot.payload as usize] & 3;
    let irot_at: BTreeSet<u64> = related
        .iter()
        .filter_map(|&item| property_of(item, b"irot"))
        .filter(|p| p.payload < p.end)
        .map(|p| p.payload)
        .collect();

    // ispe is a full box: version and flags, then width and height.
    let ispe = property_of(primary, b"ispe")?;
    let wh = read_at(f, ispe.payload + 4, 8, len)?;
    let (w, h) = (
        u32::from_be_bytes(wh[0..4].try_into().ok()?),
        u32::from_be_bytes(wh[4..8].try_into().ok()?),
    );
    let display = if angle % 2 == 1 { (h, w) } else { (w, h) };

    let extents = bmff::item_extents(f, &children, len)?;
    let orientation_at = types
        .iter()
        .find(|(_, kind)| *kind == b"Exif")
        .and_then(|(id, _)| extents.get(id))
        .and_then(|ranges| exif_orientation(bytes, ranges));

    Some(Ok(HeifRotation {
        angle,
        display,
        irot_at: irot_at.into_iter().collect(),
        orientation_at,
    }))
}

/// The primary and every item linked to it through `iref`, in either
/// direction: its thumbnail, auxiliary images (depth, gain map) and, for a
/// grid, its tiles. The `Exif` item's `cdsc` link is harmless: it has no
/// `irot`.
fn related_items(
    f: &mut Cursor<&[u8]>,
    children: &[bmff::Bmff],
    primary: u32,
    len: u64,
) -> Option<BTreeSet<u32>> {
    let mut related = BTreeSet::from([primary]);
    let Some(iref) = bmff::find(children, b"iref") else {
        return Some(related);
    };
    let version = read_at(f, iref.payload, 1, len)?[0];
    let id_size = if version == 0 { 2 } else { 4 };
    for reference in bmff::boxes(f, iref.payload + 4..iref.end, len)? {
        let body = read_at(f, reference.payload, reference.end - reference.payload, len)?;
        let id = |at: usize| -> Option<u32> {
            let b = body.get(at..at + id_size)?;
            Some(b.iter().fold(0u32, |acc, x| (acc << 8) | *x as u32))
        };
        let from = id(0)?;
        let count = u16::from_be_bytes(body.get(id_size..id_size + 2)?.try_into().ok()?) as usize;
        let to: Vec<u32> = (0..count)
            .map(|i| id(id_size + 2 + i * id_size))
            .collect::<Option<_>>()?;
        if from == primary {
            related.extend(&to);
        } else if to.contains(&primary) {
            related.insert(from);
        }
    }
    Some(related)
}

/// Every item's property indices from `ipma`.
fn associations(
    f: &mut Cursor<&[u8]>,
    iprp_children: &[bmff::Bmff],
    len: u64,
) -> Option<BTreeMap<u32, Vec<u16>>> {
    let mut all: BTreeMap<u32, Vec<u16>> = BTreeMap::new();
    for ipma in iprp_children.iter().filter(|b| &b.kind == b"ipma") {
        let body = read_at(f, ipma.payload, ipma.end - ipma.payload, len)?;
        let version = *body.first()?;
        let wide = body.get(3)? & 1 == 1;
        let mut at = 4usize;
        let mut take = |n: usize| -> Option<u32> {
            let b = body.get(at..at + n)?;
            at += n;
            Some(b.iter().fold(0u32, |acc, x| (acc << 8) | *x as u32))
        };
        let count = take(4)?;
        for _ in 0..count {
            let item = take(if version < 1 { 2 } else { 4 })?;
            let n = take(1)?;
            let list = all.entry(item).or_default();
            for _ in 0..n {
                let index = if wide {
                    take(2)? & 0x7FFF
                } else {
                    take(1)? & 0x7F
                };
                list.push(index as u16);
            }
        }
    }
    Some(all)
}

/// Where the Orientation value sits in an `Exif` item, as a file offset, and
/// whether the TIFF data is big-endian. The item's data is a 4-byte offset to
/// the TIFF header, then TIFF; only IFD0 is searched, where the tag lives.
fn exif_orientation(bytes: &[u8], ranges: &[Range<u64>]) -> Option<(u64, bool)> {
    // The item's data may be split across extents: address it as one run.
    let data: Vec<u8> = ranges
        .iter()
        .flat_map(|r| bytes.get(r.start as usize..r.end as usize).unwrap_or(&[]))
        .copied()
        .collect();
    let to_file = |mut at: usize| -> Option<u64> {
        for r in ranges {
            let n = (r.end - r.start) as usize;
            if at < n {
                return Some(r.start + at as u64);
            }
            at -= n;
        }
        None
    };
    let tiff = 4 + u32::from_be_bytes(data.get(0..4)?.try_into().ok()?) as usize;
    let big = match data.get(tiff..tiff + 2)? {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = data.get(at..at + 2)?.try_into().ok()?;
        Some(if big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = data.get(at..at + 4)?.try_into().ok()?;
        Some(if big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    };
    let ifd0 = tiff.checked_add(u32_at(tiff + 4)? as usize)?;
    let entries = u16_at(ifd0)? as usize;
    for i in 0..entries {
        let entry = ifd0 + 2 + i * 12;
        // Tag 0x0112, type SHORT, one value: stored inline at entry + 8.
        if u16_at(entry)? == 0x0112 {
            if u16_at(entry + 2)? != 3 || u32_at(entry + 4)? != 1 {
                return None;
            }
            return Some((to_file(entry + 8)?, big));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rel: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{rel}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn keys(bytes: &[u8]) -> crate::content_key::Keys {
        crate::content_key::keys(
            &mut Cursor::new(bytes),
            bytes.len() as u64,
            Some(crate::content_key::Format::Heic),
        )
        .unwrap()
    }

    /// The Orientation value a rotation wrote, read back through the offset.
    fn orientation(bytes: &[u8]) -> u16 {
        let (at, big) = read(bytes).unwrap().orientation_at.unwrap();
        let b = [bytes[at as usize], bytes[at as usize + 1]];
        if big {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        }
    }

    #[test]
    fn a_single_image_heic_has_one_irot_at_270() {
        let r = read(&fixture("content_key/tiny.heic")).unwrap();
        assert_eq!(r.angle, 3);
        assert_eq!(r.irot_at.len(), 1);
        // ispe is 16x12; a quarter turn displays it 12 wide.
        assert_eq!(r.display, (12, 16));
    }

    #[test]
    fn a_grid_and_its_thumbnail_share_one_irot() {
        let bytes = fixture("heic/grid_rot90.heic");
        let r = read(&bytes).unwrap();
        assert_eq!(r.angle, 3);
        assert_eq!(r.irot_at.len(), 1, "one shared box, changed once");
        assert_eq!(r.display, (1536, 2048));
        assert_eq!(orientation(&bytes), 6);
    }

    #[test]
    fn a_heic_without_irot_is_refused() {
        // Rename the irot property so the primary has none; the ipma index
        // then points at an unknown box, as in a file written without one.
        let mut bytes = fixture("content_key/tiny.heic");
        let at = bytes.windows(4).position(|w| w == b"irot").unwrap();
        bytes[at..at + 4].copy_from_slice(b"frot");
        assert_eq!(read(&bytes), Err(Error::NoRotation));
        assert_eq!(rotated(&bytes, false), Err(Error::NoRotation));
    }

    #[test]
    fn damaged_input_is_an_error_not_a_panic() {
        let bytes = fixture("heic/grid_rot90.heic");
        for cut in [0, 8, 40, 200, 600, bytes.len() / 2] {
            assert!(read(&bytes[..cut]).is_err(), "truncated at {cut}");
        }
        let noise: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        assert!(read(&noise).is_err());
    }

    #[test]
    fn four_clockwise_turns_give_back_the_same_bytes() {
        let original = fixture("heic/grid_rot90.heic");
        let mut bytes = original.clone();
        for _ in 0..4 {
            bytes = rotated(&bytes, false).unwrap();
            assert_eq!(bytes.len(), original.len(), "no box may grow");
        }
        assert_eq!(bytes, original);
    }

    #[test]
    fn a_clockwise_then_counter_clockwise_turn_is_no_change() {
        let original = fixture("heic/grid_rot90.heic");
        let back = rotated(&rotated(&original, false).unwrap(), true).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn a_clockwise_turn_updates_irot_and_exif_together() {
        let turned_bytes = rotated(&fixture("heic/grid_rot90.heic"), false).unwrap();
        let r = read(&turned_bytes).unwrap();
        assert_eq!(r.angle, 2);
        assert_eq!(r.display, (2048, 1536));
        assert_eq!(orientation(&turned_bytes), 3);
    }

    #[test]
    fn a_turn_keeps_the_content_key_and_changes_the_metadata_hash() {
        let original = fixture("heic/grid_rot90.heic");
        let turned_bytes = rotated(&original, false).unwrap();
        let (before, after) = (keys(&original), keys(&turned_bytes));
        assert!(
            before.meta.is_some(),
            "the fixture must parse, not fall back"
        );
        assert_eq!(before.content, after.content);
        assert_ne!(before.meta, after.meta);
    }

    #[test]
    fn orientation_follows_the_angle() {
        assert_eq!(
            (0..4).map(orientation_for).collect::<Vec<_>>(),
            vec![1, 8, 3, 6]
        );
        assert_eq!(turned(3, false), 2);
        assert_eq!(turned(0, false), 3);
        assert_eq!(turned(3, true), 0);
    }
}
