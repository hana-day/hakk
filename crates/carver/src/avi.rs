//! AVI (`.avi`) carving: the RIFF container of 2000s digital cameras (Motion JPEG with PCM
//! audio) and of OpenDML files over a gigabyte.
//!
//! The file is a `RIFF` chunk of form `AVI ` whose size covers the whole file, followed for
//! OpenDML files by further `RIFF` chunks of form `AVIX`. Inside, chunks are a four-letter
//! id, a 32-bit little-endian size and the data padded to an even length: the headers
//! (`LIST hdrl`), maker data (`LIST INFO`, Nikon's `LIST ncdt`, `JUNK`), the media
//! (`LIST movi`, one chunk per frame or audio block) and the index (`idx1`). Every chunk
//! header is checked, the media ones included, so a file is complete only if the chain of
//! sizes lands exactly on its end; the first header that does not fit is where the file
//! breaks (on disk, usually where a fragment ends).
//!
//! A fragmented file is joined back together: where a header does not fit, the next
//! fragment is looked for a whole number of clusters away, and taken if the chain of chunk
//! headers goes on from there and the frame cut in two (a JPEG) decodes completely with
//! its second part taken from there; that also tells on which cluster the cut lies. Where
//! the cut falls in audio instead, the chain must go on further, and the cut is put where
//! the waveform runs on most smoothly. When fragments were joined, the index (`idx1`) is
//! checked against the chunks found.
//!
//! Reference: Microsoft, "AVI RIFF File Reference" (AVIMAINHEADER, AVISTREAMHEADER,
//! AVIOLDINDEX); Microsoft/IBM, "Multimedia Programming Interface and Data Specifications
//! 1.0" (RIFF chunks, padding); OpenDML, "AVI File Format Extensions" 1.02 (`AVIX`
//! continuation chunks, `indx` super indexes, `ix##` chunks in `movi`); Microsoft,
//! WAVEFORMATEX (audio format tags 1 = PCM, 7 = mu-law); ITU-T G.711 (mu-law expansion).

use blocksource::{BlockSource, MemSource};

use crate::{fragments, jpeg, pieces::Pieces, reader::Reader};

/// How far from the end of a fragment (either way) to look for the next one.
const JOIN_REACH: u64 = 64 << 20;
/// Most fragments to join.
const MAX_FRAGMENTS: usize = 1024;
/// Chunk headers that must follow on from a candidate fragment for it to be taken without
/// a frame to decode across the cut (or fewer, ending the list).
const CHAIN: usize = 16;
/// Bytes on each side of the cuts considered whose waveform is compared.
const SEAM_WINDOW: u64 = 1024;
/// Largest change around an audio seam, relative to the largest before it, for the cut to
/// count as right.
const SEAM_LIMIT: f64 = 1.5;
/// Index entries checked after joining.
const MAX_INDEX_CHECKS: u64 = 1 << 20;

/// An AVI file found at some offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avi {
    /// Bytes from the start that hold together (all of them if complete): for a broken
    /// file, up to the last chunk whose end a following header confirms.
    pub len: u64,
    pub status: Status,
    /// `(source offset, length)` of each piece in file order; one piece if contiguous.
    pub fragments: Vec<(u64, u64)>,
    /// Frame size from the main header.
    pub width: u32,
    pub height: u32,
    /// Whether an index (`idx1`, or an OpenDML `indx`) was found.
    pub has_index: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The chunks fill the declared size exactly.
    Complete,
    /// A chunk header that does not fit (or, after joining, does not match the index) at
    /// this source offset.
    Corrupt { at: u64 },
    /// The data ran out, or the sizes were never written (the recording was cut off).
    Truncated,
}

/// Whether `head` starts an AVI file.
pub fn looks_like_avi(head: &[u8]) -> bool {
    head.starts_with(b"RIFF") && head.get(8..12) == Some(b"AVI ")
}

/// Follows the file at `start` without joining fragments. `None` if it is not an AVI file.
pub fn carve<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Avi> {
    follow(r, start, max_len, None)
}

/// Follows the file at `start`, joining the fragments it finds when `join` is set (its
/// grid; the reach and fragment count are this module's own, as videos are large).
pub fn follow<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    start: u64,
    max_len: u64,
    join: Option<&fragments::Options>,
) -> Option<Avi> {
    if !looks_like_avi(r.slice(start, 12)?) {
        return None;
    }
    let mut walk = Walk {
        r,
        pieces: Pieces::new(start),
        join,
        width: 0,
        height: 0,
        has_index: false,
        movi: None,
        idx1: None,
        streams: Vec::new(),
        confirmed: 0,
    };
    let mut pos = 0u64;
    let mut status = loop {
        let size = u64::from(walk.u32(pos + 4)?);
        let end = pos + 8 + size + (size & 1);
        // A recording cut off before the sizes were written leaves them zero.
        let declared = size >= 4 && end <= max_len;
        let inner_end = if declared { end } else { max_len };
        match walk.chunks(pos + 12, inner_end, Level::Top) {
            // Fewer than 8 bytes left over can hold no chunk; take them as slack.
            Ok(p) if declared && end - p < 8 => {}
            Ok(p) | Err(Stop::Ran(p)) => {
                pos = p;
                break Status::Truncated;
            }
            Err(Stop::Bad(p)) if !declared => {
                pos = p;
                break Status::Truncated;
            }
            Err(Stop::Bad(p)) => {
                pos = walk.confirmed;
                break Status::Corrupt {
                    at: walk.pieces.physical(p),
                };
            }
        }
        pos = end;
        // OpenDML: the file goes on in `RIFF AVIX` chunks.
        let mut next = [0u8; 12];
        let more = walk.pieces.read(walk.r, pos, &mut next).is_some()
            && next.starts_with(b"RIFF")
            && next.get(8..12) == Some(b"AVIX");
        if !more {
            break Status::Complete;
        }
    };
    if walk.pieces.count() > 1
        && status == Status::Complete
        && let Some(at) = walk.index_mismatch()
    {
        status = Status::Corrupt {
            at: walk.pieces.physical(at),
        };
    }
    Some(Avi {
        len: pos,
        status,
        fragments: walk.pieces.fragments(pos),
        width: walk.width,
        height: walk.height,
        has_index: walk.has_index,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    /// Directly inside `RIFF`.
    Top,
    /// Inside `LIST movi` (or a `rec ` list in it): media chunks.
    Movi,
    /// Inside another list.
    List,
}

/// Why a walk stopped short.
enum Stop {
    /// A chunk header that does not fit, at this logical offset.
    Bad(u64),
    /// The data ran out at this logical offset.
    Ran(u64),
}

/// A chunk header: id, data size, and a list's type (if the bytes were there).
struct Header {
    id: [u8; 4],
    size: u64,
    list: Option<[u8; 4]>,
}

impl Header {
    /// Offset after the chunk at `pos`, padding included.
    fn next(&self, pos: u64) -> u64 {
        pos + 8 + self.size + (self.size & 1)
    }

    /// Whether this header can stand at `pos` in a list ending at `end`.
    fn fits(&self, pos: u64, end: u64, level: Level) -> bool {
        // The padding byte of a list's last chunk may be left off.
        valid_id(&self.id, level)
            && self.next(pos) <= end + 1
            && (self.id != *b"LIST"
                || self.size >= 4 && self.list.is_some_and(|l| valid_id(&l, Level::List)))
    }

    /// Whether the chunk holds a video frame.
    fn is_frame(&self) -> bool {
        matches!(self.id, [_, _, b'd', b'c' | b'b'])
    }
}

struct Walk<'a, 'b, 'o, S: BlockSource + ?Sized> {
    r: &'a mut Reader<'b, S>,
    pieces: Pieces,
    join: Option<&'o fragments::Options>,
    width: u32,
    height: u32,
    has_index: bool,
    /// Logical offset of the `movi` list's type field (index offsets count from there).
    movi: Option<u64>,
    /// Logical offset and size of the `idx1` data.
    idx1: Option<(u64, u64)>,
    /// Per stream (in header order): its audio format, if it is an audio stream.
    streams: Vec<Option<Audio>>,
    /// Logical offset of the last good header: everything before it holds together (the
    /// data of the chunk it starts may already be something else).
    confirmed: u64,
}

/// An audio stream's sample layout (from WAVEFORMATEX).
#[derive(Debug, Clone, Copy)]
struct Audio {
    tag: u16,
    block_align: u16,
    bits: u16,
}

impl Audio {
    /// Whether the samples are levels a waveform runs through (PCM, mu-law).
    fn is_pcm(self) -> bool {
        matches!((self.tag, self.bits), (1, 8 | 16) | (7, 8))
    }

    /// The first channel's samples in `data`, as comparable levels.
    fn samples(self, data: &[u8]) -> Vec<i32> {
        let step = usize::from(self.block_align.max(1));
        match (self.tag, self.bits) {
            (1, 16) => data
                .chunks(step)
                .filter_map(|s| Some(i32::from(i16::from_le_bytes([*s.first()?, *s.get(1)?]))))
                .collect(),
            (1, 8) => data
                .chunks(step)
                .filter_map(|s| s.first().map(|&b| i32::from(b) << 8))
                .collect(),
            (7, 8) => data
                .chunks(step)
                .filter_map(|s| s.first().map(|&b| mu_law(b)))
                .collect(),
            _ => data.iter().map(|&b| i32::from(b)).collect(),
        }
    }
}

/// G.711 mu-law expansion of one code to a 14-bit level.
fn mu_law(code: u8) -> i32 {
    let u = !code;
    let exponent = (u >> 4) & 0x07;
    let mantissa = i32::from(u & 0x0F);
    let magnitude = (((mantissa << 3) + 0x84) << exponent) - 0x84;
    if u & 0x80 != 0 { -magnitude } else { magnitude }
}

impl<S: BlockSource + ?Sized> Walk<'_, '_, '_, S> {
    fn u32(&mut self, at: u64) -> Option<u32> {
        let mut b = [0u8; 4];
        self.pieces.read(self.r, at, &mut b)?;
        Some(u32::from_le_bytes(b))
    }

    fn header(&mut self, pieces: &Pieces, pos: u64) -> Option<Header> {
        let mut head = [0u8; 12];
        let list = if pieces.read(self.r, pos, &mut head).is_some() {
            true
        } else {
            pieces.read(self.r, pos, head.get_mut(..8)?)?;
            false
        };
        let [i0, i1, i2, i3, s0, s1, s2, s3, l0, l1, l2, l3] = head;
        Some(Header {
            id: [i0, i1, i2, i3],
            size: u64::from(u32::from_le_bytes([s0, s1, s2, s3])),
            list: list.then_some([l0, l1, l2, l3]),
        })
    }

    /// Walks the chunks in `pos..end`; the offset reached if they fill it (or, without a
    /// known end, where they stop).
    fn chunks(&mut self, mut pos: u64, end: u64, level: Level) -> Result<u64, Stop> {
        // The last chunk walked: (offset, header). A fragment ending inside its data shows
        // up as a bad header right after it.
        let mut last: Option<(u64, Header)> = None;
        while pos + 8 <= end {
            let pieces = self.pieces.clone();
            let Some(h) = self.header(&pieces, pos) else {
                return Err(Stop::Ran(pos));
            };
            if !h.fits(pos, end, level) {
                if self.join_at(last.as_ref(), pos, end, level) {
                    continue;
                }
                return Err(Stop::Bad(pos));
            }
            self.confirmed = pos;
            let next = h.next(pos);
            match (&h.id, h.list) {
                (b"LIST", Some(kind)) => {
                    if kind == *b"movi" {
                        self.movi = Some(pos + 8);
                    }
                    let inner = match &kind {
                        b"movi" | b"rec " => Level::Movi,
                        _ => Level::List,
                    };
                    self.chunks(pos + 12, next.min(end), inner)?;
                }
                (b"avih", _) if h.size >= 40 => {
                    self.width = self.u32(pos + 8 + 32).unwrap_or(0);
                    self.height = self.u32(pos + 8 + 36).unwrap_or(0);
                }
                (b"idx1", _) => {
                    self.has_index = true;
                    self.idx1 = Some((pos + 8, h.size));
                }
                (b"indx", _) => self.has_index = true,
                (b"strh", _) => {
                    let mut kind = [0u8; 4];
                    let audio =
                        self.pieces.read(self.r, pos + 8, &mut kind).is_some() && kind == *b"auds";
                    self.streams.push(audio.then_some(Audio {
                        tag: 0,
                        block_align: 1,
                        bits: 8,
                    }));
                }
                (b"strf", _) => {
                    let mut f = [0u8; 16];
                    if let Some(Some(a)) = self.streams.last_mut()
                        && self.pieces.read(self.r, pos + 8, &mut f).is_some()
                    {
                        let [t0, t1, _, _, _, _, _, _, _, _, _, _, b0, b1, w0, w1] = f;
                        *a = Audio {
                            tag: u16::from_le_bytes([t0, t1]),
                            block_align: u16::from_le_bytes([b0, b1]),
                            bits: u16::from_le_bytes([w0, w1]),
                        };
                    }
                }
                _ => {}
            }
            last = Some((pos, h));
            pos = next;
        }
        Ok(pos.min(end))
    }

    /// After a bad header at logical offset `bad` (in a list ending at `end`), looks for the
    /// fragment the file goes on in and takes it if found.
    fn join_at(&mut self, last: Option<&(u64, Header)>, bad: u64, end: u64, level: Level) -> bool {
        let Some(o) = self.join else {
            return false;
        };
        if self.pieces.count() >= MAX_FRAGMENTS {
            return false;
        }
        let grid = o.grid.max(512);
        // The cut lies on the grid (fragments are whole clusters from the file start),
        // after the last good header and no later than the bad one.
        let low = last
            .map_or(bad, |(at, _)| at + 8)
            .max(self.pieces.open_from() + 1);
        let cuts: Vec<u64> = (low.div_ceil(grid)..=bad / grid)
            .map(|k| k * grid)
            .collect();
        let Some(&latest) = cuts.last() else {
            return false;
        };
        // A frame cut in two decodes only with the right second part.
        let frame = last.filter(|(at, h)| h.is_frame() && h.size > 0 && self.is_jpeg(at + 8));
        let base = self.pieces.clone();
        let from = base.physical(bad);
        for k in 1..=JOIN_REACH.max(o.reach) / grid {
            for shift in [Some(k * grid), None] {
                let moved = |p: u64| match shift {
                    Some(d) => p.checked_add(d),
                    None => p.checked_sub(k * grid),
                };
                // Cheap first: a header where the bad one should be.
                let Some(head_at) = moved(from) else { continue };
                if !self.quick_header(head_at, level) {
                    continue;
                }
                let to = |c: u64| moved(base.physical(c));
                let Some(to_latest) = to(latest) else {
                    continue;
                };
                if base.overlaps(to_latest, bad) {
                    continue;
                }
                let cand = base.cut(latest, to_latest);
                let (count, ended) = self.chain(&cand, bad, end, level);
                if count == 0 {
                    continue;
                }
                let chosen = match frame {
                    Some((at, h)) => cuts.iter().copied().find(|&c| {
                        to(c).is_some_and(|t| self.decodes(&base.cut(c, t), at + 8, h.size))
                    }),
                    // Audio cut in two: its waveform must run on across the seam.
                    // The cut where it runs on most smoothly is taken.
                    None if let Some((at, h)) = last
                        && let Some(a) = self.audio_of(&h.id).filter(|a| a.is_pcm())
                        && let Some(c) = {
                            let span = (at + 8)..(at + 8 + h.size);
                            let mut best: Option<(f64, u64)> = None;
                            for &c in &cuts {
                                let seam =
                                    to(c).and_then(|t| self.seam(&base.cut(c, t), &span, &cuts, a));
                                if let Some(v) = seam
                                    && v <= SEAM_LIMIT
                                    && best.is_none_or(|(b, _)| v < b)
                                {
                                    best = Some((v, c));
                                }
                            }
                            best.map(|(_, c)| c)
                        } =>
                    {
                        Some(c)
                    }
                    None if ended || count >= CHAIN => match last {
                        Some((at, h)) if cuts.len() > 1 => {
                            let audio = self.audio_of(&h.id);
                            let span = (at + 8)..(at + 8 + h.size);
                            let rough = |w: &mut Self, c: u64| {
                                to(c)
                                    .and_then(|t| w.roughness(&base.cut(c, t), &span, &cuts, audio))
                            };
                            let mut best = None;
                            for &c in &cuts {
                                if let Some(v) = rough(self, c)
                                    && best.is_none_or(|(b, _)| v < b)
                                {
                                    best = Some((v, c));
                                }
                            }
                            best.map(|(_, c)| c).or(Some(latest))
                        }
                        _ => Some(latest),
                    },
                    None => None,
                };
                if let Some(c) = chosen
                    && let Some(t) = to(c)
                {
                    self.pieces = base.cut(c, t);
                    return true;
                }
            }
        }
        false
    }

    /// Whether the data at logical offset `at` starts a JPEG.
    fn is_jpeg(&mut self, at: u64) -> bool {
        let mut soi = [0u8; 2];
        let pieces = self.pieces.clone();
        pieces.read(self.r, at, &mut soi).is_some() && soi == [0xFF, 0xD8]
    }

    /// The audio format of the stream a chunk id names (`##wb`).
    fn audio_of(&self, id: &[u8; 4]) -> Option<Audio> {
        let digits = std::str::from_utf8(id.get(..2)?).ok()?;
        let n = usize::from_str_radix(digits, 16).ok()?;
        self.streams.get(n).copied().flatten()
    }

    /// How much the waveform (or the bytes, for other data) jumps around the cuts in the
    /// chunk data `span` read through `pieces`: lower where the cut is right.
    fn roughness(
        &mut self,
        pieces: &Pieces,
        span: &std::ops::Range<u64>,
        cuts: &[u64],
        audio: Option<Audio>,
    ) -> Option<u64> {
        let from = cuts.first()?.saturating_sub(SEAM_WINDOW).max(span.start);
        let to = cuts.last()?.saturating_add(SEAM_WINDOW).min(span.end);
        let mut data = vec![0u8; usize::try_from(to.checked_sub(from)?).ok()?];
        pieces.read(self.r, from, &mut data)?;
        let levels = match audio {
            Some(a) => a.samples(&data),
            None => data.iter().map(|&b| i32::from(b)).collect(),
        };
        Some(
            levels
                .iter()
                .zip(levels.iter().skip(1))
                .map(|(a, b)| u64::from(a.abs_diff(*b)))
                .sum(),
        )
    }

    /// How the waveform of the audio chunk data `span` jumps around the cuts when read
    /// through `pieces`: the largest sample-to-sample change from just before the first
    /// cut to just after the last, over the largest change before the first cut (data that
    /// is surely the file's own). At most about 1 where the cut is right; data from
    /// elsewhere brings a jump at the seam or a stretch that is not a waveform.
    fn seam(
        &mut self,
        pieces: &Pieces,
        span: &std::ops::Range<u64>,
        cuts: &[u64],
        a: Audio,
    ) -> Option<f64> {
        let align = u64::from(a.block_align.max(1));
        let first = *cuts.first()?;
        let last = *cuts.last()?;
        // Whole samples counted from the chunk's start.
        let from = span.start
            + (first.saturating_sub(SEAM_WINDOW).max(span.start) - span.start) / align * align;
        let to = last.saturating_add(SEAM_WINDOW).min(span.end);
        if first < from + 16 * align || to <= last {
            return None;
        }
        let mut data = vec![0u8; usize::try_from(to - from).ok()?];
        pieces.read(self.r, from, &mut data)?;
        let levels = a.samples(&data);
        let deltas: Vec<u64> = levels
            .iter()
            .zip(levels.iter().skip(1))
            .map(|(x, y)| u64::from(x.abs_diff(*y)))
            .collect();
        // Deltas wholly before the first cut.
        let sure = usize::try_from((first - from) / align)
            .ok()?
            .saturating_sub(1);
        let usual = deltas.get(..sure)?.iter().max().copied()?.max(1);
        let worst = deltas.iter().max().copied()?;
        Some(worst as f64 / usual as f64)
    }

    /// Whether a chunk header that could stand at `level` is at source offset `at`.
    fn quick_header(&mut self, at: u64, level: Level) -> bool {
        self.r
            .slice(at, 4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .is_some_and(|id| valid_id(&id, level))
    }

    /// Chunk headers that follow on from `pos` through `pieces` (up to [`CHAIN`]), and
    /// whether they reach the end of the list.
    fn chain(&mut self, pieces: &Pieces, mut pos: u64, end: u64, level: Level) -> (usize, bool) {
        let mut count = 0;
        while count < CHAIN {
            if pos + 8 > end {
                return (count, true);
            }
            let Some(h) = self.header(pieces, pos) else {
                return (count, false);
            };
            if !h.fits(pos, end, level) {
                return (count, false);
            }
            count += 1;
            pos = h.next(pos);
        }
        (count, false)
    }

    /// Whether the JPEG frame of `size` bytes at logical offset `at` decodes completely
    /// through `pieces`.
    fn decodes(&mut self, pieces: &Pieces, at: u64, size: u64) -> bool {
        let Ok(n) = usize::try_from(size) else {
            return false;
        };
        let mut frame = vec![0u8; n];
        if pieces.read(self.r, at, &mut frame).is_none() || !frame.starts_with(&[0xFF, 0xD8]) {
            return false;
        }
        // The JPEG carver looks a little past EOI (for appended parts).
        frame.resize(n + 4096, 0);
        let src = MemSource::new(frame);
        let mut r = Reader::new(&src);
        // Frames may carry a few bytes of padding after EOI.
        jpeg::carve(&mut r, 0, size).is_some_and(|j| {
            j.status == jpeg::Status::Complete && size - j.primary_len.min(size) <= 64
        })
    }

    /// After joining: the logical offset of the first chunk that does not match the index
    /// (`idx1`), if any. Some writers' indexes are off (sizes without padding, or offsets
    /// that miss the chunks altogether), so the index counts only if it matches the chunks
    /// in the first fragment, which needed no joining; and only chunk ids are compared.
    fn index_mismatch(&mut self) -> Option<u64> {
        let (at, size) = self.idx1?;
        let movi = self.movi?;
        let pieces = self.pieces.clone();
        let first = pieces.fragments(u64::MAX).first().map_or(0, |f| f.1);
        let entries = (size / 16).min(MAX_INDEX_CHECKS);
        // Offsets count from the `movi` type field, or (in some writers) from the file start.
        let mut base = None;
        for i in 0..entries {
            let mut e = [0u8; 16];
            pieces.read(self.r, at + i * 16, &mut e)?;
            let [i0, i1, i2, i3, _, _, _, _, o0, o1, o2, o3, s0, s1, s2, s3] = e;
            let offset = u64::from(u32::from_le_bytes([o0, o1, o2, o3]));
            let len = u64::from(u32::from_le_bytes([s0, s1, s2, s3]));
            let base = *base.get_or_insert(if offset < movi { movi } else { 0 });
            let chunk = base + offset;
            let id = [i0, i1, i2, i3];
            // Writers mark unused entries in odd ways (Canon's `7Fxx`); only stream chunks
            // are compared.
            if !valid_id(&id, Level::Movi) || len == 0 {
                continue;
            }
            if self.header(&pieces, chunk).is_none_or(|h| h.id != id) {
                return (chunk >= first).then_some(chunk);
            }
        }
        None
    }
}

/// Whether `id` can name a chunk at this level: four characters from the set RIFF uses
/// (letters, digits, space, underscore); in `movi`, a stream chunk (`##dc`, `##db`, `##wb`,
/// `##pc`, `##tx`), an OpenDML index (`ix##`), padding (`JUNK`) or a list.
fn valid_id(id: &[u8; 4], level: Level) -> bool {
    let plain = id
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || *c == b' ' || *c == b'_');
    if !plain {
        return false;
    }
    match level {
        Level::Movi => {
            let [a, b, c, d] = *id;
            let stream = a.is_ascii_hexdigit()
                && b.is_ascii_hexdigit()
                && matches!(&[c, d], b"dc" | b"db" | b"wb" | b"pc" | b"tx");
            let index = id.starts_with(b"ix");
            stream || index || id == b"JUNK" || id == b"LIST"
        }
        Level::Top | Level::List => true,
    }
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend(u32::try_from(data.len()).unwrap().to_le_bytes());
        v.extend(data);
        if data.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        chunk(b"LIST", &[&kind[..], body].concat())
    }

    /// A small camera-style AVI with `frames` frames of odd sizes.
    fn avi(frames: usize) -> Vec<u8> {
        let mut avih = vec![0u8; 56];
        avih[32..36].copy_from_slice(&320u32.to_le_bytes());
        avih[36..40].copy_from_slice(&240u32.to_le_bytes());
        let hdrl = list(
            b"hdrl",
            &[
                chunk(b"avih", &avih),
                list(b"strl", &chunk(b"strh", &[0; 56])),
            ]
            .concat(),
        );
        let mut movi = Vec::new();
        for i in 0..frames {
            let mut f = vec![0xFF, 0xD8];
            f.resize(1000 + i * 7, u8::try_from(i % 256).unwrap());
            movi.extend(chunk(b"00dc", &f));
            if i % 4 == 0 {
                movi.extend(chunk(b"01wb", &[0x80; 301]));
            }
        }
        let body = [
            &b"AVI "[..],
            &hdrl,
            &list(b"movi", &movi),
            &chunk(b"idx1", &[0; 32]),
        ]
        .concat();
        chunk(b"RIFF", &body)
    }

    fn run(data: Vec<u8>) -> Option<Avi> {
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        carve(&mut r, 0, 1 << 30)
    }

    #[test]
    fn a_whole_file_is_complete() {
        let a = avi(40);
        let got = run([a.clone(), vec![0x11; 3000]].concat()).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(got.len, a.len() as u64);
        assert_eq!((got.width, got.height, got.has_index), (320, 240, true));
    }

    #[test]
    fn foreign_data_inside_movi_is_found() {
        let mut a = avi(40);
        let at = 20_000;
        a[at..at + 4096].fill(0x5A);
        let got = run(a).unwrap();
        let Status::Corrupt { at: bad } = got.status else {
            panic!("{got:?}");
        };
        assert!(bad >= at as u64 && bad < at as u64 + 4096 + 2000, "{bad}");
        // Only what a following header confirms is kept.
        assert!(got.len > 0 && got.len < at as u64, "{}", got.len);
    }

    #[test]
    fn a_cut_off_file_is_truncated() {
        let a = avi(40);
        let got = run(a[..30_000].to_vec()).unwrap();
        assert_eq!(got.status, Status::Truncated);
    }

    #[test]
    fn opendml_continuation_is_followed() {
        let a = avi(10);
        let movi = list(b"movi", &chunk(b"00dc", &[1; 999]));
        let avix = chunk(b"RIFF", &[&b"AVIX"[..], &movi].concat());
        let got = run([a.clone(), avix.clone(), vec![0; 100]].concat()).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(got.len, (a.len() + avix.len()) as u64);
    }

    #[test]
    fn not_avi() {
        assert!(run(b"RIFF\x10\0\0\0WAVEfmt ".to_vec()).is_none());
        assert!(run(vec![0; 64]).is_none());
    }
}
