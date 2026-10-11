//! Structure-aware MP4 / QuickTime carving.
//!
//! A file starts with `ftyp`; top-level boxes are walked by their sizes until something
//! that is not a known top-level box (or the next file's `ftyp`). The sample tables in
//! `moov` are then checked against the data: every chunk must lie inside the file, and for
//! AVC/HEVC video every chunk must start with a plausible length-prefixed NAL unit. The
//! first chunk that fails is reported as a likely fragment boundary.
//!
//! Reference: ISO/IEC 14496-12 (box structure 4.2, `stco`/`co64` 8.7.5, `stsz` 8.7.3,
//! `stsc` 8.7.4, `stsd` 8.5.2), ISO/IEC 14496-15 (length-prefixed NAL units), ISO/IEC
//! 14496-10 7.3.1 (NAL unit header), Apple "QuickTime File Format".

use blocksource::BlockSource;

use crate::reader::Reader;

/// An MP4 / QuickTime file found at some offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4 {
    pub len: u64,
    pub status: Status,
    /// `ftyp` major brand, e.g. `*b"qt  "`.
    pub brand: [u8; 4],
    /// Whether `moov` comes before `mdat`.
    pub moov_first: bool,
    /// Chunks checked against the data (0 if there is no `moov`).
    pub chunks_checked: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Every top-level box present and every chunk checked out.
    Complete,
    /// A chunk does not fit or does not look like its track's data, at this offset.
    Corrupt { at: u64 },
    /// No `moov` (e.g. recording interrupted) or the data ran out.
    Truncated,
}

/// Top-level box types that may appear in a camera/phone file.
const TOP_LEVEL: &[&[u8; 4]] = &[
    b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"uuid", b"meta", b"pnot", b"udta",
    b"pdin", b"moof", b"mfra", b"sidx", b"styp", b"junk", b"PICT",
];

/// Tries to carve an MP4/QuickTime file at `start`. `None` if there is no `ftyp` there.
pub fn carve<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Mp4> {
    let ftyp_size = u64::from(r.u32_be(start)?);
    if r.slice(start + 4, 4)? != b"ftyp" || !(16..=512).contains(&ftyp_size) {
        return None;
    }
    let brand: [u8; 4] = r.slice(start + 8, 4)?.try_into().ok()?;
    if !brand.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        return None;
    }

    let limit = start.saturating_add(max_len).min(r.len());
    let mut pos = start;
    let mut moov = None;
    let mut mdat = None;
    let mut truncated = false;
    while pos + 8 <= limit {
        let Some(size32) = r.u32_be(pos) else { break };
        let Some(ty) = r
            .slice(pos + 4, 4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
        else {
            break;
        };
        if !TOP_LEVEL.contains(&&ty) || (ty == *b"ftyp" && pos != start) {
            break;
        }
        let (size, header) = match size32 {
            1 => match r.u64_be(pos + 8) {
                Some(s) => (s, 16),
                None => break,
            },
            // "Extends to the end of the file": its real length is unknown.
            0 => {
                truncated = true;
                (limit - pos, 8)
            }
            s => (u64::from(s), 8),
        };
        if size < header {
            break;
        }
        let end = pos.saturating_add(size);
        if end > limit {
            truncated = true;
        }
        match &ty {
            b"moov" => moov = Some((pos + header, end.min(limit))),
            b"mdat" => mdat = Some((pos + header, end.min(limit))),
            _ => {}
        }
        pos = end.min(limit);
        if truncated {
            break;
        }
    }

    let len = pos - start;
    let moov_first = matches!((moov, mdat), (Some(m), Some(d)) if m.0 < d.0);
    let mut out = Mp4 {
        len,
        status: Status::Truncated,
        brand,
        moov_first,
        chunks_checked: 0,
    };
    let (Some(moov), Some(_)) = (moov, mdat) else {
        return Some(out);
    };
    let tracks = tracks(r, moov.0, moov.1);
    let mut first_bad: Option<u64> = None;
    for t in &tracks {
        for (i, &offset) in t.chunks.iter().enumerate() {
            out.chunks_checked += 1;
            let chunk_len = t.chunk_len(i);
            let abs = start.saturating_add(offset);
            let fits = offset >= ftyp_size && offset.saturating_add(chunk_len) <= len;
            if !fits || (t.nal && !nal_ok(r, abs, chunk_len)) {
                first_bad = Some(first_bad.map_or(abs, |b| b.min(abs)));
                break;
            }
        }
    }
    out.status = match (first_bad, truncated) {
        (Some(at), _) => Status::Corrupt { at },
        (None, true) => Status::Truncated,
        (None, false) => Status::Complete,
    };
    Some(out)
}

/// What we need from one track's sample tables.
struct Track {
    /// Chunk offsets relative to the start of the file.
    chunks: Vec<u64>,
    sizes: Vec<u32>,
    /// Sample size if constant (`stsz` sample_size != 0).
    constant: u32,
    /// `stsc` entries: (first chunk, samples per chunk), first chunk is 1-based.
    stsc: Vec<(u32, u32)>,
    /// Length-prefixed NAL units (AVC/HEVC).
    nal: bool,
}

impl Track {
    /// Bytes in chunk `i` (0-based), from `stsc` and `stsz`.
    fn chunk_len(&self, i: usize) -> u64 {
        let chunk = u32::try_from(i + 1).unwrap_or(u32::MAX);
        // Walk the stsc runs to find the chunk's first sample and samples per chunk.
        let mut sample = 0u64;
        let mut per = 0u32;
        for (k, &(first, n)) in self.stsc.iter().enumerate() {
            let next = self.stsc.get(k + 1).map_or(u32::MAX, |e| e.0);
            if chunk >= next {
                sample += u64::from(next.saturating_sub(first)) * u64::from(n);
                continue;
            }
            sample += u64::from(chunk.saturating_sub(first)) * u64::from(n);
            per = n;
            break;
        }
        if self.constant != 0 {
            return u64::from(self.constant) * u64::from(per);
        }
        let s = usize::try_from(sample).unwrap_or(usize::MAX);
        let e = s.saturating_add(per as usize).min(self.sizes.len());
        self.sizes
            .get(s.min(e)..e)
            .unwrap_or_default()
            .iter()
            .map(|&x| u64::from(x))
            .sum()
    }
}

/// Reads every `trak` in `moov` (payload range `[from, to)`).
fn tracks<S: BlockSource + ?Sized>(r: &mut Reader<S>, from: u64, to: u64) -> Vec<Track> {
    let mut out = Vec::new();
    for (ty, b, e) in children(r, from, to) {
        if &ty != b"trak" {
            continue;
        }
        let Some(stbl) = path(r, b, e, &[b"mdia", b"minf", b"stbl"]) else {
            continue;
        };
        let mut t = Track {
            chunks: Vec::new(),
            sizes: Vec::new(),
            constant: 0,
            stsc: Vec::new(),
            nal: false,
        };
        for (ty, b, e) in children(r, stbl.0, stbl.1) {
            match &ty {
                // Full boxes: 4 bytes of version/flags, then an entry count.
                b"stco" | b"co64" => {
                    let wide = &ty == b"co64";
                    let n = r.u32_be(b + 4).unwrap_or(0);
                    let step = if wide { 8 } else { 4 };
                    for k in 0..u64::from(n).min((e - b).saturating_sub(8) / step) {
                        let at = b + 8 + k * step;
                        let v = if wide {
                            r.u64_be(at)
                        } else {
                            r.u32_be(at).map(u64::from)
                        };
                        t.chunks.extend(v);
                    }
                }
                b"stsz" => {
                    t.constant = r.u32_be(b + 4).unwrap_or(0);
                    let n = r.u32_be(b + 8).unwrap_or(0);
                    if t.constant == 0 {
                        for k in 0..u64::from(n).min((e - b).saturating_sub(12) / 4) {
                            t.sizes.extend(r.u32_be(b + 12 + 4 * k));
                        }
                    }
                }
                b"stsc" => {
                    let n = r.u32_be(b + 4).unwrap_or(0);
                    for k in 0..u64::from(n).min((e - b).saturating_sub(8) / 12) {
                        let at = b + 8 + 12 * k;
                        if let (Some(first), Some(per)) = (r.u32_be(at), r.u32_be(at + 4)) {
                            t.stsc.push((first, per));
                        }
                    }
                }
                b"stsd" => {
                    // The first sample entry's type sits after version/flags, count, size.
                    let codec = r.slice(b + 12, 4).unwrap_or_default();
                    t.nal = matches!(codec, b"avc1" | b"avc3" | b"hvc1" | b"hev1");
                }
                _ => {}
            }
        }
        out.push(t);
    }
    out
}

/// Child boxes of a container payload `[from, to)`: (type, payload start, end).
fn children<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    from: u64,
    to: u64,
) -> Vec<([u8; 4], u64, u64)> {
    let mut out = Vec::new();
    let mut pos = from;
    while pos + 8 <= to {
        let (Some(size), Some(ty)) = (r.u32_be(pos), r.slice(pos + 4, 4)) else {
            break;
        };
        let Ok(ty) = <[u8; 4]>::try_from(ty) else {
            break;
        };
        let (size, header) = match size {
            1 => (r.u64_be(pos + 8).unwrap_or(0), 16),
            0 => (to - pos, 8),
            s => (u64::from(s), 8),
        };
        if size < header || pos.saturating_add(size) > to {
            break;
        }
        out.push((ty, pos + header, pos + size));
        pos += size;
    }
    out
}

/// Follows a path of container types from `[from, to)`.
fn path<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    mut from: u64,
    mut to: u64,
    types: &[&[u8; 4]],
) -> Option<(u64, u64)> {
    for want in types {
        let (_, b, e) = children(r, from, to)
            .into_iter()
            .find(|(t, _, _)| t == *want)?;
        from = b;
        to = e;
    }
    Some((from, to))
}

/// A chunk of AVC/HEVC samples starts with a 4-byte NAL length that fits in the chunk,
/// followed by a NAL header whose forbidden_zero_bit is 0.
fn nal_ok<S: BlockSource + ?Sized>(r: &mut Reader<S>, at: u64, chunk_len: u64) -> bool {
    match (r.u32_be(at), r.byte(at + 4)) {
        (Some(len), Some(header)) => {
            len > 0 && u64::from(len) + 4 <= chunk_len.max(5) && header & 0x80 == 0
        }
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = u32::try_from(8 + payload.len())
            .unwrap()
            .to_be_bytes()
            .to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    fn full(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0];
        p.extend_from_slice(payload);
        bx(ty, &p)
    }

    /// ftyp, mdat with two NAL-unit chunks, moov describing them.
    fn movie(break_second_chunk: bool) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"isom\0\0\0\0isommp42");
        let mut sample = 96u32.to_be_bytes().to_vec(); // NAL length
        sample.push(0x65);
        sample.extend([0x11; 95]);
        let mut second = sample.clone();
        if break_second_chunk {
            second[0] = 0xFF;
        }
        let mdat = bx(b"mdat", &[sample.clone(), second].concat());
        let base = (ftyp.len() + 8) as u32;
        let stsd = full(
            b"stsd",
            &[&1u32.to_be_bytes()[..], &bx(b"avc1", &[0; 78])].concat(),
        );
        let stco = full(
            b"stco",
            &[
                2u32.to_be_bytes(),
                base.to_be_bytes(),
                (base + 100).to_be_bytes(),
            ]
            .concat(),
        );
        let stsz = full(
            b"stsz",
            &[
                0u32.to_be_bytes(),
                2u32.to_be_bytes(),
                100u32.to_be_bytes(),
                100u32.to_be_bytes(),
            ]
            .concat(),
        );
        let stsc = full(
            b"stsc",
            &[
                1u32.to_be_bytes(),
                1u32.to_be_bytes(),
                1u32.to_be_bytes(),
                1u32.to_be_bytes(),
            ]
            .concat(),
        );
        let stbl = bx(b"stbl", &[stsd, stsc, stsz, stco].concat());
        let trak = bx(b"trak", &bx(b"mdia", &bx(b"minf", &stbl)));
        let moov = bx(b"moov", &trak);
        [ftyp, mdat, moov].concat()
    }

    fn run(data: Vec<u8>) -> Option<Mp4> {
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        carve(&mut r, 0, 1 << 30)
    }

    #[test]
    fn complete_movie() {
        let m = movie(false);
        let mut padded = m.clone();
        padded.extend([0u8; 300]);
        let got = run(padded).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(got.len, m.len() as u64);
        assert_eq!(got.chunks_checked, 2);
        assert!(!got.moov_first);
    }

    #[test]
    fn broken_chunk_is_reported() {
        let got = run(movie(true)).unwrap();
        assert!(matches!(got.status, Status::Corrupt { .. }));
    }

    #[test]
    fn missing_moov_is_truncated() {
        let mut m = movie(false);
        let moov_at = m.windows(4).rposition(|w| w == b"moov").unwrap() - 4;
        m.truncate(moov_at);
        assert_eq!(run(m).unwrap().status, Status::Truncated);
    }

    #[test]
    fn not_an_mp4() {
        assert!(run(vec![0u8; 64]).is_none());
    }
}
