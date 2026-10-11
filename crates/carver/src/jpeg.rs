//! Structure-aware JPEG carving.
//!
//! Header segments are skipped by their lengths, so an EXIF thumbnail (which carries its
//! own SOI/EOI) never ends the file early. For baseline JPEGs the entropy-coded data is
//! Huffman-decoded block by block: the file is complete only if every MCU decodes and
//! the restart markers come in order, and the first byte that breaks decoding is reported
//! as a likely fragment boundary. Other JPEGs (progressive, lossless, arithmetic) fall back
//! to following markers.
//!
//! Reference: ITU-T T.81 (ISO/IEC 10918-1) Annex B (syntax), Annex F.2.2 (baseline Huffman
//! decoding: F.2.2.3 DECODE, F.2.2.4 RECEIVE), Annex C (Huffman table specification).

use blocksource::BlockSource;

use crate::reader::Reader;

/// A JPEG found at some offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jpeg {
    /// Bytes from SOI through EOI if complete; up to the failure point otherwise.
    pub len: u64,
    pub status: Status,
    pub width: u16,
    pub height: u16,
    pub progressive: bool,
    /// Whether the entropy-coded data was fully Huffman-decoded (baseline only).
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// SOI through EOI, structurally valid.
    Complete,
    /// Decoding broke at this source offset (often where a fragment ends).
    Corrupt { at: u64 },
    /// The data ran out (end of the source, unreadable, or `max_len` reached).
    Truncated,
}

/// Tries to carve a JPEG starting at `start`. Returns `None` if the bytes there are not a
/// plausible JPEG header.
pub fn carve<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Jpeg> {
    carve_with_plan(r, start, max_len).map(|(j, _)| j)
}

/// Like [`carve`], also returning the scan plan of a single-scan baseline JPEG (used to
/// resume decoding in other places when looking for the next fragment).
pub(crate) fn carve_with_plan<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    start: u64,
    max_len: u64,
) -> Option<(Jpeg, Option<Plan>)> {
    if r.slice(start, 3)? != [0xFF, 0xD8, 0xFF] {
        return None;
    }
    let mut p = Parser {
        r,
        start,
        limit: start.saturating_add(max_len),
        tables: Default::default(),
        frame: None,
        restart: 0,
        progressive: false,
        verified: true,
        segments: 0,
        scans: 0,
        plan: None,
    };
    let (end, status) = p.run();
    if p.segments == 0 {
        return None;
    }
    let (width, height) = p.frame.as_ref().map_or((0, 0), |f| (f.width, f.height));
    let jpeg = Jpeg {
        len: end - start,
        status,
        width,
        height,
        progressive: p.progressive,
        verified: p.verified && status == Status::Complete,
    };
    // Only a single baseline scan can be resumed elsewhere.
    let plan = if p.scans == 1 && !p.progressive {
        p.plan
    } else {
        None
    };
    Some((jpeg, plan))
}

struct Component {
    id: u8,
    h: u8,
    v: u8,
}

struct Frame {
    width: u16,
    height: u16,
    components: Vec<Component>,
    hmax: u8,
    vmax: u8,
}

/// A Huffman decoding table in the form of T.81 F.2.2.3 (MAXCODE, VALPTR, MINCODE).
#[derive(Clone)]
struct Huffman {
    maxcode: [i32; 17],
    valptr: [i32; 17],
    mincode: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    /// Builds the decoding table from BITS (counts per length) and HUFFVAL (C.2, F.15).
    fn new(counts: &[u8; 16], values: Vec<u8>) -> Option<Self> {
        let mut t = Self {
            maxcode: [-1; 17],
            valptr: [0; 17],
            mincode: [0; 17],
            values,
        };
        let mut code: i32 = 0;
        let mut k: i32 = 0;
        for (i, &n) in counts.iter().enumerate() {
            let len = i + 1;
            if n > 0 {
                *t.valptr.get_mut(len)? = k;
                *t.mincode.get_mut(len)? = code;
                code += i32::from(n);
                k += i32::from(n);
                *t.maxcode.get_mut(len)? = code - 1;
                // A code of this length must fit in `len` bits.
                if code > (1 << len) {
                    return None;
                }
            }
            code <<= 1;
        }
        (k as usize <= t.values.len()).then_some(t)
    }
}

/// Huffman tables by class (0 = DC, 1 = AC) and id (0..=3).
type Tables = [[Option<Huffman>; 4]; 2];

struct Parser<'r, 'a, S: BlockSource + ?Sized> {
    r: &'r mut Reader<'a, S>,
    start: u64,
    limit: u64,
    tables: Tables,
    frame: Option<Frame>,
    restart: u16,
    progressive: bool,
    /// False once any scan was only followed by markers, not decoded.
    verified: bool,
    /// Header segments seen (zero means this was not a JPEG at all).
    segments: u32,
    /// Scans started.
    scans: u32,
    /// The plan of the last decoded scan.
    plan: Option<Plan>,
}

/// Why decoding stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Bad data at this offset.
    Corrupt(u64),
    /// Ran out of data.
    Truncated,
}

impl<S: BlockSource + ?Sized> Parser<'_, '_, S> {
    /// Walks the file from just after SOI. Returns the end offset and status.
    fn run(&mut self) -> (u64, Status) {
        let mut pos = self.start + 2;
        loop {
            if pos >= self.limit {
                return (self.limit.min(pos), Status::Truncated);
            }
            match self.segment(pos) {
                Ok(Next::Continue(next)) => pos = next,
                Ok(Next::End(end)) => return (end, Status::Complete),
                Err(Stop::Corrupt(at)) => return (at, Status::Corrupt { at }),
                Err(Stop::Truncated) => return (pos, Status::Truncated),
            }
        }
    }

    /// Handles the marker segment at `pos` (B.1.1.4: markers may be preceded by fill bytes).
    fn segment(&mut self, mut pos: u64) -> Result<Next, Stop> {
        if self.byte(pos)? != 0xFF {
            return Err(Stop::Corrupt(pos));
        }
        while self.byte(pos + 1)? == 0xFF {
            pos += 1;
        }
        let marker = self.byte(pos + 1)?;
        match marker {
            0xD9 if self.segments > 0 => return Ok(Next::End(pos + 2)),
            // Markers without a length (TEM, RSTn) are not valid here before a scan.
            0x01 | 0xD0..=0xD7 | 0xD8 | 0xD9 | 0x00 => return Err(Stop::Corrupt(pos)),
            _ => {}
        }
        if !matches!(marker, 0xC0..=0xCF | 0xDA..=0xDF | 0xE0..=0xEF | 0xFE) {
            return Err(Stop::Corrupt(pos));
        }
        let len = u64::from(self.r.u16_be(pos + 2).ok_or(Stop::Truncated)?);
        if len < 2 {
            return Err(Stop::Corrupt(pos));
        }
        let body = pos + 4;
        let next = pos + 2 + len;
        if next > self.limit {
            return Err(Stop::Truncated);
        }
        self.segments += 1;
        match marker {
            0xC4 => self.dht(body, len - 2).ok_or(Stop::Corrupt(pos))?,
            0xC0 | 0xC1 => self.sof(body).ok_or(Stop::Corrupt(pos))?,
            0xC2 => {
                self.progressive = true;
                self.sof(body).ok_or(Stop::Corrupt(pos))?;
            }
            0xDD => self.restart = self.r.u16_be(body).ok_or(Stop::Truncated)?,
            0xDA => return self.scan(body, next).map(Next::Continue),
            _ => {}
        }
        Ok(Next::Continue(next))
    }

    /// DHT (B.2.4.2): one or more tables.
    fn dht(&mut self, mut at: u64, len: u64) -> Option<()> {
        let end = at + len;
        while at < end {
            let tc_th = self.r.byte(at)?;
            let counts: [u8; 16] = self.r.slice(at + 1, 16)?.try_into().ok()?;
            let n: usize = counts.iter().map(|&c| usize::from(c)).sum();
            let values = self.r.slice(at + 17, n)?.to_vec();
            let class = usize::from(tc_th >> 4);
            let id = usize::from(tc_th & 0x0F);
            *self.tables.get_mut(class)?.get_mut(id)? = Some(Huffman::new(&counts, values)?);
            at += 17 + n as u64;
        }
        (at == end).then_some(())
    }

    /// SOF (B.2.2): dimensions and sampling factors.
    fn sof(&mut self, at: u64) -> Option<()> {
        let height = self.r.u16_be(at + 1)?;
        let width = self.r.u16_be(at + 3)?;
        let n = usize::from(self.r.byte(at + 5)?);
        if width == 0 || n == 0 || n > 4 {
            return None;
        }
        let mut components = Vec::with_capacity(n);
        for i in 0..n as u64 {
            let c = self.r.slice(at + 6 + 3 * i, 3)?;
            let (id, hv) = (*c.first()?, *c.get(1)?);
            let (h, v) = (hv >> 4, hv & 0x0F);
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                return None;
            }
            components.push(Component { id, h, v });
        }
        let hmax = components.iter().map(|c| c.h).max()?;
        let vmax = components.iter().map(|c| c.v).max()?;
        self.frame = Some(Frame {
            width,
            height,
            components,
            hmax,
            vmax,
        });
        Some(())
    }

    /// SOS (B.2.3) followed by entropy-coded data. Returns where the next marker starts.
    fn scan(&mut self, at: u64, data: u64) -> Result<u64, Stop> {
        let corrupt = Stop::Corrupt(at);
        let ns = usize::from(self.byte(at)?);
        let mut comps = Vec::with_capacity(ns);
        for i in 0..ns as u64 {
            let cs = self.byte(at + 1 + 2 * i)?;
            let t = self.byte(at + 2 + 2 * i)?;
            comps.push((cs, usize::from(t >> 4), usize::from(t & 0x0F)));
        }
        if self.progressive || ns == 0 {
            self.verified = false;
            return self.follow_markers(data);
        }
        let Some(frame) = &self.frame else {
            return Err(corrupt);
        };
        // Per scan component: (blocks per MCU in x, in y, DC table, AC table).
        let mut layout = Vec::with_capacity(ns);
        for (cs, td, ta) in &comps {
            let c = frame
                .components
                .iter()
                .find(|c| c.id == *cs)
                .ok_or(Stop::Corrupt(at))?;
            let dc = self
                .tables
                .first()
                .and_then(|t| t.get(*td))
                .and_then(Option::as_ref);
            let ac = self
                .tables
                .get(1)
                .and_then(|t| t.get(*ta))
                .and_then(Option::as_ref);
            let (Some(dc), Some(ac)) = (dc, ac) else {
                return Err(Stop::Corrupt(at));
            };
            layout.push((c.h, c.v, dc.clone(), ac.clone()));
        }
        let (mcus, blocks_per_mcu): (u64, Vec<u32>) = if ns == 1 {
            // A non-interleaved scan: one block per MCU over the component's own grid.
            let c = frame
                .components
                .iter()
                .find(|c| Some(c.id) == comps.first().map(|x| x.0));
            let (h, v) = c.map_or((1, 1), |c| (u32::from(c.h), u32::from(c.v)));
            let w = (u32::from(frame.width) * h).div_ceil(u32::from(frame.hmax));
            let hgt = (u32::from(frame.height) * v).div_ceil(u32::from(frame.vmax));
            (
                u64::from(w.div_ceil(8)) * u64::from(hgt.div_ceil(8)),
                vec![1],
            )
        } else {
            let mx = u32::from(frame.width).div_ceil(8 * u32::from(frame.hmax));
            let my = u32::from(frame.height).div_ceil(8 * u32::from(frame.vmax));
            let per = layout
                .iter()
                .map(|l| u32::from(l.0) * u32::from(l.1))
                .collect();
            (u64::from(mx) * u64::from(my), per)
        };

        let plan = Plan {
            layout: layout
                .into_iter()
                .zip(blocks_per_mcu)
                .map(|((_, _, dc, ac), n)| (n, dc, ac))
                .collect(),
            mcus,
            restart: self.restart,
            data,
        };
        self.scans += 1;
        let outcome = decode(&plan, self.r, Snapshot::start(data), self.limit, None, None);
        self.plan = Some(plan);
        match outcome {
            Outcome::Done(pos) => Ok(pos),
            Outcome::Fail(stop) => Err(stop),
            Outcome::Capped => Err(Stop::Truncated),
        }
    }

    /// Skips entropy-coded data by markers only (for scans we do not decode). Returns where
    /// the next real marker starts.
    fn follow_markers(&mut self, mut pos: u64) -> Result<u64, Stop> {
        loop {
            if pos >= self.limit {
                return Err(Stop::Truncated);
            }
            if self.byte(pos)? == 0xFF {
                match self.byte(pos + 1)? {
                    0x00 | 0xD0..=0xD7 | 0xFF => pos += 1,
                    _ => return Ok(pos),
                }
            }
            pos += 1;
        }
    }

    fn byte(&mut self, pos: u64) -> Result<u8, Stop> {
        self.r.byte(pos).ok_or(Stop::Truncated)
    }
}

enum Next {
    Continue(u64),
    End(u64),
}

/// A source of bytes by offset: the reader itself, or several fragments spliced together.
pub(crate) trait Stream {
    fn byte(&mut self, pos: u64) -> Option<u8>;
}

impl<S: BlockSource + ?Sized> Stream for Reader<'_, S> {
    fn byte(&mut self, pos: u64) -> Option<u8> {
        Reader::byte(self, pos)
    }
}

/// What decoding one baseline scan needs.
#[derive(Clone)]
pub(crate) struct Plan {
    /// Per scan component: (blocks per MCU, DC table, AC table).
    layout: Vec<(u32, Huffman, Huffman)>,
    pub mcus: u64,
    restart: u16,
    /// Offset of the first entropy-coded byte.
    pub data: u64,
}

/// Decoder state at the start of an MCU, enough to resume from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Offset of the next byte to load; every byte before it has been consumed.
    pub pos: u64,
    acc: u32,
    count: u32,
    pub mcu: u64,
    rst: u8,
}

impl Plan {
    /// Whether the scan has restart markers (a strong check when splicing fragments).
    pub fn has_restart(&self) -> bool {
        self.restart > 0
    }
}

impl Snapshot {
    pub fn start(data: u64) -> Self {
        Self {
            pos: data,
            acc: 0,
            count: 0,
            mcu: 0,
            rst: 0,
        }
    }
}

pub(crate) enum Outcome {
    /// Every MCU decoded; the scan ends with a marker at this offset.
    Done(u64),
    /// Decoding reached `cap` without errors.
    Capped,
    /// Decoding stopped.
    Fail(Stop),
}

/// Decodes `plan`'s MCUs from `from` until the end, an error, or `cap` (an offset).
/// With `snaps`, records the state at every MCU start.
pub(crate) fn decode<T: Stream + ?Sized>(
    plan: &Plan,
    s: &mut T,
    from: Snapshot,
    limit: u64,
    cap: Option<u64>,
    mut snaps: Option<&mut Vec<Snapshot>>,
) -> Outcome {
    let mut bits = Bits {
        pos: from.pos,
        acc: from.acc,
        count: from.count,
        limit,
    };
    let mut rst = from.rst;
    for mcu in from.mcu..plan.mcus {
        let snap = Snapshot {
            pos: bits.pos,
            acc: bits.acc,
            count: bits.count,
            mcu,
            rst,
        };
        if cap.is_some_and(|c| bits.pos >= c) {
            return Outcome::Capped;
        }
        if let Some(v) = snaps.as_deref_mut() {
            v.push(snap);
        }
        let step = (|| {
            if plan.restart > 0 && mcu > 0 && mcu % u64::from(plan.restart) == 0 {
                bits.restart(s, rst)?;
                rst = (rst + 1) % 8;
            }
            for (n, dc, ac) in &plan.layout {
                for _ in 0..*n {
                    block(s, &mut bits, dc, ac)?;
                }
            }
            Ok(())
        })();
        if let Err(stop) = step {
            return Outcome::Fail(stop);
        }
    }
    // After the last MCU the scan ends at the next marker, possibly after fill bytes.
    let mut pos = bits.pos;
    loop {
        match (s.byte(pos), s.byte(pos + 1)) {
            (Some(0xFF), Some(0xFF)) => pos += 1,
            (Some(0xFF), Some(_)) => return Outcome::Done(pos),
            (Some(_), _) => return Outcome::Fail(Stop::Corrupt(pos)),
            (None, _) => return Outcome::Fail(Stop::Truncated),
        }
    }
}

/// Bit reader over entropy-coded data with byte stuffing (F.1.2.3: `FF 00` is `FF`).
struct Bits {
    /// Offset of the next byte to load.
    pos: u64,
    acc: u32,
    count: u32,
    limit: u64,
}

impl Bits {
    fn bit<T: Stream + ?Sized>(&mut self, s: &mut T) -> Result<u32, Stop> {
        if self.count == 0 {
            if self.pos >= self.limit {
                return Err(Stop::Truncated);
            }
            let b = s.byte(self.pos).ok_or(Stop::Truncated)?;
            if b == 0xFF {
                // Any marker inside an MCU means the data is broken (or a fragment ended).
                if s.byte(self.pos + 1).ok_or(Stop::Truncated)? != 0x00 {
                    return Err(Stop::Corrupt(self.pos));
                }
                self.pos += 2;
            } else {
                self.pos += 1;
            }
            self.acc = u32::from(b);
            self.count = 8;
        }
        self.count -= 1;
        Ok((self.acc >> self.count) & 1)
    }

    fn skip<T: Stream + ?Sized>(&mut self, s: &mut T, n: u32) -> Result<(), Stop> {
        for _ in 0..n {
            self.bit(s)?;
        }
        Ok(())
    }

    /// F.2.2.3 DECODE.
    fn decode<T: Stream + ?Sized>(&mut self, s: &mut T, t: &Huffman) -> Result<u8, Stop> {
        let at = self.pos;
        let mut code = self.bit(s)? as i32;
        for len in 1..=16usize {
            let max = t.maxcode.get(len).copied().unwrap_or(-1);
            if code <= max {
                let base = t.valptr.get(len).copied().unwrap_or(0);
                let min = t.mincode.get(len).copied().unwrap_or(0);
                let idx = usize::try_from(base + code - min).map_err(|_| Stop::Corrupt(at))?;
                return t.values.get(idx).copied().ok_or(Stop::Corrupt(at));
            }
            code = (code << 1) | self.bit(s)? as i32;
        }
        Err(Stop::Corrupt(at))
    }

    /// At a restart interval: drop the remaining bits and expect `RSTn` (F.1.2.3, B.2.1).
    fn restart<T: Stream + ?Sized>(&mut self, s: &mut T, n: u8) -> Result<(), Stop> {
        self.count = 0;
        match (s.byte(self.pos), s.byte(self.pos + 1)) {
            (Some(0xFF), Some(m)) if m == 0xD0 + n => {
                self.pos += 2;
                Ok(())
            }
            (Some(_), Some(_)) => Err(Stop::Corrupt(self.pos)),
            _ => Err(Stop::Truncated),
        }
    }
}

/// Decodes one 8x8 block's coefficients without reconstructing them (F.2.2.1-F.2.2.2).
fn block<T: Stream + ?Sized>(
    s: &mut T,
    bits: &mut Bits,
    dc: &Huffman,
    ac: &Huffman,
) -> Result<(), Stop> {
    let size = bits.decode(s, dc)?;
    if size > 11 {
        return Err(Stop::Corrupt(bits.pos));
    }
    bits.skip(s, u32::from(size))?;
    let mut k = 1u32;
    while k < 64 {
        let at = bits.pos;
        let rs = bits.decode(s, ac)?;
        let (run, size) = (u32::from(rs >> 4), u32::from(rs & 0x0F));
        if size == 0 {
            if run == 15 {
                k += 16; // ZRL
                continue;
            }
            break; // EOB
        }
        k += run;
        if k > 63 || size > 10 {
            return Err(Stop::Corrupt(at));
        }
        bits.skip(s, size)?;
        k += 1;
    }
    if k > 64 {
        return Err(Stop::Corrupt(bits.pos));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    /// A tiny valid baseline JPEG (8x8, grayscale, one block of all-zero coefficients).
    fn tiny() -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        // DQT: one table of 64 ones.
        j.extend([0xFF, 0xDB, 0x00, 0x43, 0x00]);
        j.extend([1u8; 64]);
        // SOF0: 8x8, 1 component, 1x1 sampling, table 0.
        j.extend([
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00,
        ]);
        // DHT DC 0: one code of length 1 -> category 0. AC 0: one code of length 1 -> EOB.
        j.extend([0xFF, 0xC4, 0x00, 0x14, 0x00, 1]);
        j.extend([0u8; 15]);
        j.push(0x00);
        j.extend([0xFF, 0xC4, 0x00, 0x14, 0x10, 1]);
        j.extend([0u8; 15]);
        j.push(0x00);
        // SOS: 1 component, tables 0/0.
        j.extend([0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        // DC code "0", AC code "0" (EOB), padded with 1s.
        j.push(0b0011_1111);
        j.extend([0xFF, 0xD9]);
        j
    }

    fn run(data: Vec<u8>) -> Option<Jpeg> {
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        carve(&mut r, 0, 1 << 20)
    }

    #[test]
    fn tiny_jpeg_is_complete_and_verified() {
        let j = tiny();
        let got = run(j.clone()).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert!(got.verified);
        assert_eq!(got.len, j.len() as u64);
        assert_eq!((got.width, got.height), (8, 8));
    }

    #[test]
    fn trailing_garbage_is_not_included() {
        let mut j = tiny();
        let len = j.len() as u64;
        j.extend([0x12; 100]);
        assert_eq!(run(j).unwrap().len, len);
    }

    /// An embedded thumbnail inside an APP1 segment does not end the file.
    #[test]
    fn thumbnail_in_app1_is_skipped() {
        let thumb = tiny();
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
        j.extend(u16::try_from(thumb.len() + 2).unwrap().to_be_bytes());
        j.extend(&thumb);
        j.extend(&tiny()[2..]);
        let got = run(j.clone()).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(got.len, j.len() as u64);
    }

    #[test]
    fn broken_entropy_data_is_corrupt() {
        let mut j = tiny();
        let n = j.len();
        // Replace the scan byte with a marker that cannot appear inside an MCU.
        j[n - 3] = 0xFF;
        j.insert(n - 2, 0xC8);
        assert!(matches!(run(j).unwrap().status, Status::Corrupt { .. }));
    }

    #[test]
    fn not_a_jpeg() {
        assert!(run(vec![0x00; 64]).is_none());
        assert!(run(vec![0xFF, 0xD8, 0xFF, 0x00, 0x00]).is_none());
    }
}
