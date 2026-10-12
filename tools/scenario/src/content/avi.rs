//! Digital-camera video (`.AVI`) generation: Motion JPEG with PCM audio in a RIFF AVI file,
//! as 2000s compact cameras wrote it.
//!
//! Two styles, picked per file: Casio-like (Motion JPEG frames without DHT, relying on the
//! standard tables; 8-bit 8 kHz audio; `LIST INFO` with the camera name) and Nikon-like
//! (frames with DHT; 16-bit 22.05 kHz audio; a `LIST ncdt` holding a JPEG thumbnail;
//! `JUNK` so the media start on a 2048-byte boundary). Both end with an `idx1` index whose
//! offsets count from the `movi` type field. Frames are real JPEGs of synthetic pictures
//! (so a carver can decode them); audio is a noisy tone.
//!
//! Reference: Microsoft, "AVI RIFF File Reference" (AVIMAINHEADER, AVISTREAMHEADER,
//! BITMAPINFOHEADER, WAVEFORMATEX, AVIOLDINDEX); RFC 2435 / the AVI1 Motion JPEG
//! convention (frames without DHT use the tables of ITU-T T.81 K.3).

use anyhow::Result;

use super::{Generated, Meta, jpeg};
use crate::rng::Rng;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
/// AVIF_HASINDEX | AVIF_ISINTERLEAVED.
const AVIF_FLAGS: u32 = 0x10 | 0x100;
/// AVIIF_KEYFRAME.
const KEYFRAME: u32 = 0x10;

struct Style {
    fps: u32,
    sample_rate: u32,
    bits: u16,
    /// Keep DHT in the frames.
    dht: bool,
    nikon: bool,
}

pub fn generate(target: u64, rng: &mut Rng) -> Result<Generated> {
    let style = if rng.index(2) == 0 {
        Style {
            fps: 15,
            sample_rate: 8000,
            bits: 8,
            dht: false,
            nikon: false,
        }
    } else {
        Style {
            fps: 30,
            sample_rate: 22050,
            bits: 16,
            dht: true,
            nikon: true,
        }
    };
    let block_align = u32::from(style.bits / 8);
    let audio_per_sec = style.sample_rate * block_align;

    // Media: a frame per tick, an audio block every second (before that second's frames).
    let mut movi = b"movi".to_vec();
    let mut index = Vec::new();
    let mut frames = 0u32;
    let mut audio_bytes = 0u32;
    let mut biggest = 0u32;
    while (movi.len() as u64) < target {
        if frames.is_multiple_of(style.fps) {
            let block = sound(&style, frames / style.fps, rng);
            push(&mut movi, &mut index, b"01wb", &block, 0);
            audio_bytes += audio_per_sec;
        }
        let frame = frame(&style, rng)?;
        biggest = biggest.max(u32::try_from(frame.len())?);
        push(&mut movi, &mut index, b"00dc", &frame, KEYFRAME);
        frames += 1;
    }

    let mut avih = Vec::new();
    for v in [
        1_000_000 / style.fps,
        biggest * style.fps + audio_per_sec,
        0,
        AVIF_FLAGS,
        frames,
        0,
        2,
        biggest,
        WIDTH,
        HEIGHT,
        0,
        0,
        0,
        0,
    ] {
        avih.extend(v.to_le_bytes());
    }
    let video = list(
        b"strl",
        &[
            chunk(
                b"strh",
                &strh(b"vids", b"MJPG", 1, style.fps, frames, biggest, 0),
            ),
            chunk(b"strf", &bitmap_info()),
        ]
        .concat(),
    );
    let audio = list(
        b"strl",
        &[
            chunk(
                b"strh",
                &strh(
                    b"auds",
                    &[0; 4],
                    block_align,
                    style.sample_rate * block_align,
                    audio_bytes / block_align,
                    audio_per_sec,
                    block_align,
                ),
            ),
            chunk(b"strf", &wave_format(style.sample_rate, style.bits)),
        ]
        .concat(),
    );
    let hdrl = list(b"hdrl", &[chunk(b"avih", &avih), video, audio].concat());
    let mut body = b"AVI ".to_vec();
    body.extend(hdrl);
    if style.nikon {
        let thumb = jpeg::encode_420(160, 120, None, rng)?;
        let ncdt = list(
            b"ncdt",
            &[chunk(b"ncvr", &rng.bytes(18)), chunk(b"ncth", &thumb)].concat(),
        );
        body.extend(ncdt);
        // JUNK so that the media data starts on a 2048-byte boundary.
        let at = 8 + body.len() + 8 + 12;
        let pad = (2048 - at % 2048) % 2048;
        body.extend(chunk(b"JUNK", &vec![0; pad]));
    } else {
        let mut name = b"CASIO EX-S3".to_vec();
        name.resize(40, b' ');
        body.extend(list(b"INFO", &chunk(b"ISFT", &name)));
    }
    body.extend(chunk(b"LIST", &movi));
    body.extend(chunk(b"idx1", &index));
    Ok(Generated {
        bytes: chunk(b"RIFF", &body),
        meta: Meta {
            width: Some(WIDTH),
            height: Some(HEIGHT),
            samples: Some(frames),
            ..Meta::default()
        },
    })
}

/// One second of audio: a tone that changes pitch each second, with a little noise (real
/// recordings run on smoothly from sample to sample; the noise keeps blocks unique).
fn sound(style: &Style, second: u32, rng: &mut Rng) -> Vec<u8> {
    let pitch = 200.0 + rng.range(0, 600) as f64;
    let rate = f64::from(style.sample_rate);
    let mut out = Vec::new();
    for i in 0..style.sample_rate {
        let t = f64::from(second) + f64::from(i) / rate;
        let wave = (t * pitch * std::f64::consts::TAU).sin() * 0.4;
        let noise = rng.range(0, 4) as f64 - 2.0;
        if style.bits == 8 {
            out.push((128.0 + wave * 127.0 + noise).clamp(0.0, 255.0) as u8);
        } else {
            let v = (wave * 32767.0 + noise * 64.0).clamp(-32768.0, 32767.0) as i16;
            out.extend(v.to_le_bytes());
        }
    }
    out
}

/// A Motion JPEG frame; without DHT for the Casio style.
fn frame(style: &Style, rng: &mut Rng) -> Result<Vec<u8>> {
    let encoded = jpeg::encode_420(WIDTH, HEIGHT, None, rng)?;
    if style.dht {
        return Ok(encoded);
    }
    // The encoder writes the T.81 K.3 tables, which are what AVI1 frames leave out.
    let mut out = Vec::with_capacity(encoded.len());
    out.extend(encoded.get(..2).unwrap_or_default());
    let mut at = 2;
    while let (Some(&0xFF), Some(&marker)) = (encoded.get(at), encoded.get(at + 1)) {
        let len = encoded
            .get(at + 2..at + 4)
            .and_then(|b| <[u8; 2]>::try_from(b).ok())
            .map_or(0, |b| usize::from(u16::from_be_bytes(b)));
        if marker == 0xDA {
            break;
        }
        if marker != 0xC4 {
            out.extend(encoded.get(at..at + 2 + len).unwrap_or_default());
        }
        at += 2 + len;
    }
    out.extend(encoded.get(at..).unwrap_or_default());
    Ok(out)
}

/// Appends a media chunk to `movi` and its `idx1` entry.
fn push(movi: &mut Vec<u8>, index: &mut Vec<u8>, id: &[u8; 4], data: &[u8], flags: u32) {
    // Offsets count from the `movi` type field, which is where `movi` starts here.
    let offset = u32::try_from(movi.len()).unwrap_or(u32::MAX);
    let size = u32::try_from(data.len()).unwrap_or(u32::MAX);
    index.extend(id);
    index.extend(flags.to_le_bytes());
    index.extend(offset.to_le_bytes());
    index.extend(size.to_le_bytes());
    movi.extend(chunk(id, data));
}

#[allow(clippy::too_many_arguments)]
fn strh(
    kind: &[u8; 4],
    handler: &[u8; 4],
    scale: u32,
    rate: u32,
    length: u32,
    buffer: u32,
    sample_size: u32,
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(kind);
    v.extend(handler);
    for x in [
        0u32,
        0,
        0,
        scale,
        rate,
        0,
        length,
        buffer,
        u32::MAX,
        sample_size,
    ] {
        v.extend(x.to_le_bytes());
    }
    for x in [0u16, 0, 0, 0] {
        v.extend(x.to_le_bytes());
    }
    v
}

fn bitmap_info() -> Vec<u8> {
    let mut v = Vec::new();
    for x in [40u32, WIDTH, HEIGHT] {
        v.extend(x.to_le_bytes());
    }
    v.extend(1u16.to_le_bytes());
    v.extend(24u16.to_le_bytes());
    v.extend(b"MJPG");
    for x in [WIDTH * HEIGHT * 3, 0, 0, 0, 0] {
        v.extend(x.to_le_bytes());
    }
    v
}

fn wave_format(rate: u32, bits: u16) -> Vec<u8> {
    let align = bits / 8;
    let mut v = Vec::new();
    v.extend(1u16.to_le_bytes());
    v.extend(1u16.to_le_bytes());
    v.extend(rate.to_le_bytes());
    v.extend((rate * u32::from(align)).to_le_bytes());
    v.extend(align.to_le_bytes());
    v.extend(bits.to_le_bytes());
    v
}

fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend(u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    v.extend(data);
    if data.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    chunk(b"LIST", &[&kind[..], body].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_add_up() {
        for seed in 0..4 {
            let g = generate(300_000, &mut Rng::new(seed)).unwrap();
            let b = &g.bytes;
            assert_eq!(&b[..4], b"RIFF");
            assert_eq!(&b[8..12], b"AVI ");
            let size = u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize;
            assert_eq!(size + 8, b.len());
        }
    }
}
