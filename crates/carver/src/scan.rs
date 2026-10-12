//! Walks a whole source and carves every JPEG and MP4/QuickTime file it finds.
//!
//! Files start on sector boundaries (they start on cluster boundaries in any file system),
//! so only sector starts are checked for headers. After a complete file the scan resumes
//! after it; after a broken one it resumes at the next sector, since the rest of the data
//! may belong to other files.
//!
//! Broken JPEGs are reassembled after the whole source has been scanned, so the search for
//! their missing pieces can skip every file found whole: on a full disk that is most of
//! the data.

use blocksource::BlockSource;

use crate::{Found, Kind, Status, avi, fragments, heif, jpeg, mp4, mts, png, raw, reader::Reader};

/// Scan limits.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Longest JPEG to follow.
    pub max_jpeg: u64,
    /// Longest video to follow.
    pub max_video: u64,
    /// Search for the missing fragments of broken JPEGs and transport streams (`None` to
    /// skip it).
    pub reassemble: Option<fragments::Options>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_jpeg: 64 << 20,
            max_video: 64 << 30,
            reassemble: Some(fragments::Options::default()),
        }
    }
}

const CHUNK: usize = 4 << 20;

/// Scans `src` from start to end.
pub fn scan<S: BlockSource + ?Sized>(src: &S, opts: &Options) -> Vec<Found> {
    let ss = u64::from(src.sector_size()).max(512);
    let step = usize::try_from(ss).unwrap_or(512);
    let mut parse = Reader::new(src);
    let mut buf = vec![0u8; CHUNK];
    let mut found = Vec::new();
    let mut pos = 0u64;
    let mut skip_until = 0u64;
    // Later fragments of files found, which repeat table headers (transport streams).
    let mut claimed: Vec<(u64, u64)> = Vec::new();
    while pos < src.len() {
        let Ok(n) = src.read_at(pos, &mut buf) else {
            // Unreadable: move on (bad ranges are retried by the scan pipeline, not here).
            pos += CHUNK as u64;
            continue;
        };
        if n == 0 {
            break;
        }
        let data = buf.get(..n).unwrap_or_default();
        for off in (0..n).step_by(step) {
            let at = pos + off as u64;
            if at < skip_until {
                continue;
            }
            let head = data
                .get(off..off + HEADER_LEN)
                .or_else(|| data.get(off..))
                .unwrap_or_default();
            let hit = if looks_like_header(head)
                && !claimed.iter().any(|&(o, l)| (o..o + l).contains(&at))
            {
                carve_at_with(&mut parse, at, opts, false)
            } else {
                None
            };
            if let Some(f) = hit {
                if let Some(&(first, len)) = f.claimed().first()
                    && first == at
                {
                    skip_until = at + len;
                }
                claimed.extend(f.claimed().iter().skip(1));
                found.push(f);
            }
        }
        pos += n as u64;
    }
    // A later fragment placed before its file's start was found as a file of its own.
    let inner: Vec<(u64, u64)> = found
        .iter()
        .flat_map(|f| f.claimed().iter().skip(1).copied())
        .collect();
    found.retain(|f| !inner.iter().any(|&(o, l)| (o..o + l).contains(&f.offset)));
    drop_echoes(&mut found);
    if let Some(o) = &opts.reassemble {
        reassemble_broken(&mut parse, &mut found, opts.max_jpeg, o);
    }
    found
}

/// Drops broken JPEGs that are the tail of another broken JPEG found before them: they
/// start inside it and break at the same place, so they are the same picture read from a
/// later `FF D8` (Canon ends the Exif segment with those bytes, on a sector boundary).
fn drop_echoes(found: &mut Vec<Found>) {
    let breaks: Vec<(u64, u64, u64)> = found
        .iter()
        .filter(|f| f.kind == Kind::Jpeg)
        .filter_map(|f| match f.status {
            Status::Corrupt { at } => Some((f.offset, f.offset + f.len, at)),
            _ => None,
        })
        .collect();
    found.retain(|f| {
        let Status::Corrupt { at } = f.status else {
            return true;
        };
        f.kind != Kind::Jpeg
            || !breaks
                .iter()
                .any(|&(o, e, b)| o < f.offset && f.offset < e && b == at)
    });
}

/// Reassembles the broken JPEGs in `found`, looking for their pieces outside the files
/// found whole.
fn reassemble_broken<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    found: &mut [Found],
    max_len: u64,
    o: &fragments::Options,
) {
    let mut taken: Vec<(u64, u64)> = found
        .iter()
        .filter(|f| f.status == Status::Complete)
        .flat_map(|f| f.fragments.iter().map(|&(at, len)| (at, at + len)))
        .collect();
    taken.sort_unstable();
    // Merge overlaps (a file inside another, e.g. a JPEG frame in a video).
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(taken.len());
    for (a, e) in taken {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((a, e)),
        }
    }
    for f in found.iter_mut() {
        if f.kind != Kind::Jpeg || !matches!(f.status, Status::Corrupt { .. }) {
            continue;
        }
        let Some(joined) =
            fragments::reassemble_avoiding(r, f.offset, max_len, o, &merged).filter(|j| j.complete)
        else {
            continue;
        };
        f.len = joined.fragments.iter().map(|x| x.1).sum();
        f.fragments = joined.fragments;
        f.status = Status::Complete;
    }
}

/// Bytes from a sector start that [`looks_like_header`] wants (three 192-byte transport
/// stream packets; other formats need only the first 8).
pub const HEADER_LEN: usize = 3 * 192;

/// Whether `head` (the first bytes of a sector, ideally [`HEADER_LEN`]) starts a file this
/// crate can carve.
pub fn looks_like_header(head: &[u8]) -> bool {
    head.starts_with(&[0xFF, 0xD8, 0xFF])
        || head.get(4..8) == Some(b"ftyp")
        || avi::looks_like_avi(head)
        || png::looks_like_png(head)
        || raw::looks_like_raw(head)
        || mts::looks_like_mts(head)
}

/// Whether the JPEG at `at` is a Motion JPEG frame: right after an AVI frame chunk header
/// (`##dc` or `##db`) that holds it. Frames are part of a video, not photos.
fn in_avi_frame<S: BlockSource + ?Sized>(r: &mut Reader<S>, at: u64) -> bool {
    let Some(head) = at.checked_sub(8).and_then(|h| r.slice(h, 8)) else {
        return false;
    };
    let [a, b, c, d, s0, s1, s2, s3] = <[u8; 8]>::try_from(head).unwrap_or_default();
    a.is_ascii_hexdigit()
        && b.is_ascii_hexdigit()
        && c == b'd'
        && (d == b'c' || d == b'b')
        && u32::from_le_bytes([s0, s1, s2, s3]) > 2
}

/// Carves the file starting at `at`, if any: a JPEG (reassembled if fragmented and
/// `opts.reassemble` is set), a HEIC image, an MP4/QuickTime file or an AVCHD/BDAV
/// transport stream. This is what [`scan`] does at each
/// header it finds, for callers that drive the reading themselves.
pub fn carve_at<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    at: u64,
    opts: &Options,
) -> Option<Found> {
    carve_at_with(r, at, opts, true)
}

/// [`carve_at`], reassembling a broken JPEG on the spot only if `jpeg_now` (otherwise the
/// caller does it later, knowing more of the disk).
fn carve_at_with<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    at: u64,
    opts: &Options,
    jpeg_now: bool,
) -> Option<Found> {
    let head = r.slice(at, 8)?.to_vec();
    if r.slice(at, HEADER_LEN).is_some_and(mts::looks_like_mts) {
        let m = mts::follow(r, at, opts.max_video, opts.reassemble.as_ref())?;
        let status = match m.status {
            mts::Status::Complete => Status::Complete,
            mts::Status::Corrupt { at } => Status::Corrupt { at },
            mts::Status::Truncated => Status::Truncated,
        };
        return Some(Found {
            fragments: m.fragments,
            ..Found::contiguous(at, m.len, Kind::Mts, status)
        });
    }
    if avi::looks_like_avi(&head) || r.slice(at, 12).is_some_and(avi::looks_like_avi) {
        let a = avi::follow(r, at, opts.max_video, opts.reassemble.as_ref())?;
        let status = match a.status {
            avi::Status::Complete => Status::Complete,
            avi::Status::Corrupt { at } => Status::Corrupt { at },
            avi::Status::Truncated => Status::Truncated,
        };
        let mut found = Found {
            fragments: a.fragments,
            ..Found::contiguous(at, a.len, Kind::Avi, status)
        };
        found.has_index = Some(a.has_index);
        if a.width > 0 && a.height > 0 {
            found.width = Some(a.width);
            found.height = Some(a.height);
        }
        return Some(found);
    }
    if png::looks_like_png(&head) {
        let p = png::follow(r, at, opts.max_jpeg, opts.reassemble.as_ref())?;
        let status = match p.status {
            png::Status::Complete => Status::Complete,
            png::Status::Corrupt { at } => Status::Corrupt { at },
            png::Status::Truncated => Status::Truncated,
        };
        let mut found = Found {
            fragments: p.fragments,
            ..Found::contiguous(at, p.len, Kind::Png, status)
        };
        if p.width > 0 && p.height > 0 {
            found.width = Some(p.width);
            found.height = Some(p.height);
        }
        return Some(found);
    }
    if r.slice(at, 16).is_some_and(raw::looks_like_raw) {
        let x = raw::carve(r, at, opts.max_jpeg.max(256 << 20))?;
        let status = match x.status {
            raw::Status::Complete => Status::Complete,
            raw::Status::Corrupt { at } => Status::Corrupt { at },
            raw::Status::Truncated => Status::Truncated,
        };
        let kind = match x.kind {
            raw::RawKind::Tiff => Kind::Tiff,
            raw::RawKind::Cr2 => Kind::Cr2,
            raw::RawKind::Nef => Kind::Nef,
            raw::RawKind::Arw => Kind::Arw,
            raw::RawKind::Dng => Kind::Dng,
            raw::RawKind::Orf => Kind::Orf,
            raw::RawKind::Rw2 => Kind::Rw2,
            raw::RawKind::Pef => Kind::Pef,
            raw::RawKind::Raf => Kind::Raf,
        };
        let mut found = Found::contiguous(at, x.len, kind, status);
        found.verified = x.verified;
        if x.width > 0 && x.height > 0 {
            found.width = Some(x.width);
            found.height = Some(x.height);
        }
        return Some(found);
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        if in_avi_frame(r, at) {
            return None;
        }
        let j = jpeg::carve(r, at, opts.max_jpeg)?;
        let status = match j.status {
            jpeg::Status::Complete => Status::Complete,
            jpeg::Status::Corrupt { at } => Status::Corrupt { at },
            jpeg::Status::Truncated => Status::Truncated,
        };
        let joined = opts
            .reassemble
            .filter(|_| jpeg_now && matches!(status, Status::Corrupt { .. }))
            .and_then(|o| fragments::reassemble(r, at, opts.max_jpeg, &o))
            .filter(|f| f.complete);
        let mut found = match joined {
            Some(f) => Found {
                len: f.fragments.iter().map(|x| x.1).sum(),
                status: Status::Complete,
                fragments: f.fragments,
                ..Found::contiguous(at, 0, Kind::Jpeg, Status::Complete)
            },
            None => Found::contiguous(at, j.len, Kind::Jpeg, status),
        };
        if j.width > 0 && j.height > 0 {
            found.width = Some(u32::from(j.width));
            found.height = Some(u32::from(j.height));
        }
        return Some(found);
    }
    if head.get(4..8) == Some(b"ftyp") && heif::is_heif(r, at).is_some() {
        let h = heif::carve(r, at, opts.max_jpeg)?;
        let status = match h.status {
            heif::Status::Complete => Status::Complete,
            heif::Status::Corrupt { at } => Status::Corrupt { at },
            heif::Status::Truncated => Status::Truncated,
        };
        return Some(Found::contiguous(at, h.len, Kind::Heic, status));
    }
    if head.get(4..8) == Some(b"ftyp") {
        let m = mp4::carve(r, at, opts.max_video)?;
        let kind = if &m.brand == b"qt  " {
            Kind::Mov
        } else if &m.brand == b"crx " {
            Kind::Cr3
        } else {
            Kind::Mp4
        };
        let status = match m.status {
            mp4::Status::Complete => Status::Complete,
            mp4::Status::Corrupt { at } => Status::Corrupt { at },
            mp4::Status::Truncated => Status::Truncated,
        };
        let mut found = Found::contiguous(at, m.len, kind, status);
        found.has_index = Some(m.has_moov);
        return Some(found);
    }
    None
}
