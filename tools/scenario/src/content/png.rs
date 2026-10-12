//! Screenshot and edited-image (`.PNG`) generation.
//!
//! Encodes a synthetic picture with the `image` crate, then rebuilds the file the way a
//! given writer lays it out, picked per file: an iPhone screenshot (`sRGB`, `eXIf`, Apple's
//! `iDOT`, 16 KiB `IDAT` chunks), an Android screenshot (8 KiB `IDAT`s, nothing else), a
//! Windows capture (`sRGB`, `gAMA`, `pHYs`, 64 KiB `IDAT`s) or an image saved by an editor
//! (`tEXt` Software, `pHYs`, 8 KiB `IDAT`s). The compressed data is the encoder's, only
//! split differently, so the file stays valid.
//!
//! Reference: W3C, "Portable Network Graphics (PNG) Specification (Third Edition)" 5.3
//! (chunk layout), 11.2 (IHDR, IDAT, IEND), 11.3 (sRGB, gAMA, pHYs, tEXt, eXIf).

use anyhow::{Result, bail};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

use super::{Generated, Meta, picture::picture};
use crate::rng::Rng;

struct Style {
    idat: usize,
    ancillary: fn(u32, u32, &mut Rng) -> Vec<Vec<u8>>,
}

const STYLES: [Style; 4] = [
    Style {
        idat: 16 << 10,
        ancillary: iphone,
    },
    Style {
        idat: 8 << 10,
        ancillary: |_, _, _| Vec::new(),
    },
    Style {
        idat: 64 << 10,
        ancillary: windows,
    },
    Style {
        idat: 8 << 10,
        ancillary: editor,
    },
];

pub fn generate(target: u64, rng: &mut Rng) -> Result<Generated> {
    let style = STYLES.get(rng.index(STYLES.len())).unwrap_or(&STYLES[1]);
    // Synthetic pictures come out at about this many bytes per pixel.
    let pixels = (target as f64 / 1.2).max(64.0 * 64.0);
    let width = ((pixels * 0.75).sqrt() as u32).max(64);
    let height = ((pixels / f64::from(width)) as u32).max(64);
    let rgb = picture(width, height, rng);
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded).write_image(&rgb, width, height, ExtendedColorType::Rgb8)?;

    let (ihdr, data) = split(&encoded)?;
    let mut out = encoded.get(..8).unwrap_or_default().to_vec();
    out.extend(chunk(b"IHDR", &ihdr));
    for c in (style.ancillary)(width, height, rng) {
        out.extend(c);
    }
    for part in data.chunks(style.idat) {
        out.extend(chunk(b"IDAT", part));
    }
    out.extend(chunk(b"IEND", &[]));
    Ok(Generated {
        bytes: out,
        meta: Meta {
            width: Some(width),
            height: Some(height),
            ..Meta::default()
        },
    })
}

/// The IHDR data and all IDAT data of an encoded PNG.
fn split(png: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut ihdr = None;
    let mut data = Vec::new();
    let mut at = 8;
    while let Some(head) = png.get(at..at + 8) {
        let len = head
            .get(..4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map_or(0, |b| u32::from_be_bytes(b) as usize);
        let body = png.get(at + 8..at + 8 + len).unwrap_or_default();
        match head.get(4..8) {
            Some(b"IHDR") => ihdr = Some(body.to_vec()),
            Some(b"IDAT") => data.extend(body),
            _ => {}
        }
        at += 12 + len;
    }
    match ihdr {
        Some(h) => Ok((h, data)),
        None => bail!("encoded PNG has no IHDR"),
    }
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = u32::try_from(data.len())
        .unwrap_or(u32::MAX)
        .to_be_bytes()
        .to_vec();
    v.extend(kind);
    v.extend(data);
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(data);
    v.extend(h.finalize().to_be_bytes());
    v
}

/// sRGB, a small Exif block (orientation and pixel size) and Apple's `iDOT` (where the
/// image data splits for parallel decoding).
fn iphone(width: u32, height: u32, rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut exif = b"MM\x00\x2A\x00\x00\x00\x08\x00\x03".to_vec();
    for (tag, value) in [(0x0112u16, 1u32), (0xA002, width), (0xA003, height)] {
        exif.extend(tag.to_be_bytes());
        exif.extend(4u16.to_be_bytes());
        exif.extend(1u32.to_be_bytes());
        exif.extend(value.to_be_bytes());
    }
    exif.extend(0u32.to_be_bytes());
    let mut idot = Vec::new();
    for v in [2u32, 0, height / 2, 40, height / 2, height - height / 2] {
        idot.extend(v.to_be_bytes());
    }
    idot.extend(
        u32::try_from(rng.range(0x1000, 0x8000))
            .unwrap_or(0)
            .to_be_bytes(),
    );
    vec![
        chunk(b"sRGB", &[0]),
        chunk(b"eXIf", &exif),
        chunk(b"iDOT", &idot),
    ]
}

/// sRGB, gamma 1/2.2 and 96 dpi, as Windows' capture tools write them.
fn windows(_: u32, _: u32, _: &mut Rng) -> Vec<Vec<u8>> {
    let mut phys = Vec::new();
    phys.extend(3780u32.to_be_bytes());
    phys.extend(3780u32.to_be_bytes());
    phys.push(1);
    vec![
        chunk(b"sRGB", &[0]),
        chunk(b"gAMA", &45455u32.to_be_bytes()),
        chunk(b"pHYs", &phys),
    ]
}

/// The editor's name and 72 dpi.
fn editor(_: u32, _: u32, _: &mut Rng) -> Vec<Vec<u8>> {
    let mut phys = Vec::new();
    phys.extend(2835u32.to_be_bytes());
    phys.extend(2835u32.to_be_bytes());
    phys.push(1);
    vec![
        chunk(b"tEXt", b"Software\x00GIMP 2.10.36"),
        chunk(b"pHYs", &phys),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_ends_with_iend() {
        for seed in 0..4 {
            let g = generate(200_000, &mut Rng::new(seed)).unwrap();
            assert!(
                g.bytes
                    .ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82])
            );
            let img = image::load_from_memory(&g.bytes).unwrap();
            assert_eq!(Some(img.width()), g.meta.width);
        }
    }
}
