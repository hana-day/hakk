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
        quant_dc: [1; 4],
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
    /// Quantization table id.
    tq: u8,
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
    /// Lookahead on the next `LOOKAHEAD` bits: `(code length, symbol)`, length 0 if the
    /// code is longer.
    fast: Vec<(u8, u8)>,
}

/// Bits decoded by one table lookup.
const LOOKAHEAD: u32 = 9;

impl Huffman {
    /// Builds the decoding table from BITS (counts per length) and HUFFVAL (C.2, F.15).
    // Code lengths are at most 16, so the casts below cannot truncate.
    #[allow(clippy::cast_possible_truncation)]
    fn new(counts: &[u8; 16], values: Vec<u8>) -> Option<Self> {
        let mut t = Self {
            maxcode: [-1; 17],
            valptr: [0; 17],
            mincode: [0; 17],
            values,
            fast: vec![(0, 0); 1 << LOOKAHEAD],
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
        if k as usize > t.values.len() {
            return None;
        }
        // Fill the lookahead: every bit pattern starting with a short code maps to it.
        let mut idx = 0usize;
        let mut code = 0u32;
        for (i, &n) in counts.iter().enumerate() {
            let len = (i + 1) as u32;
            for _ in 0..n {
                if len <= LOOKAHEAD {
                    let sym = t.values.get(idx).copied().unwrap_or(0);
                    let shift = LOOKAHEAD - len;
                    let first = (code << shift) as usize;
                    for e in t.fast.iter_mut().skip(first).take(1 << shift) {
                        *e = (len as u8, sym);
                    }
                }
                idx += 1;
                code += 1;
            }
            code <<= 1;
        }
        Some(t)
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
    /// The DC entry of each quantization table (to scale DC coefficients).
    quant_dc: [i32; 4],
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
            0xDB => self.dqt(body, len - 2).ok_or(Stop::Corrupt(pos))?,
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

    /// DQT (B.2.4.1): keeps each table's DC entry (the first value in zigzag order).
    fn dqt(&mut self, mut at: u64, len: u64) -> Option<()> {
        let end = at + len;
        while at < end {
            let pq_tq = self.r.byte(at)?;
            let wide = pq_tq >> 4 != 0;
            let dc = if wide {
                i32::from(self.r.u16_be(at + 1)?)
            } else {
                i32::from(self.r.byte(at + 1)?)
            };
            *self.quant_dc.get_mut(usize::from(pq_tq & 0x03))? = dc.max(1);
            at += 1 + 64 * if wide { 2 } else { 1 };
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
            let (id, hv, tq) = (*c.first()?, *c.get(1)?, *c.get(2)?);
            let (h, v) = (hv >> 4, hv & 0x0F);
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) {
                return None;
            }
            components.push(Component {
                id,
                h,
                v,
                tq: tq & 0x03,
            });
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
            let quant = self.quant_dc.get(usize::from(c.tq)).copied().unwrap_or(1);
            let luma = frame.components.first().is_some_and(|f| f.id == c.id);
            layout.push((c.h, c.v, dc.clone(), ac.clone(), quant, luma));
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

        let interleaved = ns > 1;
        let luma_cols = u64::from(u32::from(frame.width).div_ceil(8 * u32::from(frame.hmax)))
            * u64::from(frame.hmax);
        let mcu_cols = if interleaved {
            u64::from(u32::from(frame.width).div_ceil(8 * u32::from(frame.hmax)))
        } else {
            u64::from(u32::from(frame.width).div_ceil(8))
        };
        let plan = Plan {
            layout: layout
                .into_iter()
                .zip(blocks_per_mcu)
                .map(|((h, v, dc, ac, quant, luma), n)| ScanComp {
                    n,
                    h: u64::from(h),
                    v: u64::from(v),
                    dc,
                    ac,
                    quant,
                    luma,
                })
                .collect(),
            mcus,
            restart: self.restart,
            data,
            interleaved,
            mcu_cols,
            luma_cols: if interleaved { luma_cols } else { mcu_cols },
        };
        self.scans += 1;
        let outcome = decode(
            &plan,
            self.r,
            Snapshot::start(data),
            self.limit,
            None,
            None,
            None,
        );
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

/// One component of a scan.
#[derive(Clone)]
struct ScanComp {
    /// Blocks per MCU (h x v when interleaved, 1 otherwise).
    n: u32,
    h: u64,
    v: u64,
    dc: Huffman,
    ac: Huffman,
    /// DC quantizer, to scale DC coefficients back.
    quant: i32,
    /// The first (luminance) component, whose DC values form a 1/8-scale image.
    luma: bool,
}

/// What decoding one baseline scan needs.
#[derive(Clone)]
pub(crate) struct Plan {
    layout: Vec<ScanComp>,
    pub mcus: u64,
    restart: u16,
    /// Offset of the first entropy-coded byte.
    pub data: u64,
    interleaved: bool,
    /// MCUs per row.
    mcu_cols: u64,
    /// Luminance blocks per row.
    pub luma_cols: u64,
}

/// Decoder state at the start of an MCU, enough to resume from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Snapshot {
    /// Offset of the next byte to load; every byte before it has been consumed.
    pub pos: u64,
    acc: u64,
    count: u32,
    pub mcu: u64,
    rst: u8,
    /// DC predictors per scan component (F.2.1.3.1).
    pred: [i32; 4],
}

impl Snapshot {
    pub fn start(data: u64) -> Self {
        Self {
            pos: data,
            acc: 0,
            count: 0,
            mcu: 0,
            rst: 0,
            pred: [0; 4],
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
/// With `snaps`, records the state at every MCU start; with `dcs`, records each luminance
/// block's DC value (scaled back by its quantizer) as `(block index, value)`, where the
/// index is `row * luma_cols + column` in the 1/8-scale image.
pub(crate) fn decode<T: Stream + ?Sized>(
    plan: &Plan,
    s: &mut T,
    from: Snapshot,
    limit: u64,
    cap: Option<u64>,
    mut snaps: Option<&mut Vec<Snapshot>>,
    mut dcs: Option<&mut Vec<(u64, i32)>>,
) -> Outcome {
    let mut bits = Bits {
        pos: from.pos,
        acc: from.acc,
        count: from.count,
        limit,
        stop: None,
    };
    let mut rst = from.rst;
    let mut pred = from.pred;
    for mcu in from.mcu..plan.mcus {
        let snap = Snapshot {
            pos: bits.pos,
            acc: bits.acc,
            count: bits.count,
            mcu,
            rst,
            pred,
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
                pred = [0; 4];
            }
            let (mx, my) = (mcu % plan.mcu_cols, mcu / plan.mcu_cols);
            for (ci, c) in plan.layout.iter().enumerate() {
                for k in 0..u64::from(c.n) {
                    let diff = block(s, &mut bits, &c.dc, &c.ac)?;
                    let p = pred.get_mut(ci).ok_or(Stop::Corrupt(bits.pos))?;
                    *p = p.wrapping_add(diff);
                    if c.luma
                        && let Some(v) = dcs.as_deref_mut()
                    {
                        let (bx, by) = if plan.interleaved {
                            (mx * c.h + k % c.h, my * c.v + k / c.h)
                        } else {
                            (mx, my)
                        };
                        v.push((by * plan.luma_cols + bx, p.wrapping_mul(c.quant)));
                    }
                }
            }
            Ok(())
        })();
        if let Err(stop) = step {
            return Outcome::Fail(stop);
        }
    }
    // After the last MCU only padding bits of the last byte may remain; then the scan ends
    // at the next marker, possibly after fill bytes.
    if bits.count >= 8 {
        return Outcome::Fail(Stop::Corrupt(
            bits.pos.saturating_sub(u64::from(bits.count / 8)),
        ));
    }
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
///
/// Up to 64 bits are buffered, left-aligned in `acc`. Loading stops at a marker (or at the
/// end of the data), and asking for bits beyond it is an error at that point.
struct Bits {
    /// Offset of the next byte to load. Every byte before it is in `acc` or consumed.
    pos: u64,
    acc: u64,
    count: u32,
    limit: u64,
    /// Why loading stopped, if it did.
    stop: Option<Stop>,
}

// The buffer is shifted right by at least 48 bits before narrowing, so the casts in this
// impl cannot truncate.
#[allow(clippy::cast_possible_truncation)]
impl Bits {
    /// Loads bytes until at least 57 bits are buffered or loading has to stop.
    fn refill<T: Stream + ?Sized>(&mut self, s: &mut T) {
        while self.count <= 56 && self.stop.is_none() {
            if self.pos >= self.limit {
                self.stop = Some(Stop::Truncated);
                break;
            }
            let Some(b) = s.byte(self.pos) else {
                self.stop = Some(Stop::Truncated);
                break;
            };
            if b == 0xFF {
                match s.byte(self.pos + 1) {
                    Some(0x00) => self.pos += 2,
                    // A marker: the entropy-coded data ends here.
                    Some(_) => {
                        self.stop = Some(Stop::Corrupt(self.pos));
                        break;
                    }
                    None => {
                        self.stop = Some(Stop::Truncated);
                        break;
                    }
                }
            } else {
                self.pos += 1;
            }
            self.acc |= u64::from(b) << (56 - self.count);
            self.count += 8;
        }
    }

    /// The next `n` (<= 16) bits, right-aligned, without consuming them.
    fn peek<T: Stream + ?Sized>(&mut self, s: &mut T, n: u32) -> Result<u32, Stop> {
        if self.count < n {
            self.refill(s);
            if self.count < n {
                return Err(self.stop.unwrap_or(Stop::Truncated));
            }
        }
        Ok((self.acc >> (64 - n)) as u32)
    }

    fn consume(&mut self, n: u32) {
        self.acc = self.acc.checked_shl(n).unwrap_or(0);
        self.count -= n;
    }

    fn skip<T: Stream + ?Sized>(&mut self, s: &mut T, n: u32) -> Result<(), Stop> {
        if n > 0 {
            self.peek(s, n)?;
            self.consume(n);
        }
        Ok(())
    }

    /// F.2.2.1 RECEIVE then EXTEND: an `n`-bit value as a signed difference.
    fn receive_extend<T: Stream + ?Sized>(&mut self, s: &mut T, n: u32) -> Result<i32, Stop> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.peek(s, n)? as i32;
        self.consume(n);
        Ok(if v < (1 << (n - 1)) {
            v - (1 << n) + 1
        } else {
            v
        })
    }

    /// F.2.2.3 DECODE, with a lookahead table for codes up to `LOOKAHEAD` bits.
    fn decode<T: Stream + ?Sized>(&mut self, s: &mut T, t: &Huffman) -> Result<u8, Stop> {
        if self.count < 16 {
            self.refill(s);
        }
        let avail = self.count.min(16);
        let at = self.pos;
        if avail >= LOOKAHEAD {
            let look = (self.acc >> (64 - LOOKAHEAD)) as usize;
            if let Some(&(len, sym)) = t.fast.get(look)
                && len > 0
            {
                self.consume(u32::from(len));
                return Ok(sym);
            }
        }
        // A longer code (or few bits left): match lengths one by one.
        let bits16 = (self.acc >> 48) as i32;
        for len in 1..=16u32 {
            if len > avail {
                return Err(self.stop.unwrap_or(Stop::Truncated));
            }
            let code = bits16 >> (16 - len);
            let l = len as usize;
            if code <= t.maxcode.get(l).copied().unwrap_or(-1) {
                let base = t.valptr.get(l).copied().unwrap_or(0);
                let min = t.mincode.get(l).copied().unwrap_or(0);
                let idx = usize::try_from(base + code - min).map_err(|_| Stop::Corrupt(at))?;
                let sym = t.values.get(idx).copied().ok_or(Stop::Corrupt(at))?;
                self.consume(len);
                return Ok(sym);
            }
        }
        Err(Stop::Corrupt(at))
    }

    /// At a restart interval: drop the padding bits and expect `RSTn` (F.1.2.3, B.2.1).
    fn restart<T: Stream + ?Sized>(&mut self, s: &mut T, n: u8) -> Result<(), Stop> {
        // Only the padding of the last byte may be left before the marker.
        if self.count >= 8 {
            return Err(Stop::Corrupt(self.pos));
        }
        self.acc = 0;
        self.count = 0;
        self.stop = None;
        let mut pos = self.pos;
        while s.byte(pos) == Some(0xFF) && s.byte(pos + 1) == Some(0xFF) {
            pos += 1;
        }
        match (s.byte(pos), s.byte(pos + 1)) {
            (Some(0xFF), Some(m)) if m == 0xD0 + n => {
                self.pos = pos + 2;
                Ok(())
            }
            (Some(_), Some(_)) => Err(Stop::Corrupt(pos)),
            _ => Err(Stop::Truncated),
        }
    }
}

/// Decodes one 8x8 block (F.2.2.1-F.2.2.2) and returns its DC difference. The AC
/// coefficients are skipped without reconstructing them.
fn block<T: Stream + ?Sized>(
    s: &mut T,
    bits: &mut Bits,
    dc: &Huffman,
    ac: &Huffman,
) -> Result<i32, Stop> {
    let size = bits.decode(s, dc)?;
    if size > 11 {
        return Err(Stop::Corrupt(bits.pos));
    }
    let diff = bits.receive_extend(s, u32::from(size))?;
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
    Ok(diff)
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
