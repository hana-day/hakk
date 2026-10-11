//! Photo (JPEG) generation.
//!
//! Encodes a random-noise image with the `image` crate and inserts EXIF (APP1) and a
//! random comment (COM) right after SOI. Compressed noise is effectively random, so each
//! 512-byte block is unique.
//!
//! Reference: ITU-T T.81 Annex B (SOI, APPn, COM segments).

use anyhow::Result;
use image::{ExtendedColorType, codecs::jpeg::JpegEncoder};

use super::{
    Generated, Meta,
    exif::Exif,
    mp4,
    profile::{JpegProfile, Profiles, Segment},
};
use crate::rng::Rng;

/// Approximate bytes per pixel when encoding a synthetic picture at quality 90.
const BYTES_PER_PIXEL: f64 = 0.2;

pub fn generate(target: u64, rng: &mut Rng) -> Result<Generated> {
    // A 3:2 image of roughly `target` bytes (dimensions multiples of 16).
    let pixels = target as f64 / BYTES_PER_PIXEL;
    let height = ((pixels / 1.5).sqrt() as u32 / 16).max(1) * 16;
    let width = (height * 3 / 2 / 16).max(1) * 16;

    let pixels = super::picture::picture(width, height, rng);
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
    segment(0xFE, &no_markers(rng.bytes(len)))
}

/// A JPEG following a device profile: header segments in the device's order, an EXIF
/// thumbnail if the device embeds one, and trailing bytes after EOI if it appends any.
pub fn generate_profiled(
    p: &JpegProfile,
    profiles: &Profiles,
    target: u64,
    rng: &mut Rng,
) -> Result<Generated> {
    let [aw, ah] = p.aspect;
    let pixels = target as f64 / BYTES_PER_PIXEL;
    let height = ((pixels * f64::from(ah) / f64::from(aw)).sqrt() as u32 / 16).max(1) * 16;
    let width = (height * aw / ah / 16).max(1) * 16;
    // 4:2:0 subsampling (16x16 MCUs), as phones and cameras use.
    let restart = p.restart_rows.map(|rows| {
        u16::try_from(width.div_ceil(16))
            .unwrap_or(u16::MAX)
            .saturating_mul(rows)
    });
    let main = Parts::parse(&encode_420(width, height, restart, rng)?)?;

    let thumbnail = match p.thumbnail {
        Some([lo, hi]) => Some(thumbnail(rng.range(lo, hi), rng)?),
        None => None,
    };
    let exif = Exif::for_camera(&p.make, &p.model, rng);
    let exif_len = rng.range(p.exif_len[0], p.exif_len[1]) as usize;

    // Parts after the image, listed in the XMP container directory.
    let mut appended: Vec<(&str, &str, Vec<u8>)> = Vec::new();
    if let Some([lo, hi]) = p.gain_map {
        appended.push(("image/jpeg", "GainMap", noise_jpeg(rng.range(lo, hi), rng)?));
    }
    if let Some(m) = &p.motion
        && let Some(vp) = profiles.video.get(&m.profile)
    {
        let size = rng.range(m.size[0], m.size[1]);
        let video = mp4::generate_profiled(vp, size, &mut rng.fork()).bytes;
        appended.push(("video/mp4", "MotionPhoto", video));
    }

    let mut out = vec![0xFF, 0xD8];
    for s in &p.segments {
        match s {
            Segment::Exif => out.extend(exif.app1_with(thumbnail.as_deref(), exif_len, rng)),
            Segment::Jfif => out.extend(segment(0xE0, &JFIF)),
            Segment::App { n, len } => {
                let len = rng.range(len[0], len[1]) as usize;
                out.extend(segment(0xE0 + (n & 0x0F), &no_markers(rng.bytes(len))));
            }
            Segment::Dqt => out.extend(segment(0xDB, &main.dqt.concat())),
            Segment::DqtEach => main.dqt.iter().for_each(|t| out.extend(segment(0xDB, t))),
            Segment::Dht => out.extend(segment(0xC4, &main.dht.concat())),
            Segment::DhtEach => main.dht.iter().for_each(|t| out.extend(segment(0xC4, t))),
            Segment::Sof => out.extend(segment(0xC0, &main.sof)),
            Segment::Dri => out.extend(segment(0xDD, &main.dri)),
            Segment::Xmp => out.extend(segment(0xE1, &xmp(&appended, p.motion.is_some()))),
        }
    }
    out.extend_from_slice(&main.scan);
    for (_, _, part) in &appended {
        out.extend_from_slice(part);
    }
    if let Some([lo, hi]) = p.trailer {
        // Phones pad files with zeros (Samsung, LG, Wiko).
        out.extend(vec![0u8; rng.range(lo, hi) as usize]);
    }

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

/// An XMP packet (APP1 payload) with a container directory listing `appended` after the
/// primary image, as Ultra HDR and motion photos do.
///
/// Reference: Android "Ultra HDR image format" and "Motion photo format" (XMP
/// `Container:Directory`, `Item:Mime`, `Item:Semantic`, `Item:Length`, `Item:Padding`).
fn xmp(appended: &[(&str, &str, Vec<u8>)], motion: bool) -> Vec<u8> {
    let mut x = String::from("http://ns.adobe.com/xap/1.0/\0");
    x.push_str(r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#);
    x.push_str(r#"<rdf:Description xmlns:Container="http://ns.google.com/photos/1.0/container/" xmlns:Item="http://ns.google.com/photos/1.0/container/item/" xmlns:GCamera="http://ns.google.com/photos/1.0/camera/""#);
    if motion {
        x.push_str(r#" GCamera:MotionPhoto="1" GCamera:MotionPhotoVersion="1""#);
    }
    x.push('>');
    if !appended.is_empty() {
        x.push_str("<Container:Directory><rdf:Seq>");
        x.push_str(r#"<rdf:li rdf:parseType="Resource"><Container:Item Item:Mime="image/jpeg" Item:Semantic="Primary" Item:Length="0" Item:Padding="0"/></rdf:li>"#);
        for (mime, semantic, part) in appended {
            x.push_str(&format!(
                r#"<rdf:li rdf:parseType="Resource"><Container:Item Item:Mime="{mime}" Item:Semantic="{semantic}" Item:Length="{}" Item:Padding="0"/></rdf:li>"#,
                part.len()
            ));
        }
        x.push_str("</rdf:Seq></Container:Directory>");
    }
    x.push_str("</rdf:Description></rdf:RDF></x:xmpmeta>");
    x.into_bytes()
}

/// A noise JPEG of roughly `target` bytes (for gain maps).
fn noise_jpeg(target: u64, rng: &mut Rng) -> Result<Vec<u8>> {
    // Noise at quality 90 is about 1.6 bytes per pixel.
    let side = ((target as f64 / 1.6).sqrt() as u32 / 16).max(1) * 16;
    encode_noise(side, side, 90, rng)
}

/// Removes `FF D8` and `FF D9` from random filler. Real vendor data rarely contains them,
/// and stray ones would make the files harder to carve than the real thing.
pub fn no_markers(mut bytes: Vec<u8>) -> Vec<u8> {
    for i in 1..bytes.len() {
        if bytes.get(i - 1) == Some(&0xFF)
            && let Some(b) = bytes.get_mut(i)
            && (*b == 0xD8 || *b == 0xD9)
        {
            *b = 0x00;
        }
    }
    bytes
}

/// JFIF 1.01, aspect ratio 1:1.
const JFIF: [u8; 14] = [b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0];

fn encode_noise(width: u32, height: u32, quality: u8, rng: &mut Rng) -> Result<Vec<u8>> {
    let mut pixels = vec![0u8; width as usize * height as usize * 3];
    rng.fill(&mut pixels);
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, quality).encode(
        &pixels,
        width,
        height,
        ExtendedColorType::Rgb8,
    )?;
    Ok(encoded)
}

/// Encodes noise with 4:2:0 subsampling and, if given, a restart interval (in MCUs).
fn encode_420(width: u32, height: u32, restart: Option<u16>, rng: &mut Rng) -> Result<Vec<u8>> {
    let pixels = super::picture::picture(width, height, rng);
    let mut encoded = Vec::new();
    let mut enc = jpeg_encoder::Encoder::new(&mut encoded, 90);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
    if let Some(n) = restart {
        enc.set_restart_interval(n);
    }
    enc.encode(
        &pixels,
        u16::try_from(width)?,
        u16::try_from(height)?,
        jpeg_encoder::ColorType::Rgb,
    )?;
    Ok(encoded)
}

/// A 160x120 JPEG thumbnail of about `target` bytes (as close as the quality steps allow).
fn thumbnail(target: u64, rng: &mut Rng) -> Result<Vec<u8>> {
    let mut best = Vec::new();
    for quality in [90, 75, 60, 45, 30, 20, 10] {
        best = encode_noise(160, 120, quality, &mut rng.fork())?;
        if best.len() as u64 <= target {
            break;
        }
    }
    Ok(best)
}

/// An encoded JPEG split into its tables, frame header and scan.
struct Parts {
    /// One entry per quantization table (precision/id byte + values).
    dqt: Vec<Vec<u8>>,
    /// One entry per Huffman table (class/id byte + counts + symbols).
    dht: Vec<Vec<u8>>,
    /// SOF0 payload.
    sof: Vec<u8>,
    /// DRI payload, if the encoder wrote one.
    dri: Vec<u8>,
    /// From the SOS marker through EOI.
    scan: Vec<u8>,
}

impl Parts {
    /// Reference: ITU-T T.81 B.2.4.1 (DQT), B.2.4.2 (DHT), B.2.4.4 (DRI), B.2.2 (SOF),
    /// B.2.3 (SOS).
    fn parse(jpeg: &[u8]) -> Result<Self> {
        let (mut dqt, mut dht, mut sof, mut dri) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut i = 2;
        loop {
            let (Some(&0xFF), Some(&marker)) = (jpeg.get(i), jpeg.get(i + 1)) else {
                anyhow::bail!("unexpected bytes at {i} in the encoder output");
            };
            if marker == 0xDA {
                let scan = jpeg.get(i..).unwrap_or_default().to_vec();
                return Ok(Self {
                    dqt,
                    dht,
                    sof,
                    dri,
                    scan,
                });
            }
            let len = jpeg
                .get(i + 2..i + 4)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map(|b| usize::from(u16::from_be_bytes(b)))
                .unwrap_or(0);
            let body = jpeg.get(i + 4..i + 2 + len).unwrap_or_default();
            match marker {
                0xDB => dqt.extend(split(body, |t| {
                    1 + 64 * (1 + usize::from(t.first().copied().unwrap_or(0) >> 4))
                })),
                0xC4 => dht.extend(split(body, |t| {
                    1 + 16
                        + t.get(1..17)
                            .unwrap_or_default()
                            .iter()
                            .map(|&c| usize::from(c))
                            .sum::<usize>()
                })),
                0xC0 => sof = body.to_vec(),
                0xDD => dri = body.to_vec(),
                _ => {} // APP0 from the encoder is dropped; the profile decides.
            }
            i += 2 + len;
        }
    }
}

/// Splits a segment payload holding several tables, given each table's length.
fn split(mut body: &[u8], len: impl Fn(&[u8]) -> usize) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while !body.is_empty() {
        let n = len(body).min(body.len());
        let (t, rest) = body.split_at(n);
        out.push(t.to_vec());
        body = rest;
    }
    out
}

/// A marker segment: `FF marker`, a 16-bit length, then `payload`.
fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut seg = vec![0xFF, marker];
    seg.extend_from_slice(
        &u16::try_from(payload.len() + 2)
            .unwrap_or(u16::MAX)
            .to_be_bytes(),
    );
    seg.extend_from_slice(payload);
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
