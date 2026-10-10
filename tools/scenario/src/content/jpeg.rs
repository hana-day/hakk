//! Photo (JPEG) generation.
//!
//! Encodes a random-noise image with the `image` crate and inserts EXIF (APP1) and a
//! random comment (COM) right after SOI. Compressed noise is effectively random, so each
//! 512-byte block is unique.
//!
//! Reference: ITU-T T.81 Annex B (SOI, APPn, COM segments).

use anyhow::Result;
use image::{ExtendedColorType, codecs::jpeg::JpegEncoder};

use super::{Generated, Meta, exif::Exif};
use crate::rng::Rng;

/// Approximate bytes per pixel when encoding noise at quality 90.
const BYTES_PER_PIXEL: f64 = 1.6;

pub fn generate(target: u64, rng: &mut Rng) -> Result<Generated> {
    // A 3:2 image of roughly `target` bytes (dimensions multiples of 16).
    let pixels = target as f64 / BYTES_PER_PIXEL;
    let height = ((pixels / 1.5).sqrt() as u32 / 16).max(1) * 16;
    let width = (height * 3 / 2 / 16).max(1) * 16;

    let mut pixels = vec![0u8; width as usize * height as usize * 3];
    rng.fill(&mut pixels);
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, 90).encode(
        &pixels,
        width,
        height,
        ExtendedColorType::Rgb8,
    )?;

    let exif = Exif::random(rng);
    let mut out = Vec::with_capacity(encoded.len() + 2048);
    out.extend_from_slice(&[0xFF, 0xD8]);
    out.extend_from_slice(&exif.app1());
    out.extend_from_slice(&comment(rng));
    out.extend_from_slice(encoded.get(2..).unwrap_or_default());

    Ok(Generated {
        bytes: out,
        meta: Meta {
            exif: Some(exif),
            width: Some(width),
            height: Some(height),
            ..Meta::default()
        },
    })
}

/// A random COM segment. Its length is random too, shifting the tables that follow.
fn comment(rng: &mut Rng) -> Vec<u8> {
    let len = rng.range(600, 1500) as usize;
    let mut seg = vec![0xFF, 0xFE];
    seg.extend_from_slice(&u16::try_from(len + 2).unwrap_or(u16::MAX).to_be_bytes());
    seg.extend_from_slice(&rng.bytes(len));
    seg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_has_exif() {
        let g = generate(300_000, &mut Rng::new(11)).unwrap();
        let img = image::load_from_memory_with_format(&g.bytes, image::ImageFormat::Jpeg).unwrap();
        assert_eq!(Some(img.width()), g.meta.width);
        assert_eq!(Some(img.height()), g.meta.height);
        // EXIF APP1 right after SOI.
        assert_eq!(&g.bytes[..4], &[0xFF, 0xD8, 0xFF, 0xE1]);
        assert_eq!(&g.bytes[6..12], b"Exif\0\0");
        let exif = g.meta.exif.unwrap();
        let tiff = &g.bytes[12..];
        assert!(
            tiff.windows(exif.model.len())
                .any(|w| w == exif.model.as_bytes())
        );
        assert!(
            tiff.windows(19)
                .any(|w| w == exif.date_time_original.as_bytes())
        );
        // The size is roughly on target.
        let ratio = g.bytes.len() as f64 / 300_000.0;
        assert!((0.6..1.6).contains(&ratio), "{ratio}");
    }

    #[test]
    fn deterministic() {
        let a = generate(100_000, &mut Rng::new(5)).unwrap();
        let b = generate(100_000, &mut Rng::new(5)).unwrap();
        let c = generate(100_000, &mut Rng::new(6)).unwrap();
        assert_eq!(a.bytes, b.bytes);
        assert_ne!(a.bytes, c.bytes);
    }
}
