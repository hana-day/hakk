//! Video (MP4) generation.
//!
//! Boxes are laid out `ftyp` -> `mdat` -> `moov` (at the end), like cameras write them.
//! Samples are length-prefixed NAL units filled with random bytes, so the structure is valid
//! but the video does not play. Meant for structural carving (walking boxes) and locating.
//!
//! Reference: ISO/IEC 14496-12 (ISO BMFF), ISO/IEC 14496-15 (AVC file format).

use super::{Generated, Meta};
use crate::rng::Rng;

const TIMESCALE: u32 = 30_000;
const SAMPLE_DELTA: u32 = 1_001; // 29.97fps
const WIDTH: u16 = 1920;
const HEIGHT: u16 = 1080;

pub fn generate(target: u64, rng: &mut Rng) -> Generated {
    // Pick sample sizes: a large I-frame every 30 samples, P-frames otherwise.
    let mut sizes = Vec::new();
    let mut total = 0u64;
    while total < target.saturating_sub(4096) || sizes.is_empty() {
        let key = sizes.len() % 30 == 0;
        let size = if key {
            rng.range(40_000, 80_000)
        } else {
            rng.range(4_000, 16_000)
        };
        sizes.push(u32::try_from(size).unwrap_or(u32::MAX));
        total += size;
    }

    let ftyp = bx(
        b"ftyp",
        &[b"mp42".as_slice(), &0u32.to_be_bytes(), b"isom", b"mp42"].concat(),
    );

    let mut mdat_payload = Vec::with_capacity(usize::try_from(total).unwrap_or(0));
    let mut offsets = Vec::with_capacity(sizes.len());
    let mdat_start = (ftyp.len() + 8) as u64;
    for (i, &size) in sizes.iter().enumerate() {
        offsets.push(mdat_start + mdat_payload.len() as u64);
        let nal_len = size - 4;
        mdat_payload.extend_from_slice(&nal_len.to_be_bytes());
        // NAL header: IDR (5) or non-IDR (1).
        mdat_payload.push(if i % 30 == 0 { 0x65 } else { 0x41 });
        mdat_payload.extend_from_slice(&rng.bytes(nal_len as usize - 1));
    }
    let mdat = bx(b"mdat", &mdat_payload);

    let samples = u32::try_from(sizes.len()).unwrap_or(u32::MAX);
    let duration = samples * SAMPLE_DELTA;
    let moov = moov(&sizes, &offsets, duration, rng);

    let mut out = Vec::with_capacity(ftyp.len() + mdat.len() + moov.len());
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&mdat);
    out.extend_from_slice(&moov);
    Generated {
        bytes: out,
        meta: Meta {
            width: Some(WIDTH.into()),
            height: Some(HEIGHT.into()),
            samples: Some(samples),
            ..Meta::default()
        },
    }
}

fn moov(sizes: &[u32], offsets: &[u64], duration: u32, rng: &mut Rng) -> Vec<u8> {
    // Creation time (seconds since 1904), roughly 2018 to 2025.
    let created = u32::try_from(rng.range(3_597_000_000, 3_850_000_000)).unwrap_or(0);
    let n = u32::try_from(sizes.len()).unwrap_or(u32::MAX);

    let mvhd = full(
        b"mvhd",
        0,
        0,
        &cat(&[
            &created.to_be_bytes(),
            &created.to_be_bytes(),
            &TIMESCALE.to_be_bytes(),
            &duration.to_be_bytes(),
            &0x0001_0000u32.to_be_bytes(), // rate 1.0
            &0x0100u16.to_be_bytes(),      // volume 1.0
            &[0; 10],
            &MATRIX,
            &[0; 24],
            &2u32.to_be_bytes(), // next_track_ID
        ]),
    );

    let tkhd = full(
        b"tkhd",
        0,
        3,
        &cat(&[
            &created.to_be_bytes(),
            &created.to_be_bytes(),
            &1u32.to_be_bytes(), // track_ID
            &[0; 4],
            &duration.to_be_bytes(),
            &[0; 8],
            &[0; 4], // layer, alternate_group
            &[0; 4], // volume, reserved
            &MATRIX,
            &(u32::from(WIDTH) << 16).to_be_bytes(),
            &(u32::from(HEIGHT) << 16).to_be_bytes(),
        ]),
    );

    let mdhd = full(
        b"mdhd",
        0,
        0,
        &cat(&[
            &created.to_be_bytes(),
            &created.to_be_bytes(),
            &TIMESCALE.to_be_bytes(),
            &duration.to_be_bytes(),
            &0x55C4u16.to_be_bytes(), // language "und"
            &[0; 2],
        ]),
    );
    let hdlr = full(
        b"hdlr",
        0,
        0,
        &cat(&[&[0; 4], b"vide", &[0; 12], b"VideoHandler\0"]),
    );

    let vmhd = full(b"vmhd", 0, 1, &[0; 8]);
    let dref = full(
        b"dref",
        0,
        0,
        &cat(&[&1u32.to_be_bytes(), &full(b"url ", 0, 1, &[])]),
    );
    let dinf = bx(b"dinf", &dref);

    let stsd = full(b"stsd", 0, 0, &cat(&[&1u32.to_be_bytes(), &avc1(rng)]));
    let stts = full(
        b"stts",
        0,
        0,
        &cat(&[
            &1u32.to_be_bytes(),
            &n.to_be_bytes(),
            &SAMPLE_DELTA.to_be_bytes(),
        ]),
    );
    // One sample per chunk.
    let stsc = full(
        b"stsc",
        0,
        0,
        &cat(&[
            &1u32.to_be_bytes(),
            &1u32.to_be_bytes(),
            &1u32.to_be_bytes(),
            &1u32.to_be_bytes(),
        ]),
    );
    let mut stsz = cat(&[&0u32.to_be_bytes(), &n.to_be_bytes()]);
    for s in sizes {
        stsz.extend_from_slice(&s.to_be_bytes());
    }
    let stsz = full(b"stsz", 0, 0, &stsz);
    let mut stco = n.to_be_bytes().to_vec();
    for &o in offsets {
        stco.extend_from_slice(&u32::try_from(o).unwrap_or(u32::MAX).to_be_bytes());
    }
    let stco = full(b"stco", 0, 0, &stco);
    let mut stss = Vec::new();
    let keys: Vec<u32> = (0..n).filter(|i| i % 30 == 0).map(|i| i + 1).collect();
    stss.extend_from_slice(&u32::try_from(keys.len()).unwrap_or(0).to_be_bytes());
    for k in keys {
        stss.extend_from_slice(&k.to_be_bytes());
    }
    let stss = full(b"stss", 0, 0, &stss);

    let stbl = bx(b"stbl", &cat(&[&stsd, &stts, &stss, &stsc, &stsz, &stco]));
    let minf = bx(b"minf", &cat(&[&vmhd, &dinf, &stbl]));
    let mdia = bx(b"mdia", &cat(&[&mdhd, &hdlr, &minf]));
    let trak = bx(b"trak", &cat(&[&tkhd, &mdia]));
    bx(b"moov", &cat(&[&mvhd, &trak]))
}

/// An `avc1` sample entry with `avcC`. SPS/PPS are placeholders.
fn avc1(rng: &mut Rng) -> Vec<u8> {
    let mut sps = vec![0x67, 0x64, 0x00, 0x28];
    sps.extend_from_slice(&rng.bytes(12));
    let pps = [0x68, 0xEE, 0x3C, 0x80];
    let avcc = bx(
        b"avcC",
        &cat(&[
            &[1, 0x64, 0x00, 0x28, 0xFF, 0xE1],
            &u16::try_from(sps.len()).unwrap_or(0).to_be_bytes(),
            &sps,
            &[1],
            &u16::try_from(pps.len()).unwrap_or(0).to_be_bytes(),
            &pps,
        ]),
    );
    let mut compressor = [0u8; 32];
    let name = b"hakk synthetic";
    compressor[0] = name.len() as u8;
    for (d, s) in compressor.iter_mut().skip(1).zip(name) {
        *d = *s;
    }
    bx(
        b"avc1",
        &cat(&[
            &[0; 6],
            &1u16.to_be_bytes(), // data_reference_index
            &[0; 16],
            &WIDTH.to_be_bytes(),
            &HEIGHT.to_be_bytes(),
            &0x0048_0000u32.to_be_bytes(),
            &0x0048_0000u32.to_be_bytes(),
            &[0; 4],
            &1u16.to_be_bytes(), // frame_count
            &compressor,
            &0x0018u16.to_be_bytes(),
            &(-1i16).to_be_bytes(),
            &avcc,
        ]),
    )
}

const MATRIX: [u8; 36] = {
    let mut m = [0u8; 36];
    // { 0x00010000,0,0, 0,0x00010000,0, 0,0,0x40000000 }
    m[1] = 1;
    m[17] = 1;
    m[32] = 0x40;
    m
};

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8 + payload.len()).unwrap_or(u32::MAX);
    cat(&[&size.to_be_bytes(), ty, payload])
}

fn full(ty: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let vf = (u32::from(version) << 24) | (flags & 0x00FF_FFFF);
    bx(ty, &cat(&[&vf.to_be_bytes(), payload]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walking the boxes, the top-level order and sizes are consistent.
    #[test]
    fn top_level_boxes_are_consistent() {
        let g = generate(500_000, &mut Rng::new(7));
        let mut pos = 0usize;
        let mut types = Vec::new();
        while pos < g.bytes.len() {
            let size = u32::from_be_bytes(g.bytes[pos..pos + 4].try_into().unwrap()) as usize;
            types.push(String::from_utf8_lossy(&g.bytes[pos + 4..pos + 8]).into_owned());
            assert!(size >= 8);
            pos += size;
        }
        assert_eq!(pos, g.bytes.len());
        assert_eq!(types, ["ftyp", "mdat", "moov"]);
        assert!(g.bytes.len() >= 500_000 - 4096);
    }
}
