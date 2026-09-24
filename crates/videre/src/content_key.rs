//! A file's identity with its metadata left out.
//!
//! `file_hashes.hash` is the key every piece of user and derived data hangs
//! off (face names, marks, tags, embeddings, thumbnails). Hashing whole files
//! made any metadata edit (an EXIF rotate, a date or keyword change in another
//! application) turn the same photo into a new one and orphan everything
//! attached to it. So the key is BLAKE3 over the file's content only, and a
//! second hash covers the metadata, to record that it changed.
//!
//! Each format is split into byte spans by reading block headers only; the
//! spans are then hashed in one pass. Bytes that are neither content nor
//! metadata (container headers, box and IFD layout) feed neither hasher, so
//! rewriting them around unchanged pixels changes nothing. A file that does
//! not parse, or in which no image or media data is found, falls back to the
//! full-file hash with no metadata hash, which is exactly the old behaviour:
//! never wrong, only less forgiving. Without that second rule, every file
//! holding nothing but metadata would share the hash of empty content, and
//! dedupe would offer to delete all but one of them.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::Range;

/// The two hashes one read of a file produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keys {
    /// BLAKE3 of the content without metadata: the file's identity. The
    /// full-file hash when the format is not parsed.
    pub content: String,
    /// BLAKE3 of the metadata alone (of nothing, for a file that has none);
    /// `None` when the format was not parsed.
    pub meta: Option<String>,
}

/// The formats whose metadata can be told apart from their content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Webp,
    Gif,
    Bmp,
    /// TIFF, and DNG, which is TIFF-structured.
    Tiff,
    Heic,
    /// MP4 and QuickTime: both ISO base media files.
    Movie,
}

impl Format {
    /// The format for a type `mime_probe` reports.
    pub fn for_mime(mime: &str) -> Option<Self> {
        Some(match mime {
            "image/jpeg" => Self::Jpeg,
            "image/png" => Self::Png,
            "image/webp" => Self::Webp,
            "image/gif" => Self::Gif,
            "image/bmp" => Self::Bmp,
            "image/tiff" => Self::Tiff,
            "image/heic" => Self::Heic,
            "video/mp4" | "video/quicktime" => Self::Movie,
            _ => return None,
        })
    }
}

enum Span {
    Content(Range<u64>),
    Meta(Range<u64>),
}

/// Bytes read per call, as the whole-file hash always read.
const BUFFER: usize = 64 * 1024;

/// Hash `file` (of `len` bytes) as `format`, or as a whole when the format is
/// unknown or does not parse.
pub fn keys<F: Read + Seek>(file: &mut F, len: u64, format: Option<Format>) -> io::Result<Keys> {
    let mut buf = vec![0u8; BUFFER];
    if let Some(format) = format {
        if let Some(spans) = spans(format, file, len) {
            if let Some(keys) = hash_spans(file, len, &spans, &mut buf)? {
                return Ok(keys);
            }
        }
    }
    file.seek(SeekFrom::Start(0))?;
    let mut content = blake3::Hasher::new();
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        content.update(&buf[..n]);
    }
    Ok(Keys {
        content: content.finalize().to_hex().to_string(),
        meta: None,
    })
}

fn spans<F: Read + Seek>(format: Format, f: &mut F, len: u64) -> Option<Vec<Span>> {
    match format {
        Format::Jpeg => jpeg(f, len),
        Format::Png => png(f, len),
        Format::Webp => webp(f, len),
        Format::Gif => gif(f, len),
        Format::Bmp => Some(vec![Span::Content(0..len)]),
        Format::Tiff => tiff(f, len),
        Format::Heic => heic(f, len),
        Format::Movie => movie(f, len),
    }
}

/// Hash the spans in the order given (logical order, which for TIFF and HEIC
/// is not file order, so a rewrite that moves data keeps the key). `None`
/// when a span lies outside the file.
fn hash_spans<F: Read + Seek>(
    f: &mut F,
    len: u64,
    spans: &[Span],
    buf: &mut [u8],
) -> io::Result<Option<Keys>> {
    let (mut content, mut meta) = (blake3::Hasher::new(), blake3::Hasher::new());
    for span in spans {
        let (range, hasher) = match span {
            Span::Content(r) => (r, &mut content),
            Span::Meta(r) => (r, &mut meta),
        };
        if range.start > range.end || range.end > len || !copy_range(f, range, hasher, buf)? {
            return Ok(None);
        }
    }
    Ok(Some(Keys {
        content: content.finalize().to_hex().to_string(),
        meta: Some(meta.finalize().to_hex().to_string()),
    }))
}

/// Feed `range` of `f` to `hasher`; false when the file ends first.
fn copy_range<F: Read + Seek>(
    f: &mut F,
    range: &Range<u64>,
    hasher: &mut blake3::Hasher,
    buf: &mut [u8],
) -> io::Result<bool> {
    f.seek(SeekFrom::Start(range.start))?;
    let mut left = range.end - range.start;
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            return Ok(false);
        }
        hasher.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(true)
}

/// `n` bytes at `pos`, or `None` when they are not all there. Header reads
/// only; a block's own data is never read here.
fn read_at<F: Read + Seek>(f: &mut F, pos: u64, n: u64, len: u64) -> Option<Vec<u8>> {
    if n > 1 << 24 || pos.checked_add(n)? > len {
        return None;
    }
    f.seek(SeekFrom::Start(pos)).ok()?;
    let mut buf = vec![0u8; n as usize];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// JPEG: every APPn segment (JFIF, EXIF, XMP, ICC, IPTC, MPF, maker data) and
/// COM is metadata; the tables, frame header and everything from the start of
/// scan to the end of the file is content.
fn jpeg<F: Read + Seek>(f: &mut F, len: u64) -> Option<Vec<Span>> {
    if read_at(f, 0, 2, len)? != [0xFF, 0xD8] {
        return None;
    }
    let mut spans = vec![Span::Content(0..2)];
    let mut pos = 2u64;
    loop {
        let m = read_at(f, pos, 2, len)?;
        if m[0] != 0xFF {
            return None;
        }
        let marker = m[1];
        if marker == 0xFF {
            // A fill byte before the marker.
            spans.push(Span::Content(pos..pos + 1));
            pos += 1;
            continue;
        }
        if marker == 0xDA {
            // Start of scan: the rest of the file is image data.
            spans.push(Span::Content(pos..len));
            return Some(spans);
        }
        if marker == 0xD9 {
            // The end of the image before any scan: no image data.
            return None;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            spans.push(Span::Content(pos..pos + 2));
            pos += 2;
            continue;
        }
        let l = read_at(f, pos + 2, 2, len)?;
        let seg = u16::from_be_bytes([l[0], l[1]]) as u64;
        if seg < 2 {
            return None;
        }
        let end = pos + 2 + seg;
        if end > len {
            return None;
        }
        if (0xE0..=0xEF).contains(&marker) || marker == 0xFE {
            spans.push(Span::Meta(pos..end));
        } else {
            spans.push(Span::Content(pos..end));
        }
        pos = end;
    }
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const PNG_META: [&[u8; 4]; 6] = [b"tEXt", b"zTXt", b"iTXt", b"eXIf", b"tIME", b"iCCP"];

/// PNG: text, EXIF, time and ICC chunks are metadata; every other chunk is
/// content. A PNG without image data (`IDAT`) is not parsed.
fn png<F: Read + Seek>(f: &mut F, len: u64) -> Option<Vec<Span>> {
    if read_at(f, 0, 8, len)? != PNG_SIGNATURE {
        return None;
    }
    let mut spans = vec![Span::Content(0..8)];
    let mut image = false;
    let mut pos = 8u64;
    while pos < len {
        let h = read_at(f, pos, 8, len)?;
        let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as u64;
        let kind: [u8; 4] = [h[4], h[5], h[6], h[7]];
        let end = pos.checked_add(12 + n)?;
        if end > len {
            return None;
        }
        if PNG_META.iter().any(|m| **m == kind) {
            spans.push(Span::Meta(pos..end));
        } else {
            image |= &kind == b"IDAT";
            spans.push(Span::Content(pos..end));
        }
        pos = end;
        if &kind == b"IEND" {
            if pos < len {
                spans.push(Span::Content(pos..len));
            }
            return image.then_some(spans);
        }
    }
    None
}

/// WebP: EXIF, XMP and ICC chunks are metadata; every other chunk is content,
/// except two headers that are neither. The RIFF header's size field changes
/// with any chunk. The VP8X header only says which of the other chunks are
/// present (each hashed on its own) and repeats the image's size; a writer
/// adding EXIF to a simple WebP creates it, so it must not change the key.
fn webp<F: Read + Seek>(f: &mut F, len: u64) -> Option<Vec<Span>> {
    let h = read_at(f, 0, 12, len)?;
    if &h[0..4] != b"RIFF" || &h[8..12] != b"WEBP" {
        return None;
    }
    let mut spans = Vec::new();
    let mut image = false;
    let mut pos = 12u64;
    while pos + 8 <= len {
        let c = read_at(f, pos, 8, len)?;
        let n = u32::from_le_bytes([c[4], c[5], c[6], c[7]]) as u64;
        if pos + 8 + n > len {
            return None;
        }
        // Chunks are padded to even length; a missing final pad byte is
        // tolerated.
        let end = (pos + 8 + n + (n & 1)).min(len);
        match &c[0..4] {
            b"EXIF" | b"XMP " | b"ICCP" => spans.push(Span::Meta(pos..end)),
            b"VP8X" => {}
            kind => {
                image |= matches!(kind, b"VP8 " | b"VP8L" | b"ANMF");
                spans.push(Span::Content(pos..end));
            }
        }
        pos = end;
    }
    image.then_some(spans)
}

/// GIF: comment extensions and the XMP application extension are metadata;
/// every other block (including other application extensions, such as the
/// loop count) is content. A GIF without an image is not parsed.
fn gif<F: Read + Seek>(f: &mut F, len: u64) -> Option<Vec<Span>> {
    let h = read_at(f, 0, 13, len)?;
    if &h[0..3] != b"GIF" {
        return None;
    }
    let table = |flags: u8| -> u64 {
        if flags & 0x80 != 0 {
            3 * (1u64 << ((flags & 7) + 1))
        } else {
            0
        }
    };
    let mut pos = 13 + table(h[10]);
    let mut spans = vec![Span::Content(0..pos)];
    let mut image = false;
    loop {
        let block = read_at(f, pos, 1, len)?[0];
        match block {
            0x3B => {
                spans.push(Span::Content(pos..len));
                return image.then_some(spans);
            }
            0x2C => {
                let d = read_at(f, pos, 10, len)?;
                let data = pos + 10 + table(d[9]) + 1;
                let end = skip_sub_blocks(f, data, len)?;
                spans.push(Span::Content(pos..end));
                image = true;
                pos = end;
            }
            0x21 => {
                let label = read_at(f, pos + 1, 1, len)?[0];
                let meta = match label {
                    0xFE => true,
                    0xFF => read_at(f, pos + 2, 12, len)
                        .is_some_and(|id| id[0] == 11 && &id[1..12] == b"XMP DataXMP"),
                    _ => false,
                };
                let end = skip_sub_blocks(f, pos + 2, len)?;
                spans.push(if meta {
                    Span::Meta(pos..end)
                } else {
                    Span::Content(pos..end)
                });
                pos = end;
            }
            _ => return None,
        }
    }
}

/// The position after a chain of GIF data sub-blocks starting at `pos`.
fn skip_sub_blocks<F: Read + Seek>(f: &mut F, mut pos: u64, len: u64) -> Option<u64> {
    loop {
        let n = read_at(f, pos, 1, len)?[0] as u64;
        pos += 1;
        if n == 0 {
            return Some(pos);
        }
        pos += n;
        if pos > len {
            return None;
        }
    }
}

/// TIFF and DNG: the strip or tile data of every full-resolution image (IFD0,
/// its chain and SubIFDs) is content, in IFD and strip order; every other byte
/// (header, IFD entries, their values, and reduced-resolution copies such as
/// thumbnails and the previews an editor regenerates) is metadata, as the
/// thumbnail inside a JPEG's EXIF is. BigTIFF, and a TIFF without
/// full-resolution strips or tiles, is not parsed.
fn tiff<F: Read + Seek>(f: &mut F, len: u64) -> Option<Vec<Span>> {
    let h = read_at(f, 0, 8, len)?;
    let le = match &h[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |b: &[u8]| {
        if le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }
    };
    let u32_at = |b: &[u8]| {
        if le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        }
    };
    if u16_at(&h[2..4]) != 42 {
        return None;
    }
    let mut content: Vec<Range<u64>> = Vec::new();
    let mut queue = VecDeque::from([u32_at(&h[4..8]) as u64]);
    let mut seen = HashSet::new();
    while let Some(ifd) = queue.pop_front() {
        if ifd == 0 || !seen.insert(ifd) {
            continue;
        }
        if seen.len() > 256 {
            return None;
        }
        let count = u16_at(&read_at(f, ifd, 2, len)?) as u64;
        let entries = read_at(f, ifd + 2, count * 12, len)?;
        let mut tags: BTreeMap<u16, Vec<u64>> = BTreeMap::new();
        for e in entries.chunks_exact(12) {
            let tag = u16_at(&e[0..2]);
            if !matches!(tag, 254 | 273 | 279 | 324 | 325 | 330) {
                continue;
            }
            let size: u64 = match u16_at(&e[2..4]) {
                3 => 2,
                4 | 13 => 4,
                _ => return None,
            };
            let n = u32_at(&e[4..8]) as u64;
            let raw = if n * size <= 4 {
                e[8..12].to_vec()
            } else {
                read_at(f, u32_at(&e[8..12]) as u64, n * size, len)?
            };
            let values = raw
                .chunks_exact(size as usize)
                .take(n as usize)
                .map(|v| {
                    if size == 2 {
                        u16_at(v) as u64
                    } else {
                        u32_at(v) as u64
                    }
                })
                .collect();
            tags.insert(tag, values);
        }
        // NewSubfileType bit 0: a reduced-resolution copy of another image.
        let reduced = tags
            .get(&254)
            .and_then(|v| v.first())
            .is_some_and(|t| t & 1 == 1);
        for (offsets, counts) in [(273u16, 279u16), (324, 325)] {
            if reduced {
                break;
            }
            if let (Some(o), Some(c)) = (tags.get(&offsets), tags.get(&counts)) {
                if o.len() != c.len() {
                    return None;
                }
                for (&start, &n) in o.iter().zip(c) {
                    let end = start.checked_add(n)?;
                    if end > len {
                        return None;
                    }
                    content.push(start..end);
                }
            }
        }
        if let Some(sub) = tags.get(&330) {
            queue.extend(sub.iter().copied());
        }
        let next = read_at(f, ifd + 2 + count * 12, 4, len)?;
        queue.push_back(u32_at(&next) as u64);
    }
    if content.is_empty() {
        return None;
    }
    let mut spans: Vec<Span> = content.iter().cloned().map(Span::Content).collect();
    let mut sorted = content;
    sorted.sort_by_key(|r| r.start);
    let mut pos = 0u64;
    for r in sorted {
        if r.start > pos {
            spans.push(Span::Meta(pos..r.start));
        }
        pos = pos.max(r.end);
    }
    if pos < len {
        spans.push(Span::Meta(pos..len));
    }
    Some(spans)
}

/// One ISO base media box: its type, where it starts, where its payload
/// starts, where it ends.
struct Bmff {
    kind: [u8; 4],
    start: u64,
    payload: u64,
    end: u64,
}

fn bmff_boxes<F: Read + Seek>(f: &mut F, range: Range<u64>, len: u64) -> Option<Vec<Bmff>> {
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

/// MP4 and QuickTime: the media samples (`mdat` payloads) are content; every
/// other box (`moov` with its `udta` and `meta`, `free`, `wide`, ...) is
/// metadata. A movie without an `mdat` is not parsed.
fn movie<F: Read + Seek>(f: &mut F, len: u64) -> Option<Vec<Span>> {
    let boxes = bmff_boxes(f, 0..len, len)?;
    if !boxes.iter().any(|b| &b.kind == b"mdat") {
        return None;
    }
    Some(
        boxes
            .into_iter()
            .map(|b| {
                if &b.kind == b"mdat" {
                    Span::Content(b.payload..b.end)
                } else {
                    Span::Meta(b.start..b.end)
                }
            })
            .collect(),
    )
}

/// HEIC: the data of every item except `Exif` and `mime` (XMP) items is
/// content, in item order, plus the `hvcC` decoder configurations; the `Exif`
/// and `mime` items' data is metadata. Everything else (box structure, `irot`,
/// `ispe`) is neither. A HEIC whose only items are metadata is not parsed.
fn heic<F: Read + Seek>(f: &mut F, len: u64) -> Option<Vec<Span>> {
    let top = bmff_boxes(f, 0..len, len)?;
    let meta = top.iter().find(|b| &b.kind == b"meta")?;
    // `meta` is a full box: four bytes of version and flags before its children.
    let children = bmff_boxes(f, meta.payload + 4..meta.end, len)?;
    let find = |kind: &[u8; 4]| children.iter().find(|b| &b.kind == kind);

    let mut item_types: BTreeMap<u32, [u8; 4]> = BTreeMap::new();
    let iinf = find(b"iinf")?;
    let v = read_at(f, iinf.payload, 1, len)?[0];
    let first = iinf.payload + 4 + if v == 0 { 2 } else { 4 };
    for infe in bmff_boxes(f, first..iinf.end, len)? {
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

    let idat = find(b"idat").map(|b| b.payload);
    let iloc = find(b"iloc")?;
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

    let mut content = Vec::new();
    let mut metadata = Vec::new();
    for (id, ranges) in extents {
        let is_meta = matches!(item_types.get(&id), Some(k) if k == b"Exif" || k == b"mime");
        for r in ranges {
            if is_meta {
                metadata.push(Span::Meta(r));
            } else {
                content.push(Span::Content(r));
            }
        }
    }
    if content.is_empty() {
        return None;
    }
    if let Some(iprp) = find(b"iprp") {
        for ipco in bmff_boxes(f, iprp.payload..iprp.end, len)? {
            if &ipco.kind == b"ipco" {
                for property in bmff_boxes(f, ipco.payload..ipco.end, len)? {
                    if &property.kind == b"hvcC" {
                        content.push(Span::Content(property.start..property.end));
                    }
                }
            }
        }
    }
    content.extend(metadata);
    Some(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn keys_of(bytes: &[u8], format: Format) -> Keys {
        keys(
            &mut Cursor::new(bytes.to_vec()),
            bytes.len() as u64,
            Some(format),
        )
        .unwrap()
    }

    fn full(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    /// A metadata-only change keeps the content key and changes the metadata
    /// hash; a content change changes the key.
    fn assert_split(original: &[u8], metadata_edit: &[u8], pixel_edit: &[u8], format: Format) {
        let a = keys_of(original, format);
        let b = keys_of(metadata_edit, format);
        let c = keys_of(pixel_edit, format);
        assert!(a.meta.is_some(), "{format:?} was parsed");
        assert!(b.meta.is_some(), "{format:?} edit was parsed");
        assert_eq!(
            a.content, b.content,
            "{format:?}: metadata edit keeps the key"
        );
        assert_ne!(
            a.meta, b.meta,
            "{format:?}: metadata edit changes meta_hash"
        );
        assert_ne!(
            a.content, c.content,
            "{format:?}: pixel edit changes the key"
        );
    }

    /// Parsed without error but with nothing to identify it by: the whole
    /// file is the key, as for a file that does not parse.
    fn assert_whole_file(bytes: &[u8], format: Format) {
        let k = keys_of(bytes, format);
        assert_eq!(
            k,
            Keys {
                content: full(bytes),
                meta: None
            },
            "{format:?}"
        );
    }

    fn jpeg_with(app1: Option<&[u8]>, scan: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        if let Some(app1) = app1 {
            v.extend([0xFF, 0xE1]);
            v.extend(((app1.len() + 2) as u16).to_be_bytes());
            v.extend(app1);
        }
        v.extend([0xFF, 0xDB, 0x00, 0x04, 0x00, 0x01]); // a tiny DQT
        v.extend([0xFF, 0xDA, 0x00, 0x02]);
        v.extend(scan);
        v.extend([0xFF, 0xD9]);
        v
    }

    #[test]
    fn jpeg_app_segments_are_metadata() {
        assert_split(
            &jpeg_with(Some(b"Exif\0\0date=2011"), b"pixels"),
            &jpeg_with(Some(b"Exif\0\0date=2024 and a longer comment"), b"pixels"),
            &jpeg_with(Some(b"Exif\0\0date=2011"), b"pixelz"),
            Format::Jpeg,
        );
        // A photo with no metadata gains an EXIF segment (a first rotate).
        assert_split(
            &jpeg_with(None, b"pixels"),
            &jpeg_with(Some(b"Exif\0\0orientation=6"), b"pixels"),
            &jpeg_with(None, b"pixelz"),
            Format::Jpeg,
        );
    }

    #[test]
    fn a_file_without_metadata_is_keyed_by_all_of_it() {
        let plain = jpeg_with(None, b"pixels");
        let k = keys_of(&plain, Format::Jpeg);
        assert_eq!(k.content, full(&plain));
        assert_eq!(k.meta, Some(full(b"")));
    }

    fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = (data.len() as u32).to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(data);
        v.extend([0, 0, 0, 0]); // CRC is hashed as content, never checked
        v
    }

    fn png_with(text: Option<&[u8]>, idat: Option<&[u8]>) -> Vec<u8> {
        let mut v = PNG_SIGNATURE.to_vec();
        v.extend(png_chunk(b"IHDR", &[0; 13]));
        if let Some(text) = text {
            v.extend(png_chunk(b"tEXt", text));
        }
        if let Some(idat) = idat {
            v.extend(png_chunk(b"IDAT", idat));
        }
        v.extend(png_chunk(b"IEND", &[]));
        v
    }

    #[test]
    fn png_text_chunks_are_metadata() {
        assert_split(
            &png_with(Some(b"Title\0Arsiv"), Some(b"data")),
            &png_with(Some(b"Title\0Arsiv 2011, edited"), Some(b"data")),
            &png_with(Some(b"Title\0Arsiv"), Some(b"dota")),
            Format::Png,
        );
        assert_split(
            &png_with(None, Some(b"data")),
            &png_with(Some(b"Title\0Arsiv"), Some(b"data")),
            &png_with(None, Some(b"dota")),
            Format::Png,
        );
    }

    fn riff_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = kind.to_vec();
        v.extend((data.len() as u32).to_le_bytes());
        v.extend(data);
        if data.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    /// A WebP from its chunks, in order.
    fn webp_of(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (kind, data) in chunks {
            body.extend(riff_chunk(kind, data));
        }
        let mut v = b"RIFF".to_vec();
        v.extend((body.len() as u32).to_le_bytes());
        v.extend(body);
        v
    }

    #[test]
    fn webp_metadata_chunks_and_the_extended_header_are_not_content() {
        // A simple WebP gains the extended header, a profile and EXIF, as a
        // writer adding an orientation creates them.
        let mut flags = [0u8; 10];
        flags[0] = 0x28;
        assert_split(
            &webp_of(&[(b"VP8L", b"image")]),
            &webp_of(&[
                (b"VP8X", &flags),
                (b"ICCP", b"profile"),
                (b"VP8L", b"image"),
                (b"EXIF", b"Exif\0\0orientation=6"),
            ]),
            &webp_of(&[(b"VP8L", b"imagf")]),
            Format::Webp,
        );
    }

    fn gif_with(comment: &[u8], image: Option<&[u8]>) -> Vec<u8> {
        let mut v = b"GIF89a".to_vec();
        v.extend([1, 0, 1, 0, 0, 0, 0]); // 1x1, no global table
        v.extend([0x21, 0xFE, comment.len() as u8]);
        v.extend(comment);
        v.push(0);
        if let Some(image) = image {
            v.extend([0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, 2, image.len() as u8]);
            v.extend(image);
            v.push(0);
        }
        v.push(0x3B);
        v
    }

    #[test]
    fn gif_comments_are_metadata() {
        assert_split(
            &gif_with(b"Arsiv", Some(b"ab")),
            &gif_with(b"Arsiv 2011", Some(b"ab")),
            &gif_with(b"Arsiv", Some(b"ac")),
            Format::Gif,
        );
    }

    /// A little-endian TIFF: one IFD with an ASCII tag and, when given, one
    /// strip, placed before or after the tag's text.
    fn tiff_with(description: &[u8], strip: Option<&[u8]>, strip_first: bool) -> Vec<u8> {
        let strip_bytes = strip.unwrap_or(b"");
        let mut v = b"II".to_vec();
        v.extend(42u16.to_le_bytes());
        let header = 8u32;
        let body_len = (strip_bytes.len() + description.len()) as u32;
        let (strip_at, desc_at) = if strip_first {
            (header, header + strip_bytes.len() as u32)
        } else {
            (header + description.len() as u32, header)
        };
        let ifd_at = header + body_len;
        v.extend(ifd_at.to_le_bytes());
        let mut body = vec![0u8; body_len as usize];
        body[(strip_at - header) as usize..][..strip_bytes.len()].copy_from_slice(strip_bytes);
        body[(desc_at - header) as usize..][..description.len()].copy_from_slice(description);
        v.extend(body);
        let entry = |tag: u16, kind: u16, count: u32, value: u32| {
            let mut e = tag.to_le_bytes().to_vec();
            e.extend(kind.to_le_bytes());
            e.extend(count.to_le_bytes());
            e.extend(value.to_le_bytes());
            e
        };
        v.extend((if strip.is_some() { 3u16 } else { 1 }).to_le_bytes());
        v.extend(entry(270, 2, description.len() as u32, desc_at));
        if strip.is_some() {
            v.extend(entry(273, 4, 1, strip_at));
            v.extend(entry(279, 4, 1, strip_bytes.len() as u32));
        }
        v.extend(0u32.to_le_bytes());
        v
    }

    #[test]
    fn tiff_strips_are_the_content_wherever_they_sit() {
        let original = tiff_with(b"Arsiv 2011 ", Some(b"pixels"), true);
        // Metadata rewritten and the strip moved after it: same content.
        let rewritten = tiff_with(b"Arsiv 2024, edited ", Some(b"pixels"), false);
        let pixel_edit = tiff_with(b"Arsiv 2011 ", Some(b"pixelz"), true);
        assert_split(&original, &rewritten, &pixel_edit, Format::Tiff);
    }

    /// A little-endian TIFF whose IFD0 holds the image strip and whose IFD1,
    /// marked reduced-resolution, holds a preview strip.
    fn tiff_with_preview(strip: &[u8], preview: &[u8]) -> Vec<u8> {
        let entry = |tag: u16, kind: u16, count: u32, value: u32| {
            let mut e = tag.to_le_bytes().to_vec();
            e.extend(kind.to_le_bytes());
            e.extend(count.to_le_bytes());
            e.extend(value.to_le_bytes());
            e
        };
        let strip_at = 8u32;
        let preview_at = strip_at + strip.len() as u32;
        let ifd0_at = preview_at + preview.len() as u32;
        let ifd1_at = ifd0_at + 2 + 2 * 12 + 4;
        let mut v = b"II".to_vec();
        v.extend(42u16.to_le_bytes());
        v.extend(ifd0_at.to_le_bytes());
        v.extend(strip);
        v.extend(preview);
        v.extend(2u16.to_le_bytes());
        v.extend(entry(273, 4, 1, strip_at));
        v.extend(entry(279, 4, 1, strip.len() as u32));
        v.extend(ifd1_at.to_le_bytes());
        v.extend(3u16.to_le_bytes());
        v.extend(entry(254, 4, 1, 1));
        v.extend(entry(273, 4, 1, preview_at));
        v.extend(entry(279, 4, 1, preview.len() as u32));
        v.extend(0u32.to_le_bytes());
        v
    }

    #[test]
    fn tiff_previews_are_metadata() {
        assert_split(
            &tiff_with_preview(b"pixels", b"preview"),
            &tiff_with_preview(b"pixels", b"new preview"),
            &tiff_with_preview(b"pixelz", b"preview"),
            Format::Tiff,
        );
    }

    fn bmff(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(payload);
        v
    }

    fn movie_with(moov: &[u8], samples: Option<&[u8]>) -> Vec<u8> {
        let mut v = bmff(b"ftyp", b"isom");
        v.extend(bmff(b"moov", moov));
        if let Some(samples) = samples {
            v.extend(bmff(b"mdat", samples));
        }
        v
    }

    #[test]
    fn movie_samples_are_the_content() {
        assert_split(
            &movie_with(b"udta date=2011", Some(b"samples")),
            &movie_with(b"udta date=2024, edited", Some(b"samples")),
            &movie_with(b"udta date=2011", Some(b"sampler")),
            Format::Movie,
        );
    }

    /// A minimal HEIC holding `items` (type, data), all stored in `mdat`,
    /// and one `hvcC` property.
    fn heic_of(items: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let infe = |id: u16, kind: &[u8; 4]| {
            let mut p = vec![2, 0, 0, 0];
            p.extend(id.to_be_bytes());
            p.extend([0, 0]);
            p.extend(kind);
            p.push(0);
            bmff(b"infe", &p)
        };
        let mut iinf = vec![0, 0, 0, 0];
        iinf.extend((items.len() as u16).to_be_bytes());
        for (i, (kind, _)) in items.iter().enumerate() {
            iinf.extend(infe(i as u16 + 1, kind));
        }
        let iprp = bmff(b"iprp", &bmff(b"ipco", &bmff(b"hvcC", b"config")));
        // iloc v0: offset 4 bytes, length 4 bytes, no base offset. Header,
        // version/flags, size bytes, item count, then per item: id, data
        // reference, extent count, offset, length.
        let iloc_len = 8 + 4 + 2 + 2 + items.len() * (2 + 2 + 2 + 4 + 4);
        let meta_len = 8 + 4 + bmff(b"iinf", &iinf).len() + iprp.len() + iloc_len;
        let ftyp = bmff(b"ftyp", b"heic");
        let mut offset = (ftyp.len() + meta_len + 8) as u32;
        let mut iloc = vec![0, 0, 0, 0, 0x44, 0x00];
        iloc.extend((items.len() as u16).to_be_bytes());
        for (i, (_, data)) in items.iter().enumerate() {
            iloc.extend((i as u16 + 1).to_be_bytes());
            iloc.extend([0, 0]);
            iloc.extend(1u16.to_be_bytes());
            iloc.extend(offset.to_be_bytes());
            iloc.extend((data.len() as u32).to_be_bytes());
            offset += data.len() as u32;
        }
        let mut meta = vec![0, 0, 0, 0];
        meta.extend(bmff(b"iinf", &iinf));
        meta.extend(iprp);
        meta.extend(bmff(b"iloc", &iloc));
        let mut v = ftyp;
        v.extend(bmff(b"meta", &meta));
        v.extend(bmff(
            b"mdat",
            &items
                .iter()
                .flat_map(|(_, d)| d.iter().copied())
                .collect::<Vec<_>>(),
        ));
        v
    }

    #[test]
    fn heic_exif_items_are_metadata() {
        assert_split(
            &heic_of(&[(b"hvc1", b"image"), (b"Exif", b"Exif date=2011")]),
            &heic_of(&[(b"hvc1", b"image"), (b"Exif", b"Exif date=2024 edited")]),
            &heic_of(&[(b"hvc1", b"imagf"), (b"Exif", b"Exif date=2011")]),
            Format::Heic,
        );
    }

    #[test]
    fn bmp_is_all_content() {
        let k = keys_of(b"BMpixels", Format::Bmp);
        assert_eq!(k.content, full(b"BMpixels"));
        assert!(k.meta.is_some());
    }

    #[test]
    fn a_file_with_only_metadata_is_keyed_by_all_of_it() {
        // SOI, an EXIF segment, EOI: no scan.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x06];
        jpeg.extend(b"Exif");
        jpeg.extend([0xFF, 0xD9]);
        assert_whole_file(&jpeg, Format::Jpeg);
        assert_whole_file(&png_with(Some(b"Title\0Arsiv"), None), Format::Png);
        assert_whole_file(
            &webp_of(&[
                (b"VP8X", &[8, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
                (b"EXIF", b"Exif"),
            ]),
            Format::Webp,
        );
        assert_whole_file(&gif_with(b"Arsiv", None), Format::Gif);
        assert_whole_file(&tiff_with(b"Arsiv 2011 ", None, true), Format::Tiff);
        assert_whole_file(&movie_with(b"udta date=2011", None), Format::Movie);
        assert_whole_file(&heic_of(&[(b"Exif", b"Exif date=2011")]), Format::Heic);
    }

    #[test]
    fn a_file_that_does_not_parse_falls_back_to_the_whole_file() {
        let truncated = &jpeg_with(Some(b"Exif"), b"pixels")[..7];
        assert_whole_file(truncated, Format::Jpeg);
        let unknown = keys(&mut Cursor::new(b"anything".to_vec()), 8, None).unwrap();
        assert_eq!(
            unknown,
            Keys {
                content: full(b"anything"),
                meta: None
            }
        );
        let empty = keys(&mut Cursor::new(Vec::new()), 0, Some(Format::Jpeg)).unwrap();
        assert_eq!(
            empty,
            Keys {
                content: full(b""),
                meta: None
            }
        );
    }
}
