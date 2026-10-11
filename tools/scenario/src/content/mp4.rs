//! Video (MP4 / QuickTime) generation.
//!
//! Samples are length-prefixed NAL units (video) and AAC-sized frames (audio) filled with
//! random bytes, so the structure is valid but nothing plays. A [`VideoProfile`] decides
//! the box layout: where `moov` goes, how audio and video chunks interleave in `mdat`,
//! 32- or 64-bit chunk offsets, metadata tracks, and boxes after `mdat`. Without a
//! profile, files are `ftyp mdat moov` with a single video track.
//!
//! Reference: ISO/IEC 14496-12 (ISO BMFF), ISO/IEC 14496-14 (MP4, `esds`),
//! ISO/IEC 14496-15 (AVC file format), Apple "QuickTime File Format" (`wide`, `mebx`).

use super::{
    Generated, Meta,
    profile::{Layout, VideoProfile},
};
use crate::rng::Rng;

const VIDEO_TIMESCALE: u32 = 30_000;
const VIDEO_DELTA: u32 = 1_001; // 29.97 fps
const AUDIO_RATE: u32 = 44_100;
const AUDIO_DELTA: u32 = 1_024; // samples per AAC frame
const WIDTH: u16 = 1920;
const HEIGHT: u16 = 1080;

/// The layout used when no profile is given.
fn plain() -> VideoProfile {
    VideoProfile {
        dir: String::new(),
        name: String::new(),
        brand: "mp42".to_owned(),
        compatible: vec!["isom".to_owned(), "mp42".to_owned()],
        layout: Layout::MoovLast,
        wide: false,
        reserve: None,
        video_per_chunk: 1,
        audio_per_chunk: None,
        co64: false,
        metadata_tracks: 0,
        trailing_uuid: Vec::new(),
    }
}

pub fn generate(target: u64, rng: &mut Rng) -> Generated {
    generate_profiled(&plain(), target, rng)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Video,
    Audio,
    Metadata,
}

/// One track's samples and where its chunks ended up in `mdat`.
struct Track {
    kind: Kind,
    sizes: Vec<u32>,
    per_chunk: u32,
    /// Chunk offsets relative to the start of the `mdat` payload.
    chunks: Vec<u64>,
}

impl Track {
    fn chunk_count(&self) -> usize {
        self.sizes.len().div_ceil(self.per_chunk as usize)
    }
}

pub fn generate_profiled(p: &VideoProfile, target: u64, rng: &mut Rng) -> Generated {
    // Video: a large I-frame every 30 samples, P-frames otherwise.
    let mut video = Vec::new();
    let mut total = 0u64;
    while total < target.saturating_sub(8192) || video.is_empty() {
        let size = if video.len() % 30 == 0 {
            rng.range(40_000, 80_000)
        } else {
            rng.range(4_000, 16_000)
        };
        video.push(u32::try_from(size).unwrap_or(u32::MAX));
        total += size;
    }
    let seconds = video.len() as f64 * f64::from(VIDEO_DELTA) / f64::from(VIDEO_TIMESCALE);

    let mut tracks = vec![Track {
        kind: Kind::Video,
        sizes: video,
        per_chunk: p.video_per_chunk,
        chunks: Vec::new(),
    }];
    if let Some(per_chunk) = p.audio_per_chunk {
        let frames = (seconds * f64::from(AUDIO_RATE) / f64::from(AUDIO_DELTA)).ceil() as usize;
        tracks.push(Track {
            kind: Kind::Audio,
            sizes: (0..frames.max(1))
                .map(|_| u32::try_from(rng.range(300, 420)).unwrap_or(400))
                .collect(),
            per_chunk,
            chunks: Vec::new(),
        });
    }
    for _ in 0..p.metadata_tracks {
        tracks.push(Track {
            kind: Kind::Metadata,
            sizes: vec![u32::try_from(rng.range(60, 200)).unwrap_or(100)],
            per_chunk: 1,
            chunks: Vec::new(),
        });
    }

    let mdat = fill_mdat(&mut tracks, rng);

    let mut ftyp_payload = p.brand.as_bytes().to_vec();
    ftyp_payload.extend_from_slice(&0u32.to_be_bytes());
    for b in &p.compatible {
        ftyp_payload.extend_from_slice(b.as_bytes());
    }
    let ftyp = bx(b"ftyp", &ftyp_payload);
    let wide = if p.wide { bx(b"wide", &[]) } else { Vec::new() };
    let mdat_header = mdat_header(mdat.len() as u64);
    let reserved = p.reserve.map(|[lo, hi]| rng.range(lo, hi) as usize);
    let created = u32::try_from(rng.range(3_597_000_000, 3_850_000_000)).unwrap_or(0);
    let sps_seed = rng.bytes(12);

    let mut out = ftyp.clone();
    match p.layout {
        Layout::MoovLast => {
            let base = (ftyp.len() + wide.len() + mdat_header.len()) as u64;
            out.extend(&wide);
            out.extend(&mdat_header);
            out.extend(&mdat);
            out.extend(moov(&tracks, base, p.co64, created, &sps_seed));
        }
        Layout::MoovFirst => {
            // Chunk offset tables have fixed-width entries, so moov's size does not depend
            // on the base offset: build once to measure, then again with the real base.
            let size = moov(&tracks, 0, p.co64, created, &sps_seed).len();
            // The free box fills the reserved area after moov (at least an empty box).
            let reserve = match reserved {
                Some(total) => bx(b"free", &vec![0; total.saturating_sub(size).max(8) - 8]),
                None => Vec::new(),
            };
            let base = (ftyp.len() + size + reserve.len() + wide.len() + mdat_header.len()) as u64;
            out.extend(moov(&tracks, base, p.co64, created, &sps_seed));
            out.extend(&reserve);
            out.extend(&wide);
            out.extend(&mdat_header);
            out.extend(&mdat);
        }
    }
    for [lo, hi] in &p.trailing_uuid {
        let mut payload = rng.bytes(16); // the UUID itself
        let len = rng.range(*lo, *hi) as usize;
        payload.extend(rng.bytes(len));
        out.extend(bx(b"uuid", &payload));
    }

    Generated {
        bytes: out,
        meta: Meta {
            width: Some(WIDTH.into()),
            height: Some(HEIGHT.into()),
            samples: tracks
                .first()
                .map(|t| u32::try_from(t.sizes.len()).unwrap_or(u32::MAX)),
            ..Meta::default()
        },
    }
}

/// Lays chunks out in `mdat` in presentation order (metadata first, then audio and video
/// chunks interleaved by start time) and fills the samples with random bytes.
fn fill_mdat(tracks: &mut [Track], rng: &mut Rng) -> Vec<u8> {
    let mut mdat = Vec::new();
    let mut next = vec![0usize; tracks.len()];
    loop {
        // The pending chunk that starts earliest (metadata counts as time zero).
        let pick = tracks
            .iter()
            .enumerate()
            .filter(|(i, t)| next.get(*i).is_some_and(|&n| n < t.chunk_count()))
            .min_by(|(i, a), (j, b)| {
                let ta = start_time(a, next.get(*i).copied().unwrap_or(0));
                let tb = start_time(b, next.get(*j).copied().unwrap_or(0));
                ta.total_cmp(&tb)
            })
            .map(|(i, _)| i);
        let Some(i) = pick else { break };
        let (Some(t), Some(n)) = (tracks.get_mut(i), next.get_mut(i)) else {
            break;
        };
        t.chunks.push(mdat.len() as u64);
        let first = *n * t.per_chunk as usize;
        let last = (first + t.per_chunk as usize).min(t.sizes.len());
        for (k, &size) in t
            .sizes
            .get(first..last)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            mdat.extend(sample(t.kind, first + k, size, rng));
        }
        *n += 1;
    }
    mdat
}

fn start_time(t: &Track, chunk: usize) -> f64 {
    let sample = (chunk * t.per_chunk as usize) as f64;
    match t.kind {
        Kind::Video => sample * f64::from(VIDEO_DELTA) / f64::from(VIDEO_TIMESCALE),
        Kind::Audio => sample * f64::from(AUDIO_DELTA) / f64::from(AUDIO_RATE),
        Kind::Metadata => -1.0,
    }
}

fn sample(kind: Kind, index: usize, size: u32, rng: &mut Rng) -> Vec<u8> {
    match kind {
        Kind::Video => {
            // A length-prefixed NAL unit: IDR (5) every 30 samples, non-IDR (1) otherwise.
            let nal_len = size.saturating_sub(4);
            let mut s = nal_len.to_be_bytes().to_vec();
            s.push(if index.is_multiple_of(30) { 0x65 } else { 0x41 });
            s.extend(rng.bytes(nal_len.saturating_sub(1) as usize));
            s
        }
        Kind::Audio | Kind::Metadata => rng.bytes(size as usize),
    }
}

fn mdat_header(payload: u64) -> Vec<u8> {
    match u32::try_from(payload + 8) {
        Ok(size) => cat(&[&size.to_be_bytes(), b"mdat"]),
        // 64-bit "largesize" form.
        Err(_) => cat(&[&1u32.to_be_bytes(), b"mdat", &(payload + 16).to_be_bytes()]),
    }
}

fn moov(tracks: &[Track], base: u64, co64: bool, created: u32, sps_seed: &[u8]) -> Vec<u8> {
    let video_duration = tracks.first().map_or(0, |t| {
        u32::try_from(t.sizes.len()).unwrap_or(0) * VIDEO_DELTA
    });
    let next_track = u32::try_from(tracks.len() + 1).unwrap_or(u32::MAX);
    let mvhd = full(
        b"mvhd",
        0,
        0,
        &cat(&[
            &created.to_be_bytes(),
            &created.to_be_bytes(),
            &VIDEO_TIMESCALE.to_be_bytes(),
            &video_duration.to_be_bytes(),
            &0x0001_0000u32.to_be_bytes(), // rate 1.0
            &0x0100u16.to_be_bytes(),      // volume 1.0
            &[0; 10],
            &MATRIX,
            &[0; 24],
            &next_track.to_be_bytes(),
        ]),
    );
    let mut payload = mvhd;
    for (i, t) in tracks.iter().enumerate() {
        let id = u32::try_from(i + 1).unwrap_or(u32::MAX);
        payload.extend(trak(t, id, base, co64, created, video_duration, sps_seed));
    }
    bx(b"moov", &payload)
}

fn trak(
    t: &Track,
    id: u32,
    base: u64,
    co64: bool,
    created: u32,
    movie_duration: u32,
    sps_seed: &[u8],
) -> Vec<u8> {
    let n = u32::try_from(t.sizes.len()).unwrap_or(u32::MAX);
    let (timescale, delta, handler, header, entry) = match t.kind {
        Kind::Video => (
            VIDEO_TIMESCALE,
            VIDEO_DELTA,
            b"vide",
            full(b"vmhd", 0, 1, &[0; 8]),
            avc1(sps_seed),
        ),
        Kind::Audio => (
            AUDIO_RATE,
            AUDIO_DELTA,
            b"soun",
            full(b"smhd", 0, 0, &[0; 4]),
            mp4a(),
        ),
        Kind::Metadata => (
            VIDEO_TIMESCALE,
            movie_duration.max(1),
            b"meta",
            full(b"nmhd", 0, 0, &[]),
            mebx(),
        ),
    };
    let duration = n.saturating_mul(delta);
    let (w, h) = if t.kind == Kind::Video {
        (u32::from(WIDTH) << 16, u32::from(HEIGHT) << 16)
    } else {
        (0, 0)
    };
    let tkhd = full(
        b"tkhd",
        0,
        3,
        &cat(&[
            &created.to_be_bytes(),
            &created.to_be_bytes(),
            &id.to_be_bytes(),
            &[0; 4],
            &movie_duration.to_be_bytes(),
            &[0; 8],
            &[0; 4], // layer, alternate_group
            &if t.kind == Kind::Audio { 0x0100u16 } else { 0 }.to_be_bytes(),
            &[0; 2],
            &MATRIX,
            &w.to_be_bytes(),
            &h.to_be_bytes(),
        ]),
    );
    let mdhd = full(
        b"mdhd",
        0,
        0,
        &cat(&[
            &created.to_be_bytes(),
            &created.to_be_bytes(),
            &timescale.to_be_bytes(),
            &duration.to_be_bytes(),
            &0x55C4u16.to_be_bytes(), // language "und"
            &[0; 2],
        ]),
    );
    let hdlr = full(
        b"hdlr",
        0,
        0,
        &cat(&[&[0; 4], handler, &[0; 12], b"Handler\0"]),
    );
    let dref = full(
        b"dref",
        0,
        0,
        &cat(&[&1u32.to_be_bytes(), &full(b"url ", 0, 1, &[])]),
    );

    let stsd = full(b"stsd", 0, 0, &cat(&[&1u32.to_be_bytes(), &entry]));
    let stts = full(
        b"stts",
        0,
        0,
        &cat(&[&1u32.to_be_bytes(), &n.to_be_bytes(), &delta.to_be_bytes()]),
    );
    let stsc = full(b"stsc", 0, 0, &stsc(t));
    let mut stsz = cat(&[&0u32.to_be_bytes(), &n.to_be_bytes()]);
    for s in &t.sizes {
        stsz.extend_from_slice(&s.to_be_bytes());
    }
    let stsz = full(b"stsz", 0, 0, &stsz);
    let chunks = u32::try_from(t.chunks.len()).unwrap_or(u32::MAX);
    let offsets = if co64 {
        let mut v = chunks.to_be_bytes().to_vec();
        t.chunks
            .iter()
            .for_each(|c| v.extend_from_slice(&(base + c).to_be_bytes()));
        full(b"co64", 0, 0, &v)
    } else {
        let mut v = chunks.to_be_bytes().to_vec();
        t.chunks.iter().for_each(|c| {
            v.extend_from_slice(&u32::try_from(base + c).unwrap_or(u32::MAX).to_be_bytes());
        });
        full(b"stco", 0, 0, &v)
    };
    let mut stbl = cat(&[&stsd, &stts]);
    if t.kind == Kind::Video {
        let keys: Vec<u32> = (0..n).filter(|i| i % 30 == 0).map(|i| i + 1).collect();
        let mut stss = u32::try_from(keys.len())
            .unwrap_or(0)
            .to_be_bytes()
            .to_vec();
        keys.iter()
            .for_each(|k| stss.extend_from_slice(&k.to_be_bytes()));
        stbl.extend(full(b"stss", 0, 0, &stss));
    }
    stbl.extend(cat(&[&stsc, &stsz, &offsets]));

    let minf = bx(
        b"minf",
        &cat(&[&header, &bx(b"dinf", &dref), &bx(b"stbl", &stbl)]),
    );
    let mdia = bx(b"mdia", &cat(&[&mdhd, &hdlr, &minf]));
    bx(b"trak", &cat(&[&tkhd, &mdia]))
}

/// Sample-to-chunk entries: a constant chunk size, plus a shorter last chunk if needed.
fn stsc(t: &Track) -> Vec<u8> {
    let chunks = u32::try_from(t.chunk_count()).unwrap_or(u32::MAX);
    let last = u32::try_from(t.sizes.len()).unwrap_or(0) % t.per_chunk;
    let mut entries = vec![(
        1u32,
        t.per_chunk.min(u32::try_from(t.sizes.len()).unwrap_or(0)),
    )];
    if last != 0 && chunks > 1 {
        entries.push((chunks, last));
    }
    let mut v = u32::try_from(entries.len())
        .unwrap_or(0)
        .to_be_bytes()
        .to_vec();
    for (first, per) in entries {
        v.extend(cat(&[
            &first.to_be_bytes(),
            &per.to_be_bytes(),
            &1u32.to_be_bytes(),
        ]));
    }
    v
}

/// An `avc1` sample entry with `avcC`. SPS/PPS are placeholders.
fn avc1(sps_seed: &[u8]) -> Vec<u8> {
    let mut sps = vec![0x67, 0x64, 0x00, 0x28];
    sps.extend_from_slice(sps_seed);
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

/// An `mp4a` sample entry with a minimal `esds` (AAC LC, 44.1 kHz, stereo).
fn mp4a() -> Vec<u8> {
    let descriptor = |tag: u8, body: &[u8]| cat(&[&[tag, body.len() as u8], body]);
    let decoder_specific = descriptor(0x05, &[0x12, 0x10]);
    let decoder_config = descriptor(
        0x04,
        &cat(&[
            &[0x40, 0x15],             // AAC, audio stream
            &[0, 0x18, 0],             // buffer size
            &128_000u32.to_be_bytes(), // max bitrate
            &128_000u32.to_be_bytes(), // average bitrate
            &decoder_specific,
        ]),
    );
    let sl_config = descriptor(0x06, &[0x02]);
    let es = descriptor(0x03, &cat(&[&[0, 0, 0], &decoder_config, &sl_config]));
    let esds = full(b"esds", 0, 0, &es);
    bx(
        b"mp4a",
        &cat(&[
            &[0; 6],
            &1u16.to_be_bytes(), // data_reference_index
            &[0; 8],
            &2u16.to_be_bytes(),  // channels
            &16u16.to_be_bytes(), // sample size
            &[0; 4],
            &(AUDIO_RATE << 16).to_be_bytes(),
            &esds,
        ]),
    )
}

/// A `mebx` (timed metadata) sample entry with an empty key table.
fn mebx() -> Vec<u8> {
    bx(
        b"mebx",
        &cat(&[&[0; 6], &1u16.to_be_bytes(), &bx(b"keys", &[])]),
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

    fn top_level(bytes: &[u8]) -> Vec<String> {
        let mut pos = 0usize;
        let mut types = Vec::new();
        while pos < bytes.len() {
            let size = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
            types.push(String::from_utf8_lossy(&bytes[pos + 4..pos + 8]).into_owned());
            assert!(size >= 8);
            pos += size;
        }
        assert_eq!(pos, bytes.len());
        types
    }

    /// Walking the boxes, the top-level order and sizes are consistent.
    #[test]
    fn plain_layout() {
        let g = generate(500_000, &mut Rng::new(7));
        assert_eq!(top_level(&g.bytes), ["ftyp", "mdat", "moov"]);
        assert!(g.bytes.len() >= 500_000 - 8192);
    }

    fn iphone() -> VideoProfile {
        VideoProfile {
            brand: "qt  ".to_owned(),
            compatible: vec!["qt  ".to_owned()],
            wide: true,
            video_per_chunk: 15,
            audio_per_chunk: Some(43),
            metadata_tracks: 2,
            ..plain()
        }
    }

    #[test]
    fn quicktime_layout() {
        let g = generate_profiled(&iphone(), 2_000_000, &mut Rng::new(1));
        assert_eq!(top_level(&g.bytes), ["ftyp", "wide", "mdat", "moov"]);
        assert_eq!(&g.bytes[8..12], b"qt  ");
    }

    #[test]
    fn moov_first_with_reserve_and_uuid() {
        let p = VideoProfile {
            layout: Layout::MoovFirst,
            reserve: Some([4000, 8000]),
            co64: true,
            audio_per_chunk: Some(49),
            video_per_chunk: 33,
            trailing_uuid: vec![[100, 200]],
            ..plain()
        };
        let g = generate_profiled(&p, 2_000_000, &mut Rng::new(2));
        assert_eq!(
            top_level(&g.bytes),
            ["ftyp", "moov", "free", "mdat", "uuid"]
        );
    }

    /// Every chunk offset in moov points inside mdat, at the first sample of that chunk.
    #[test]
    fn chunk_offsets_point_at_samples() {
        for (layout, co64) in [(Layout::MoovLast, false), (Layout::MoovFirst, true)] {
            let p = VideoProfile {
                layout,
                co64,
                reserve: Some([100, 100]),
                ..iphone()
            };
            let g = generate_profiled(&p, 1_000_000, &mut Rng::new(3));
            let b = &g.bytes;
            let tag: &[u8] = if co64 { b"co64" } else { b"stco" };
            let video = b.windows(4).position(|w| w == tag).unwrap();
            let n = u32::from_be_bytes(b[video + 8..video + 12].try_into().unwrap()) as usize;
            let first = if co64 {
                u64::from_be_bytes(b[video + 12..video + 20].try_into().unwrap()) as usize
            } else {
                u32::from_be_bytes(b[video + 12..video + 16].try_into().unwrap()) as usize
            };
            assert!(n > 1);
            // The first video chunk starts with a 4-byte length then an IDR NAL header.
            assert_eq!(b[first + 4], 0x65, "{layout:?}");
        }
    }
}
