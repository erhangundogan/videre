//! Orientation-aware decode of source image files.
//!
//! Contract: every function returns the image **as a human sees it**,
//! display canvas, by reading the EXIF Orientation tag through the
//! `image` crate's decoder and applying it.
//!
//! :warning: **Never call these on files videre itself produced.** QuickLook
//! conversions (`videre_core::heic::decode_via_quicklook`) and the
//! thumbnail/`original` caches are already upright pixels with no
//! orientation tag; running them through here would double-rotate. HEIC and
//! video therefore do not route through this module.

use std::path::Path;

use image::ImageDecoder;

pub fn decode_oriented_file(path: &Path) -> image::ImageResult<image::DynamicImage> {
    let (img, orientation) = decode_raw_with_orientation(path)?;
    Ok(apply(img, orientation))
}

/// Reader variant of [`decode_oriented_file`]: the same contract for an
/// already-opened, already-validated handle, so callers that must not reopen
/// the path (library I/O rules) keep that property.
pub fn decode_oriented_reader<R: std::io::BufRead + std::io::Seek>(
    reader: R,
) -> image::ImageResult<image::DynamicImage> {
    let (img, orientation) = decode_raw_with_orientation_reader(reader)?;
    Ok(apply(img, orientation))
}

pub fn decode_oriented_bytes(bytes: &[u8]) -> Option<image::DynamicImage> {
    let decoder = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let (img, orientation) = decode_decoder(decoder).ok()?;
    Some(apply(img, orientation))
}

pub fn decode_raw_with_orientation(
    path: &Path,
) -> image::ImageResult<(image::DynamicImage, image::metadata::Orientation)> {
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    decode_raw_with_orientation_reader(reader)
}

pub fn decode_raw_with_orientation_reader<R: std::io::BufRead + std::io::Seek>(
    reader: R,
) -> image::ImageResult<(image::DynamicImage, image::metadata::Orientation)> {
    let decoder = image::ImageReader::new(reader)
        .with_guessed_format()?
        .into_decoder()?;
    decode_decoder(decoder)
}

fn decode_decoder<D: ImageDecoder>(
    mut decoder: D,
) -> image::ImageResult<(image::DynamicImage, image::metadata::Orientation)> {
    let mut orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // The `image` crate reads the EXIF Orientation only for JPEG, WebP and TIFF;
    // its PNG decoder always reports NoTransforms even when the file carries an
    // eXIf orientation (which is how the gallery's rotate button turns a PNG). So
    // when the decoder claims no transform, fall back to the raw EXIF chunk it
    // exposes and read the orientation from there. Only PNG needs this in
    // practice; for the formats that already resolve it, `orientation` is set and
    // this branch is skipped.
    if matches!(orientation, image::metadata::Orientation::NoTransforms) {
        if let Ok(Some(chunk)) = decoder.exif_metadata() {
            if let Some(from_exif) = image::metadata::Orientation::from_exif_chunk(&chunk) {
                orientation = from_exif;
            }
        }
    }
    let img = image::DynamicImage::from_decoder(decoder)?;
    Ok((img, orientation))
}

/// 64-bit difference hash of a decoded image: greyscale, Lanczos3 down to
/// 9x8, one bit per left-greater-than-right comparison along each row. This is
/// the near-duplicate fingerprint stored in `file_hashes.phash`; changing any
/// step changes every stored value, so it lives in exactly one place.
pub fn dhash(image: &image::DynamicImage) -> u64 {
    let small = image::imageops::resize(
        &image.to_luma8(),
        9,
        8,
        image::imageops::FilterType::Lanczos3,
    );
    let mut hash = 0u64;
    for row in 0..8u32 {
        for col in 0..8u32 {
            let left = small.get_pixel(col, row)[0];
            let right = small.get_pixel(col + 1, row)[0];
            hash = (hash << 1) | u64::from(left > right);
        }
    }
    hash
}

/// What confirms that two images with near fingerprints are one picture: the
/// upright canvas reduced to 16x16 greyscale, and its width over height. A
/// resize or recompression barely moves the thumbnail; a different shot of a
/// burst, which a fingerprint can place a few bits away, differs where the
/// subject moved. Taken from the image `dhash` reads, already upright.
pub struct PixelSignature {
    pub luma: [u8; 256],
    pub aspect: f32,
}

pub fn pixel_signature(image: &image::DynamicImage) -> PixelSignature {
    let small = image::imageops::resize(
        &image.to_luma8(),
        16,
        16,
        image::imageops::FilterType::Lanczos3,
    );
    let mut luma = [0u8; 256];
    luma.copy_from_slice(small.as_raw());
    PixelSignature {
        luma,
        aspect: image.width() as f32 / image.height().max(1) as f32,
    }
}

/// Mean absolute difference of two signatures' greyscale values, 0 to 255.
pub fn signature_mad(a: &[u8; 256], b: &[u8; 256]) -> f32 {
    let sum: u32 = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u32).sum();
    sum as f32 / 256.0
}

fn apply(
    mut img: image::DynamicImage,
    orientation: image::metadata::Orientation,
) -> image::DynamicImage {
    img.apply_orientation(orientation);
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImage;

    /// Build a tiny JPEG with the given EXIF orientation tag by inserting an
    /// APP1 EXIF segment after the SOI marker. The segment is a minimal
    /// big-endian TIFF: IFD0 with a single SHORT entry, tag 0x0112.
    fn jpeg_with_orientation(orientation: u16) -> Vec<u8> {
        let mut img = image::DynamicImage::new_rgb8(4, 2);
        // Asymmetric pixels: row 0 white, row 1 black, so orientation can be
        // asserted from decoded pixel geometry, not just dimensions.
        for y in 0..2u32 {
            for x in 0..4u32 {
                img.put_pixel(
                    x,
                    y,
                    if y == 0 {
                        image::Rgba([255, 255, 255, 255])
                    } else {
                        image::Rgba([0, 0, 0, 255])
                    },
                );
            }
        }
        let mut jpeg = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut jpeg),
            image::ImageFormat::Jpeg,
        )
        .unwrap();

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM");
        tiff.extend_from_slice(&42u16.to_be_bytes());
        tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 offset
        tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
        tiff.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
        tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_be_bytes()); // count
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&0u16.to_be_bytes()); // value padding
        tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD

        let mut app1 = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);

        let mut out = Vec::new();
        out.extend_from_slice(&jpeg[0..2]); // SOI
        out.extend_from_slice(&[0xFF, 0xE1]); // APP1 marker
        out.extend_from_slice(&(app1.len() as u16 + 2).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn top_left_is_white(img: &image::DynamicImage) -> bool {
        let rgb = img.to_rgb8();
        rgb.get_pixel(0, 0)[0] > 128
    }

    fn fixture(name: &str) -> image::DynamicImage {
        decode_oriented_file(Path::new(&format!(
            "{}/../videre/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        )))
        .unwrap()
    }

    fn reencoded(img: &image::DynamicImage, w: u32, h: u32, quality: u8) -> image::DynamicImage {
        let resized = img.resize_exact(w, h, image::imageops::FilterType::Lanczos3);
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
            .encode_image(&resized.to_rgb8())
            .unwrap();
        decode_oriented_bytes(&jpeg).unwrap()
    }

    #[test]
    fn a_pixel_signature_is_taken_from_the_upright_canvas() {
        // The o6 fixture is the original tagged Orientation 6 (90 CW): its
        // signature is the rotated original's, as its fingerprint is.
        let rotated = image::DynamicImage::ImageRgb8(image::imageops::rotate90(
            &fixture("ai-generated-couple.jpg").to_rgb8(),
        ));
        let tagged = pixel_signature(&fixture("ai-generated-couple_o6.jpg"));
        assert!(signature_mad(&tagged.luma, &pixel_signature(&rotated).luma) < 1.0);
        assert!((tagged.aspect - rotated.width() as f32 / rotated.height() as f32).abs() < 1e-6);
    }

    #[test]
    fn a_resized_or_recompressed_copy_keeps_its_signature() {
        let original = fixture("ai-generated-couple.jpg");
        let sig = pixel_signature(&original);
        let (w, h) = (original.width(), original.height());
        let half = pixel_signature(&reencoded(&original, w / 2, h / 2, 90));
        let rough = pixel_signature(&reencoded(&original, w, h, 60));
        assert!(signature_mad(&sig.luma, &half.luma) < 2.0, "resize");
        assert!(signature_mad(&sig.luma, &rough.luma) < 3.0, "recompress");
        assert!((sig.aspect - half.aspect).abs() < 0.01);
    }

    #[test]
    fn a_different_picture_has_a_distant_signature() {
        let a = pixel_signature(&fixture("ai-generated-couple.jpg"));
        let b = pixel_signature(&fixture("sample_with_exif.jpg"));
        assert!(signature_mad(&a.luma, &b.luma) > 20.0);
    }

    #[test]
    fn aspect_is_width_over_height() {
        let img = image::DynamicImage::new_rgb8(1600, 1200);
        assert!((pixel_signature(&img).aspect - 4.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn dhash_of_a_flat_image_is_zero() {
        let flat = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            32,
            32,
            image::Rgb([90, 90, 90]),
        ));
        assert_eq!(dhash(&flat), 0);
    }

    #[test]
    fn untagged_file_passes_through_untouched() {
        let mut plain = Vec::new();
        let img = image::DynamicImage::new_rgb8(4, 2);
        img.write_to(
            &mut std::io::Cursor::new(&mut plain),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        let decoded = decode_oriented_bytes(&plain).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (4, 2));
    }

    #[test]
    fn orientation6_yields_the_display_canvas() {
        // Orientation 6 = rotate 90 CW to display: the 4x2 landscape becomes
        // 2x4 portrait and the top row (white) lands on the right column.
        let bytes = jpeg_with_orientation(6);
        let decoded = decode_oriented_bytes(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 4));
        let rgb = decoded.to_rgb8();
        assert!(
            rgb.get_pixel(1, 0)[0] > 128,
            "white row must move to the right column"
        );
    }

    #[test]
    fn file_and_bytes_variants_agree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o6.jpg");
        std::fs::write(&path, jpeg_with_orientation(6)).unwrap();
        let via_file = decode_oriented_file(&path).unwrap();
        let via_bytes = decode_oriented_bytes(&jpeg_with_orientation(6)).unwrap();
        assert_eq!(
            (via_file.width(), via_file.height()),
            (via_bytes.width(), via_bytes.height())
        );
        assert_eq!(top_left_is_white(&via_file), top_left_is_white(&via_bytes));
    }

    #[test]
    fn raw_variant_returns_the_tag_for_the_caller_to_apply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o6.jpg");
        std::fs::write(&path, jpeg_with_orientation(6)).unwrap();
        let (img, orientation) = decode_raw_with_orientation(&path).unwrap();
        assert_eq!(
            (img.width(), img.height()),
            (4, 2),
            "raw canvas is untouched"
        );
        assert!(matches!(
            orientation,
            image::metadata::Orientation::Rotate90
        ));
    }
}
