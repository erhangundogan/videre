//! Reading ISO base media (BMFF) box structure: the layout HEIC, MP4 and
//! QuickTime share. Header reads only; an item's own data is never read here.
//!
//! Shared by `content_key`, which splits a file into content and metadata, and
//! `heif_rotate`, which finds the bytes a rotation changes. Both walk the same
//! `meta` children and item tables, so they parse them one way.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

/// `n` bytes at `pos`, or `None` when they are not all there.
pub(crate) fn read_at<F: Read + Seek>(f: &mut F, pos: u64, n: u64, len: u64) -> Option<Vec<u8>> {
    if n > 1 << 24 || pos.checked_add(n)? > len {
        return None;
    }
    f.seek(SeekFrom::Start(pos)).ok()?;
    let mut buf = vec![0u8; n as usize];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// One box: its type, where it starts, where its payload starts, where it
/// ends.
pub(crate) struct Bmff {
    pub kind: [u8; 4],
    pub start: u64,
    pub payload: u64,
    pub end: u64,
}

/// The boxes laid end to end in `range`, or `None` when one overruns it.
pub(crate) fn boxes<F: Read + Seek>(f: &mut F, range: Range<u64>, len: u64) -> Option<Vec<Bmff>> {
    let mut boxes = Vec::new();
    let mut pos = range.start;
    while pos + 8 <= range.end {
        let h = read_at(f, pos, 8, len)?;
        let size = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
        let kind = [h[4], h[5], h[6], h[7]];
        let (header, size) = match size {
            0 => (8, range.end - pos),
            1 => {
                let l = read_at(f, pos + 8, 8, len)?;
                (16, u64::from_be_bytes(l.try_into().ok()?))
            }
            n => (8, n),
        };
        if size < header || pos.checked_add(size)? > range.end {
            return None;
        }
        boxes.push(Bmff {
            kind,
            start: pos,
            payload: pos + header,
            end: pos + size,
        });
        pos += size;
    }
    Some(boxes)
}

/// The children of the top-level `meta` box, which is a full box: four bytes
/// of version and flags come before them.
pub(crate) fn meta_children<F: Read + Seek>(f: &mut F, len: u64) -> Option<Vec<Bmff>> {
    let top = boxes(f, 0..len, len)?;
    let meta = top.iter().find(|b| &b.kind == b"meta")?;
    boxes(f, meta.payload + 4..meta.end, len)
}

/// The first child of type `kind`.
pub(crate) fn find<'a>(children: &'a [Bmff], kind: &[u8; 4]) -> Option<&'a Bmff> {
    children.iter().find(|b| &b.kind == kind)
}

/// Every item's type from `iinf`, by item ID.
pub(crate) fn item_types<F: Read + Seek>(
    f: &mut F,
    children: &[Bmff],
    len: u64,
) -> Option<BTreeMap<u32, [u8; 4]>> {
    let mut item_types = BTreeMap::new();
    let iinf = find(children, b"iinf")?;
    let v = read_at(f, iinf.payload, 1, len)?[0];
    let first = iinf.payload + 4 + if v == 0 { 2 } else { 4 };
    for infe in boxes(f, first..iinf.end, len)? {
        if &infe.kind != b"infe" {
            continue;
        }
        let b = read_at(f, infe.payload, (infe.end - infe.payload).min(16), len)?;
        let (id, kind_at) = match b[0] {
            2 => (u16::from_be_bytes([b[4], b[5]]) as u32, 8),
            3 => (u32::from_be_bytes([b[4], b[5], b[6], b[7]]), 10),
            _ => return None,
        };
        let kind: [u8; 4] = b.get(kind_at..kind_at + 4)?.try_into().ok()?;
        item_types.insert(id, kind);
    }
    Some(item_types)
}

/// Every item's data extents from `iloc`, in file offsets, by item ID. An
/// extent in `idat` (construction method 1) is resolved against it; any
/// other method, a zero-length extent or one past the end is `None`.
pub(crate) fn item_extents<F: Read + Seek>(
    f: &mut F,
    children: &[Bmff],
    len: u64,
) -> Option<BTreeMap<u32, Vec<Range<u64>>>> {
    let idat = find(children, b"idat").map(|b| b.payload);
    let iloc = find(children, b"iloc")?;
    let body = read_at(f, iloc.payload, iloc.end - iloc.payload, len)?;
    let version = *body.first()?;
    let mut at = 4usize;
    let take = |at: &mut usize, n: usize| -> Option<u64> {
        let bytes = body.get(*at..*at + n)?;
        *at += n;
        Some(bytes.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64))
    };
    let sizes = take(&mut at, 1)? as u8;
    let (offset_size, length_size) = ((sizes >> 4) as usize, (sizes & 0xF) as usize);
    let sizes = take(&mut at, 1)? as u8;
    let base_size = (sizes >> 4) as usize;
    let index_size = if version == 1 || version == 2 {
        (sizes & 0xF) as usize
    } else {
        0
    };
    let count = take(&mut at, if version < 2 { 2 } else { 4 })?;
    let mut extents: BTreeMap<u32, Vec<Range<u64>>> = BTreeMap::new();
    for _ in 0..count {
        let id = take(&mut at, if version < 2 { 2 } else { 4 })? as u32;
        let method = if version == 1 || version == 2 {
            take(&mut at, 2)? & 0xF
        } else {
            0
        };
        take(&mut at, 2)?; // data reference index
        let base = take(&mut at, base_size)?;
        let n = take(&mut at, 2)?;
        let origin = match method {
            0 => 0,
            1 => idat?,
            _ => return None,
        };
        for _ in 0..n {
            take(&mut at, index_size)?;
            let offset = take(&mut at, offset_size)?;
            let length = take(&mut at, length_size)?;
            if length == 0 {
                return None;
            }
            let start = origin.checked_add(base)?.checked_add(offset)?;
            let end = start.checked_add(length)?;
            if end > len {
                return None;
            }
            extents.entry(id).or_default().push(start..end);
        }
    }
    Some(extents)
}
