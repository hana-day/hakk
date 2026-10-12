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
    /// Bytes from SOI through EOI if complete (plus any appended parts, see `appended`);
    /// up to the failure point otherwise.
    pub len: u64,
    /// Bytes from SOI through the primary image's EOI.
    pub primary_len: u64,
    /// Parts stored after the primary image's EOI and included in `len`: a gain map
    /// (Ultra HDR), a motion photo's video, MPF images.
    pub appended: Vec<Appended>,
    pub status: Status,
    pub width: u16,
    pub height: u16,
    pub progressive: bool,
    /// Whether the entropy-coded data was fully Huffman-decoded (baseline only).
    pub verified: bool,
}

/// A part stored after the primary image, found through the file's own directory (XMP
/// `Container:Directory` or MPF) and checked to start where the directory says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Appended {
    pub kind: AppendedKind,
    /// Offset from the start of the file.
    pub offset: u64,
    pub len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendedKind {
    /// Another JPEG (a gain map, a preview, an MPF image).
    Image,
    /// An MP4/QuickTime video (a motion photo).
    Video,
    /// Something else the directory lists.
    Other,
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
        quant: [[1; 64]; 4],
        directory: Directory::default(),
    };
    let (end, status) = p.run();
    if p.segments == 0 {
        return None;
    }
    let (width, height) = p.frame.as_ref().map_or((0, 0), |f| (f.width, f.height));
    let appended = if status == Status::Complete {
        p.appended(end)
    } else {
        Vec::new()
    };
    let total_end = appended.last().map_or(end, |a| start + a.offset + a.len);
    let jpeg = Jpeg {
        len: total_end - start,
        primary_len: end - start,
        appended,
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

/// One item of an XMP container directory.
struct Item {
    mime: String,
    length: u64,
    padding: u64,
}

/// What the header says follows the primary image.
#[derive(Default)]
struct Directory {
    /// XMP `Container:Directory` items, the primary first.
    items: Vec<Item>,
    /// `GCamera:MicroVideoOffset`: the video's length at the end of the file.
    micro_video: Option<u64>,
    /// MPF entries: (absolute offset, size) of the other images.
    mpf: Vec<(u64, u64)>,
}

/// The value of `name="..."` in an XML fragment.
fn attribute(xml: &str, name: &str) -> Option<String> {
    let at = xml.find(&format!("{name}=\""))? + name.len() + 2;
    let rest = xml.get(at..)?;
    Some(rest.get(..rest.find('"')?)?.to_owned())
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

/// The example tables of T.81 K.3 (Tables K.3 to K.6) as (BITS, HUFFVAL), by class and id:
/// DC luminance, DC chrominance, AC luminance, AC chrominance. Motion JPEG frames (AVI1)
/// leave out DHT and are decoded with these.
fn standard_table(class: usize, id: usize) -> Option<Huffman> {
    const DC_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    const DC_LUMA: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    const DC_CHROMA: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
    const AC_LUMA: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
    const AC_LUMA_VALUES: [u8; 162] = [
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52,
        0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
        0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3,
        0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8,
        0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ];
    const AC_CHROMA: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
    const AC_CHROMA_VALUES: [u8; 162] = [
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33,
        0x52, 0xF0, 0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18,
        0x19, 0x1A, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4,
        0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA,
        0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
        0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ];
    let (bits, values): (&[u8; 16], &[u8]) = match (class, id) {
        (0, 0) => (&DC_LUMA, &DC_VALUES),
        (0, 1) => (&DC_CHROMA, &DC_VALUES),
        (1, 0) => (&AC_LUMA, &AC_LUMA_VALUES),
        (1, 1) => (&AC_CHROMA, &AC_CHROMA_VALUES),
        _ => return None,
    };
    Huffman::new(bits, values.to_vec())
}

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
    /// Quantization tables in zigzag order (to scale coefficients back).
    quant: [[i32; 64]; 4],
    /// What the header says follows the primary image.
    directory: Directory,
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
            0xE1 => self.xmp(body, len - 2),
            0xE2 => self.mpf(body, len - 2),
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

    /// APP1: an XMP packet may carry a container directory (Google "Container" items, used
    /// by Ultra HDR and motion photos) or an older motion photo's video offset.
    ///
    /// Reference: Android "Ultra HDR image format" and "Motion photo format" (XMP
    /// `Container:Directory` with `Item:Semantic`, `Item:Length`, `Item:Padding`;
    /// `GCamera:MicroVideoOffset`).
    fn xmp(&mut self, body: u64, len: u64) {
        const NS: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
        let Some(text) = usize::try_from(len)
            .ok()
            .and_then(|n| self.r.slice(body, n))
        else {
            return;
        };
        if !text.starts_with(NS) {
            return;
        }
        let text = String::from_utf8_lossy(text).into_owned();
        let mut items = Vec::new();
        for element in text.split("<Container:Item").skip(1) {
            let element = element.split('>').next().unwrap_or_default();
            items.push(Item {
                mime: attribute(element, "Item:Mime").unwrap_or_default(),
                length: attribute(element, "Item:Length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                padding: attribute(element, "Item:Padding")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
            });
        }
        if !items.is_empty() {
            self.directory.items = items;
        }
        if let Some(v) = attribute(&text, "GCamera:MicroVideoOffset").and_then(|v| v.parse().ok()) {
            self.directory.micro_video = Some(v);
        }
    }

    /// APP2: an MPF (Multi-Picture Format) index lists the other images in the file.
    ///
    /// Reference: CIPA DC-007 "Multi-Picture Format" 5.2 (MP Index IFD, MP Entry).
    fn mpf(&mut self, body: u64, len: u64) {
        let Some(head) = self.r.slice(body, 4) else {
            return;
        };
        if head != b"MPF\0" || len < 16 {
            return;
        }
        let tiff = body + 4;
        let little = match self.r.slice(tiff, 2) {
            Some(b"II") => true,
            Some(b"MM") => false,
            _ => return,
        };
        let Some(ifd) = self.u32_endian(tiff + 4, little).map(u64::from) else {
            return;
        };
        let mut entries = Vec::new();
        let Some(n) = self.u16_endian(tiff + ifd, little) else {
            return;
        };
        for i in 0..u64::from(n.min(64)) {
            let at = tiff + ifd + 2 + 12 * i;
            if self.u16_endian(at, little) != Some(0xB002) {
                continue;
            }
            let (Some(total), Some(off)) = (
                self.u32_endian(at + 4, little),
                self.u32_endian(at + 8, little),
            ) else {
                continue;
            };
            // MP Entry: attribute (4), size (4), offset (4), dependent entries (2 + 2).
            for k in 0..u64::from(total / 16).min(16) {
                let e = tiff + u64::from(off) + 16 * k;
                if let (Some(size), Some(offset)) = (
                    self.u32_endian(e + 4, little),
                    self.u32_endian(e + 8, little),
                ) && offset > 0
                {
                    entries.push((tiff + u64::from(offset), u64::from(size)));
                }
            }
        }
        if !entries.is_empty() {
            self.directory.mpf = entries;
        }
    }

    fn u16_endian(&mut self, at: u64, little: bool) -> Option<u16> {
        let b: [u8; 2] = self.r.slice(at, 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32_endian(&mut self, at: u64, little: bool) -> Option<u32> {
        let b: [u8; 4] = self.r.slice(at, 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    /// The parts after the primary image ending at `end`, as the header's directory
    /// describes them, kept only if each starts with what it should and lies within reach.
    fn appended(&mut self, end: u64) -> Vec<Appended> {
        let start = self.start;
        let mut out = Vec::new();
        let items = std::mem::take(&mut self.directory.items);
        if items.len() > 1 {
            // Items after the primary follow it back to back; the primary's padding first.
            let mut at = end + items.first().map_or(0, |i| i.padding);
            for item in items.iter().skip(1) {
                let kind = if item.mime.starts_with("video/") {
                    AppendedKind::Video
                } else if item.mime.starts_with("image/jpeg") {
                    AppendedKind::Image
                } else {
                    AppendedKind::Other
                };
                if item.length == 0 || !self.starts_like(at, kind) {
                    break;
                }
                out.push(Appended {
                    kind,
                    offset: at - start,
                    len: item.length,
                });
                at += item.length + item.padding;
            }
        } else if let Some(len) = self.directory.micro_video {
            // The video is the last `len` bytes of the file, right after the image.
            if len > 0 && self.starts_like(end, AppendedKind::Video) {
                out.push(Appended {
                    kind: AppendedKind::Video,
                    offset: end - start,
                    len,
                });
            }
        } else {
            let mut entries = std::mem::take(&mut self.directory.mpf);
            entries.sort_unstable();
            for (at, len) in entries {
                if at >= end && len > 0 && self.starts_like(at, AppendedKind::Image) {
                    out.push(Appended {
                        kind: AppendedKind::Image,
                        offset: at - start,
                        len,
                    });
                }
            }
        }
        // Keep only parts that lie within reach.
        let limit = self.limit.min(self.r.len());
        out.retain(|a| start + a.offset + a.len <= limit);
        out
    }

    /// Whether the bytes at `at` start a part of this kind.
    fn starts_like(&mut self, at: u64, kind: AppendedKind) -> bool {
        match kind {
            AppendedKind::Image => self.r.slice(at, 3) == Some([0xFF, 0xD8, 0xFF].as_slice()),
            AppendedKind::Video => self.r.slice(at + 4, 4) == Some(b"ftyp".as_slice()),
            AppendedKind::Other => true,
        }
    }

    /// DQT (B.2.4.1): keeps each table (in zigzag order).
    fn dqt(&mut self, mut at: u64, len: u64) -> Option<()> {
        let end = at + len;
        while at < end {
            let pq_tq = self.r.byte(at)?;
            let wide = pq_tq >> 4 != 0;
            let mut table = [1i32; 64];
            for (k, q) in table.iter_mut().enumerate() {
                let k = k as u64;
                *q = if wide {
                    i32::from(self.r.u16_be(at + 1 + 2 * k)?)
                } else {
                    i32::from(self.r.byte(at + 1 + k)?)
                };
            }
            *self.quant.get_mut(usize::from(pq_tq & 0x03))? = table;
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
        // Tables the scan uses but no DHT defined: the standard ones (Motion JPEG).
        for (_, td, ta) in &comps {
            for (class, id) in [(0, *td), (1, *ta)] {
                if let Some(slot) = self.tables.get_mut(class).and_then(|t| t.get_mut(id))
                    && slot.is_none()
                {
                    *slot = standard_table(class, id);
                }
            }
        }
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
            let quant = self
                .quant
                .get(usize::from(c.tq))
                .copied()
                .unwrap_or([1; 64]);
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
    /// Quantization table in zigzag order, to scale coefficients back.
    quant: [i32; 64],
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

impl Plan {
    /// Whether the scan has restart markers (checked by the decoder, a strong signal when
    /// splicing fragments).
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
/// With `snaps`, records the state at every MCU start; with `edges`, records the top and
/// bottom pixel rows of each luminance block (see [`Edges`]).
pub(crate) fn decode<T: Stream + ?Sized>(
    plan: &Plan,
    s: &mut T,
    from: Snapshot,
    limit: u64,
    cap: Option<u64>,
    mut snaps: Option<&mut Vec<Snapshot>>,
    mut edges: Option<&mut Vec<Edges>>,
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
    let mut coefs = [0i32; 64];
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
                    let want = c.luma && edges.is_some();
                    let diff = block(
                        s,
                        &mut bits,
                        &c.dc,
                        &c.ac,
                        want.then_some((&mut coefs, &c.quant)),
                    )?;
                    let p = pred.get_mut(ci).ok_or(Stop::Corrupt(bits.pos))?;
                    *p = p.wrapping_add(diff);
                    if c.luma
                        && let Some(v) = edges.as_deref_mut()
                    {
                        let (bx, by) = if plan.interleaved {
                            (mx * c.h + k % c.h, my * c.v + k / c.h)
                        } else {
                            (mx, my)
                        };
                        coefs[0] = p.wrapping_mul(c.quant[0]);
                        let (top, bottom) = edge_rows(&coefs);
                        v.push(Edges {
                            idx: by * plan.luma_cols + bx,
                            top,
                            bottom,
                        });
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

/// The top and bottom pixel rows of a luminance block, to judge whether two blocks fit
/// together vertically (the level shift of 128 is left out; only differences matter).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Edges {
    /// `row * luma_cols + column` in the grid of 8x8 luminance blocks.
    pub idx: u64,
    pub top: [i16; 8],
    pub bottom: [i16; 8],
}

/// Zigzag position -> natural (row-major) position (T.81 Figure A.6).
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// `c(u) * cos((2x + 1) u pi / 16) / 2` for x, u in 0..8 (T.81 A.3.3, the 8x8 IDCT).
fn idct_basis() -> &'static [[f32; 8]; 8] {
    static BASIS: std::sync::OnceLock<[[f32; 8]; 8]> = std::sync::OnceLock::new();
    BASIS.get_or_init(|| {
        let mut t = [[0f32; 8]; 8];
        for (x, row) in t.iter_mut().enumerate() {
            for (u, v) in row.iter_mut().enumerate() {
                let c = if u == 0 {
                    std::f32::consts::FRAC_1_SQRT_2
                } else {
                    1.0
                };
                let angle = (2 * x + 1) as f32 * u as f32 * std::f32::consts::PI / 16.0;
                *v = c * angle.cos() / 2.0;
            }
        }
        t
    })
}

/// Pixel rows 0 and 7 of a block from its dequantized coefficients (zigzag order).
// Every index below is into an 8x8 array with a 0..8 loop variable, and pixel values are
// clamped to the i16 range before narrowing.
#[allow(clippy::indexing_slicing, clippy::cast_possible_truncation)]
fn edge_rows(zz: &[i32; 64]) -> ([i16; 8], [i16; 8]) {
    let basis = idct_basis();
    let mut f = [[0f32; 8]; 8];
    for (k, &v) in zz.iter().enumerate() {
        if v != 0 {
            let n = ZIGZAG.get(k).copied().unwrap_or(0);
            if let Some(cell) = f.get_mut(n / 8).and_then(|r| r.get_mut(n % 8)) {
                *cell = v as f32;
            }
        }
    }
    // g[v][x] = sum over u of basis[x][u] * f[v][u]
    let mut g = [[0f32; 8]; 8];
    for (gv, fv) in g.iter_mut().zip(&f) {
        for (x, out) in gv.iter_mut().enumerate() {
            *out = basis[x].iter().zip(fv).map(|(b, c)| b * c).sum();
        }
    }
    let row = |y: usize| {
        let mut px = [0i16; 8];
        for (x, p) in px.iter_mut().enumerate() {
            let v: f32 = (0..8).map(|v| basis[y][v] * g[v][x]).sum();
            *p = v.round().clamp(-2048.0, 2047.0) as i16;
        }
        px
    };
    (row(0), row(7))
}

/// Decodes one 8x8 block (F.2.2.1-F.2.2.2) and returns its DC difference. With `coefs`,
/// the AC coefficients are dequantized into the given buffer (zigzag order); otherwise
/// they are only skipped.
fn block<T: Stream + ?Sized>(
    s: &mut T,
    bits: &mut Bits,
    dc: &Huffman,
    ac: &Huffman,
    mut coefs: Option<(&mut [i32; 64], &[i32; 64])>,
) -> Result<i32, Stop> {
    if let Some((c, _)) = coefs.as_mut() {
        c.fill(0);
    }
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
        match coefs.as_mut() {
            Some((c, q)) => {
                let v = bits.receive_extend(s, size)?;
                let i = k as usize;
                if let (Some(slot), Some(qk)) = (c.get_mut(i), q.get(i)) {
                    *slot = v.wrapping_mul(*qk);
                }
            }
            None => bits.skip(s, size)?,
        }
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

    /// A JPEG whose XMP directory lists `parts` after it, followed by `tail`.
    fn with_directory(parts: &[(&str, usize)], tail: &[u8]) -> Vec<u8> {
        let mut xmp = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        xmp.extend_from_slice(b"<Container:Directory><rdf:Seq>");
        xmp.extend_from_slice(
            b"<Container:Item Item:Mime=\"image/jpeg\" Item:Semantic=\"Primary\"/>",
        );
        for (mime, len) in parts {
            xmp.extend_from_slice(
                format!("<Container:Item Item:Mime=\"{mime}\" Item:Length=\"{len}\"/>").as_bytes(),
            );
        }
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE1];
        j.extend(u16::try_from(xmp.len() + 2).unwrap().to_be_bytes());
        j.extend(xmp);
        j.extend(&tiny()[2..]);
        j.extend(tail);
        j
    }

    #[test]
    fn appended_parts_from_xmp_directory() {
        let gain = tiny();
        let mut video = 16u32.to_be_bytes().to_vec();
        video.extend(b"ftypisom\0\0\0\0");
        video.extend([0x55; 100]);
        let parts = [("image/jpeg", gain.len()), ("video/mp4", video.len())];
        let j = with_directory(&parts, &[gain.clone(), video.clone()].concat());
        let got = run([j.clone(), vec![0xEE; 50]].concat()).unwrap();
        assert_eq!(got.len, j.len() as u64);
        assert_eq!(got.appended.len(), 2);
        assert_eq!(got.appended[1].kind, AppendedKind::Video);
        assert_eq!(got.primary_len, (j.len() - gain.len() - video.len()) as u64);
    }

    /// If the bytes after the image are not what the directory says (overwritten, or
    /// fragmented elsewhere), only the image is kept.
    #[test]
    fn missing_appended_part_keeps_the_image() {
        let parts = [("video/mp4", 116)];
        let j = with_directory(&parts, &[0x11; 116]);
        let got = run(j.clone()).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert!(got.appended.is_empty());
        assert_eq!(got.len, (j.len() - 116) as u64);
    }

    #[test]
    fn not_a_jpeg() {
        assert!(run(vec![0x00; 64]).is_none());
        assert!(run(vec![0xFF, 0xD8, 0xFF, 0x00, 0x00]).is_none());
    }
}
