//! Camera RAW generation: TIFF-based files laid out the way Canon (`.CR2`), Nikon (`.NEF`),
//! Sony (`.ARW`) and phones writing DNG (`.dng`) lay them out.
//!
//! Every style carries a JPEG preview of a synthetic picture and a thumbnail; the sensor
//! data is random bytes, either stored plainly or wrapped as lossless JPEG (SOI, SOF3, SOS
//! ... EOI) like Canon's and DNG's compressed raw data. Directory layouts follow the
//! vendors: CR2 has its own header (`CR`, version 2.0, the raw IFD's offset) and four
//! IFDs (preview strip, thumbnail, small RGB, raw); NEF and ARW put the raw data (and
//! for NEF the preview) in SubIFDs; DNG has DNGVersion, a thumbnail in IFD0 and the raw
//! data in tiles in a SubIFD.
//!
//! Reference: Adobe, "TIFF Revision 6.0" section 2 (header, IFDs, field types), 7 (strips),
//! 15 (tiles); "TIFF Technical Note 1" (SubIFDs); Adobe, "Digital Negative (DNG)
//! Specification" 1.6; CIPA DC-008 "Exif 2.32" 4.6.5 (JPEGInterchangeFormat); ITU-T T.81
//! B.2 (lossless JPEG markers); ExifTool's tag tables for the CR2 header.

use anyhow::Result;

use super::{Generated, Meta, jpeg};
use crate::{rng::Rng, scenario::ContentKind};

const SHORT: u16 = 3;
const LONG: u16 = 4;
const ASCII: u16 = 2;
const UNDEFINED: u16 = 7;

/// A value or a pointer resolved when the file is laid out.
#[derive(Clone)]
enum Value {
    Shorts(Vec<u16>),
    Longs(Vec<u32>),
    Ascii(&'static str),
    Undefined(Vec<u8>),
    /// Offsets of blobs (strips, tiles, a JPEG).
    Blobs(Vec<usize>),
    /// Lengths of blobs.
    Lengths(Vec<usize>),
    /// Offsets of IFDs.
    Ifds(Vec<usize>),
}

impl Value {
    fn kind_and_count(&self) -> (u16, usize) {
        match self {
            Value::Shorts(v) => (SHORT, v.len()),
            Value::Longs(v) => (LONG, v.len()),
            Value::Ascii(s) => (ASCII, s.len() + 1),
            Value::Undefined(v) => (UNDEFINED, v.len()),
            Value::Blobs(v) | Value::Lengths(v) | Value::Ifds(v) => (LONG, v.len()),
        }
    }
}

struct Ifd {
    entries: Vec<(u16, Value)>,
    /// The next IFD in the chain (IFD0 -> IFD1 ...).
    next: Option<usize>,
}

fn at_of(offsets: &[usize], i: usize) -> usize {
    offsets.get(i).copied().unwrap_or(0)
}

/// Lays out a little-endian TIFF: header, the IFDs each followed by its out-of-line values,
/// then the blobs.
fn build(header: &[u8], ifds: &[Ifd], blobs: &[Vec<u8>]) -> Vec<u8> {
    let even = |x: usize| x + (x & 1);
    // Pass 1: where everything goes.
    let mut at = header.len();
    let mut ifd_at = Vec::new();
    for ifd in ifds {
        ifd_at.push(at);
        at += 2 + ifd.entries.len() * 12 + 4;
        for (_, v) in &ifd.entries {
            let (kind, count) = v.kind_and_count();
            let size = count
                * if kind == SHORT {
                    2
                } else if kind == LONG {
                    4
                } else {
                    1
                };
            if size > 4 {
                at = even(at + size);
            }
        }
    }
    let mut blob_at = Vec::new();
    for b in blobs {
        blob_at.push(at);
        at = even(at + b.len());
    }
    let as32 = |x: usize| u32::try_from(x).unwrap_or(u32::MAX);
    // Pass 2: write.
    let mut out = header.to_vec();
    for (i, ifd) in ifds.iter().enumerate() {
        let mut extra = Vec::new();
        let extra_at = at_of(&ifd_at, i) + 2 + ifd.entries.len() * 12 + 4;
        out.extend(u16::try_from(ifd.entries.len()).unwrap_or(0).to_le_bytes());
        let mut sorted = ifd.entries.clone();
        sorted.sort_by_key(|e| e.0);
        for (tag, v) in &sorted {
            let (kind, count) = v.kind_and_count();
            let data: Vec<u8> = match v {
                Value::Shorts(s) => s.iter().flat_map(|x| x.to_le_bytes()).collect(),
                Value::Longs(l) => l.iter().flat_map(|x| x.to_le_bytes()).collect(),
                Value::Ascii(s) => [s.as_bytes(), &[0]].concat(),
                Value::Undefined(u) => u.clone(),
                Value::Blobs(b) => b
                    .iter()
                    .flat_map(|&i| as32(at_of(&blob_at, i)).to_le_bytes())
                    .collect(),
                Value::Lengths(b) => b
                    .iter()
                    .flat_map(|&i| as32(blobs.get(i).map_or(0, Vec::len)).to_le_bytes())
                    .collect(),
                Value::Ifds(f) => f
                    .iter()
                    .flat_map(|&i| as32(at_of(&ifd_at, i)).to_le_bytes())
                    .collect(),
            };
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(as32(count).to_le_bytes());
            if data.len() <= 4 {
                let mut field = data.clone();
                field.resize(4, 0);
                out.extend(field);
            } else {
                out.extend(as32(extra_at + extra.len()).to_le_bytes());
                extra.extend(&data);
                if extra.len() % 2 == 1 {
                    extra.push(0);
                }
            }
        }
        out.extend(as32(ifd.next.map_or(0, |n| at_of(&ifd_at, n))).to_le_bytes());
        out.extend(extra);
    }
    for b in blobs {
        out.extend(b);
        if out.len() % 2 == 1 {
            out.push(0);
        }
    }
    out
}

/// Random bytes wrapped as a lossless JPEG (no `FF` inside, so no stuffing is needed).
fn lossless(len: usize, width: u16, height: u16, rng: &mut Rng) -> Vec<u8> {
    let mut j = vec![0xFF, 0xD8, 0xFF, 0xC3, 0x00, 0x0E, 14];
    j.extend(height.to_be_bytes());
    j.extend(width.to_be_bytes());
    j.extend([2, 1, 0x11, 0, 2, 0x11, 0]);
    j.extend([0xFF, 0xDA, 0x00, 0x0A, 2, 1, 0x00, 2, 0x00, 1, 0, 0]);
    let mut data = rng.bytes(len.saturating_sub(j.len() + 2));
    for b in &mut data {
        if *b == 0xFF {
            *b = 0xFE;
        }
    }
    j.extend(data);
    j.extend([0xFF, 0xD9]);
    j
}

/// An uncompressed RGB thumbnail strip of `w` x `h` pixels.
fn rgb(w: usize, h: usize, rng: &mut Rng) -> Vec<u8> {
    rng.bytes(w * h * 3)
}

pub fn generate(kind: ContentKind, target: u64, rng: &mut Rng) -> Result<Generated> {
    let raw_len = usize::try_from(target)?
        .saturating_sub(400 << 10)
        .max(64 << 10);
    let (pw, ph) = (1620u32, 1080u32);
    let preview = jpeg::encode_420(pw, ph, None, rng)?;
    let thumb = jpeg::encode_420(160, 120, None, rng)?;
    let exif = Ifd {
        entries: vec![
            (0x9000, Value::Undefined(b"0231".to_vec())),
            (0x829A, Value::Longs(vec![1, 250])),
            (
                0x8827,
                Value::Shorts(vec![u16::try_from(rng.range(100, 6400)).unwrap_or(100)]),
            ),
        ],
        next: None,
    };
    let (sw, sh) = (6000u32, 4000u32);
    let size = |w: u32, h: u32| vec![(256, Value::Longs(vec![w])), (257, Value::Longs(vec![h]))];
    let bytes = match kind {
        ContentKind::Cr2 => {
            // Blobs: preview, thumbnail, small RGB, raw.
            let blobs = vec![
                preview,
                thumb,
                rgb(592, 395, rng),
                lossless(raw_len, 1500, 4000, rng),
            ];
            let mut ifd0 = size(pw, ph);
            ifd0.extend([
                (259, Value::Shorts(vec![6])),
                (271, Value::Ascii("Canon")),
                (272, Value::Ascii("Canon EOS 5D Mark III")),
                (273, Value::Blobs(vec![0])),
                (279, Value::Lengths(vec![0])),
                (34665, Value::Ifds(vec![4])),
            ]);
            let ifd1 = vec![
                (259, Value::Shorts(vec![6])),
                (513, Value::Blobs(vec![1])),
                (514, Value::Lengths(vec![1])),
            ];
            let mut ifd2 = size(592, 395);
            ifd2.extend([
                (259, Value::Shorts(vec![1])),
                (273, Value::Blobs(vec![2])),
                (279, Value::Lengths(vec![2])),
            ]);
            let ifd3 = vec![
                (259, Value::Shorts(vec![6])),
                (273, Value::Blobs(vec![3])),
                (279, Value::Lengths(vec![3])),
            ];
            let ifds = [
                Ifd {
                    entries: ifd0,
                    next: Some(1),
                },
                Ifd {
                    entries: ifd1,
                    next: Some(2),
                },
                Ifd {
                    entries: ifd2,
                    next: Some(3),
                },
                Ifd {
                    entries: ifd3,
                    next: None,
                },
                exif,
            ];
            // The CR2 header: TIFF header with IFD0 at 16, "CR", version 2.0, raw IFD offset
            // (patched below once known).
            let mut head = b"II\x2A\x00\x10\x00\x00\x00CR\x02\x00".to_vec();
            head.extend(0u32.to_le_bytes());
            let mut out = build(&head, &ifds, &blobs);
            let raw_ifd = raw_ifd_offset(&out, 3).unwrap_or(0);
            if let Some(f) = out.get_mut(12..16) {
                f.copy_from_slice(&raw_ifd.to_le_bytes());
            }
            out
        }
        ContentKind::Nef | ContentKind::Arw => {
            let nikon = kind == ContentKind::Nef;
            // Sony stores 14-bit samples in 16-bit words; Nikon's are compressed.
            let (rw, rh) = (4000u32, u32::try_from(raw_len / 8000).unwrap_or(1).max(1));
            let raw = if nikon {
                rng.bytes(raw_len)
            } else {
                let mut words = rng.bytes(rw as usize * rh as usize * 2);
                for w in words.chunks_exact_mut(2) {
                    if let Some(high) = w.get_mut(1) {
                        *high &= 0x3F;
                    }
                }
                words
            };
            // Blobs: thumbnail RGB, preview JPEG, raw.
            let blobs = vec![rgb(160, 120, rng), preview, raw];
            let mut ifd0 = size(160, 120);
            ifd0.extend([
                (254, Value::Longs(vec![1])),
                (259, Value::Shorts(vec![1])),
                (
                    271,
                    Value::Ascii(if nikon { "NIKON CORPORATION" } else { "SONY" }),
                ),
                (
                    272,
                    Value::Ascii(if nikon { "NIKON D750" } else { "ILCE-7M3" }),
                ),
                (273, Value::Blobs(vec![0])),
                (279, Value::Lengths(vec![0])),
                (330, Value::Ifds(vec![1, 2])),
                (34665, Value::Ifds(vec![3])),
            ]);
            let sub_preview = vec![
                (254, Value::Longs(vec![1])),
                (259, Value::Shorts(vec![6])),
                (513, Value::Blobs(vec![1])),
                (514, Value::Lengths(vec![1])),
            ];
            let mut sub_raw = size(rw, rh);
            sub_raw.extend([
                (254, Value::Longs(vec![0])),
                (258, Value::Shorts(vec![if nikon { 12 } else { 14 }])),
                (259, Value::Shorts(vec![if nikon { 34713 } else { 1 }])),
                (273, Value::Blobs(vec![2])),
                (279, Value::Lengths(vec![2])),
            ]);
            let ifds = [
                Ifd {
                    entries: ifd0,
                    next: None,
                },
                Ifd {
                    entries: sub_preview,
                    next: None,
                },
                Ifd {
                    entries: sub_raw,
                    next: None,
                },
                exif,
            ];
            build(b"II\x2A\x00\x08\x00\x00\x00", &ifds, &blobs)
        }
        _ => {
            // DNG from a phone: thumbnail in IFD0, raw tiles (lossless JPEG) in a SubIFD.
            let tiles = 16usize;
            let mut blobs = vec![rgb(256, 192, rng)];
            for _ in 0..tiles {
                blobs.push(lossless(raw_len / tiles, 256, 256, rng));
            }
            let mut ifd0 = size(256, 192);
            ifd0.extend([
                (254, Value::Longs(vec![1])),
                (259, Value::Shorts(vec![1])),
                (271, Value::Ascii("Google")),
                (272, Value::Ascii("Pixel 4a")),
                (273, Value::Blobs(vec![0])),
                (279, Value::Lengths(vec![0])),
                (330, Value::Ifds(vec![1])),
                (34665, Value::Ifds(vec![2])),
                (0xC612, Value::Undefined(vec![1, 4, 0, 0])),
            ]);
            let mut raw = size(4032, 3024);
            raw.extend([
                (254, Value::Longs(vec![0])),
                (259, Value::Shorts(vec![7])),
                (322, Value::Longs(vec![256])),
                (323, Value::Longs(vec![256])),
                (324, Value::Blobs((1..=tiles).collect())),
                (325, Value::Lengths((1..=tiles).collect())),
            ]);
            let ifds = [
                Ifd {
                    entries: ifd0,
                    next: None,
                },
                Ifd {
                    entries: raw,
                    next: None,
                },
                exif,
            ];
            build(b"II\x2A\x00\x08\x00\x00\x00", &ifds, &blobs)
        }
    };
    Ok(Generated {
        bytes,
        meta: Meta {
            width: Some(sw),
            height: Some(sh),
            ..Meta::default()
        },
    })
}

/// Offset of the `n`th IFD in the IFD0 chain of a little-endian TIFF built here.
fn raw_ifd_offset(tiff: &[u8], n: usize) -> Option<u32> {
    let le32 = |at: usize| {
        tiff.get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
    };
    let mut at = le32(4)?;
    for _ in 0..n {
        let i = usize::try_from(at).ok()?;
        let count = usize::from(u16::from_le_bytes(tiff.get(i..i + 2)?.try_into().ok()?));
        at = le32(i + 2 + count * 12)?;
    }
    Some(at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_hold_together() {
        for kind in [
            ContentKind::Cr2,
            ContentKind::Nef,
            ContentKind::Arw,
            ContentKind::Dng,
        ] {
            let g = generate(kind, 2 << 20, &mut Rng::new(1)).unwrap();
            assert!(g.bytes.starts_with(b"II\x2A\x00"), "{kind:?}");
            assert!(g.bytes.len() > 1 << 20);
        }
    }
}
