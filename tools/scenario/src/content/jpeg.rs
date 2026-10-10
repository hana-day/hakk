//! 写真（JPEG）の生成。
//!
//! 乱数ノイズの画像を `image` クレートで JPEG に符号化し、SOI の直後に EXIF（APP1）と
//! 乱数入りのコメント（COM）を差し込む。ノイズの圧縮データは乱数同然なので、
//! 512 バイトごとの中身が一意になる。

use anyhow::Result;
use image::{ExtendedColorType, codecs::jpeg::JpegEncoder};

use super::{Generated, Meta, exif::Exif};
use crate::rng::Rng;

/// ノイズ画像を品質 90 で符号化したときの、1画素あたりのおおよそのバイト数。
const BYTES_PER_PIXEL: f64 = 1.6;

pub fn generate(target: u64, rng: &mut Rng) -> Result<Generated> {
    // 3:2 の画像で、おおよそ target バイトになる大きさ（16の倍数）
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

/// 乱数入りの COM セグメント。長さも乱数にして、後ろに続く量子化表などの位置をずらす。
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
        // SOI の直後が EXIF の APP1
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
        // 大きさはおおよそ目標どおり
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
