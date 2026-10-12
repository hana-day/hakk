//! Camera RAW and TIFF carving: the TIFF-based formats (Canon `.cr2`, Nikon `.nef`, Sony
//! `.arw`, Adobe `.dng`, Olympus `.orf`, Panasonic `.rw2`, Pentax `.pef`, plain `.tif`) and
//! Fujifilm `.raf`. (Canon `.cr3` is ISO BMFF and goes through the MP4 carver.)
//!
//! None of them has an end marker, so the length comes from the structure: a TIFF file
//! ends where the furthest thing its directories point at ends (directories, values stored
//! out of line, image strips and tiles, embedded JPEGs, sub-directories), and a RAF file
//! where the furthest of the parts its header lists ends. The parts that can be checked
//! are: every directory parses, every embedded JPEG preview decodes completely, lossless
//! JPEG raw data ends with EOI where it should, and uncompressed raw data stored as 16-bit
//! words holds no value above its bit depth (sampled every few kilobytes, so data from
//! another file shows). Compressed raw data in vendor formats is not checked.
//!
//! Reference: Adobe, "TIFF Revision 6.0" section 2 (header, IFD, field types), section 7
//! (strips), section 15 (tiles), and Adobe "TIFF Technical Note 1" (SubIFDs); Adobe, "Digital
//! Negative (DNG) Specification" 1.6 (DNGVersion, previews in SubIFDs); CIPA DC-008 "Exif
//! 2.32" 4.6.3 (Exif and GPS IFD pointers), 4.6.5 (JPEGInterchangeFormat); the vendor
//! variants as documented by ExifTool's tag tables (Canon CR2 header "CR", Olympus ORF
//! magic "RO", Panasonic RW2 magic 0x55 with RawDataOffset 0x118 and RawFormat 0x2D, the
//! Fujifilm RAF header layout).

use blocksource::{BlockSource, MemSource};

use crate::{jpeg, reader::Reader};

/// What kind of TIFF-based or RAW file it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKind {
    Tiff,
    Cr2,
    Nef,
    Arw,
    Dng,
    Orf,
    Rw2,
    Pef,
    Raf,
}

/// A file found at some offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    pub kind: RawKind,
    pub len: u64,
    pub status: Status,
    pub width: u32,
    pub height: u32,
    /// Whether all the large data (sensor data) was checked; false for vendor-compressed
    /// or packed raw data, which only the vendor's decoder could check.
    pub verified: bool,
}

/// Image data larger than this counts as sensor data that should be checked.
const LARGE: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The structure holds together and every embedded JPEG decodes.
    Complete,
    /// Something it points at is broken at this source offset (an embedded JPEG).
    Corrupt { at: u64 },
    /// The data ran out before the furthest part.
    Truncated,
}

/// Most directories followed (IFD chains and sub-directories together).
const MAX_IFDS: usize = 64;
/// Most entries in one directory.
const MAX_ENTRIES: u64 = 1000;
/// Most strip or tile offsets read from one entry.
const MAX_PARTS: u64 = 1 << 20;
/// Uncompressed raw data is sampled every this many bytes.
const SAMPLE_EVERY: u64 = 4096;
/// Bytes read at each sample.
const SAMPLE_LEN: usize = 64;
/// Panasonic writes its raw data in blocks of this many bytes.
const RW2_BLOCK: u64 = 8192;

/// Whether `head` starts a TIFF-based file or a RAF file.
pub fn looks_like_raw(head: &[u8]) -> bool {
    head.starts_with(b"FUJIFILMCCD-RAW ") || tiff_order(head).is_some()
}

/// Byte order and magic number of a TIFF-style header (`II`/`MM` with 42, or the Olympus
/// and Panasonic variants), if `head` starts with one whose first directory offset is sane.
fn tiff_order(head: &[u8]) -> Option<(Order, u16)> {
    let order = match head.get(..2)? {
        b"II" => Order::Little,
        b"MM" => Order::Big,
        _ => return None,
    };
    let magic = order.u16(head.get(2..4)?);
    let first = order.u32(head.get(4..8)?);
    let known = matches!(magic, 42 | 0x4F52 | 0x5352 | 0x55);
    (known && (8..1 << 30).contains(&first)).then_some((order, magic))
}

/// Follows the file at `start`. `None` if it is not one of these formats.
pub fn carve<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Raw> {
    let head = r.slice(start, 16)?.to_vec();
    if head.starts_with(b"FUJIFILMCCD-RAW ") {
        return raf(r, start, max_len);
    }
    let (order, magic) = tiff_order(&head)?;
    let mut t = Tiff {
        r,
        start,
        max_len,
        order,
        ends: 0,
        ifds: 0,
        jpegs: Vec::new(),
        words: Vec::new(),
        unchecked: false,
        make: Vec::new(),
        dng: false,
        width: 0,
        height: 0,
        rw2: Rw2::default(),
    };
    let first = u64::from(order.u32(head.get(4..8)?));
    t.chain(first)?;
    if t.ifds == 0 {
        return None;
    }
    let kind = if head.get(8..10) == Some(b"CR") {
        RawKind::Cr2
    } else if matches!(magic, 0x4F52 | 0x5352) {
        RawKind::Orf
    } else if magic == 0x55 {
        RawKind::Rw2
    } else if t.dng {
        RawKind::Dng
    } else if t.make.starts_with(b"NIKON") {
        RawKind::Nef
    } else if t.make.starts_with(b"SONY") {
        RawKind::Arw
    } else if t.make.starts_with(b"PENTAX") || t.make.starts_with(b"RICOH") {
        RawKind::Pef
    } else {
        RawKind::Tiff
    };
    if kind == RawKind::Rw2 {
        t.rw2_data();
        // Panasonic's raw data is in its own compression.
        t.unchecked = true;
    }
    // Some writers pad the end to an even length and some do not; the padding byte, if
    // any, is left out.
    let len = t.ends;
    let (width, height) = (t.width, t.height);
    let jpegs = std::mem::take(&mut t.jpegs);
    let words = std::mem::take(&mut t.words);
    let status = if t.r.slice(start + len - 1, 1).is_none() {
        Status::Truncated
    } else {
        match check_jpegs(t.r, start, &jpegs) {
            Status::Complete => check_words(t.r, start, &words),
            other => other,
        }
    };
    Some(Raw {
        kind,
        len,
        status,
        width,
        height,
        verified: !t.unchecked,
    })
}

/// Fujifilm RAF: a 0x54-byte header naming the camera, then big-endian (offset, length)
/// pairs of the JPEG preview, the CFA header and the CFA data.
fn raf<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Raw> {
    let h = r.slice(start + 0x54, 24)?.to_vec();
    let be = |i: usize| u64::from(Order::Big.u32(h.get(i..i + 4).unwrap_or_default()));
    let parts = [(be(0), be(4)), (be(8), be(12)), (be(16), be(20))];
    let mut len = 0u64;
    for &(o, l) in &parts {
        if o == 0 || l == 0 {
            continue;
        }
        let end = o.checked_add(l)?;
        if o < 0x54 + 24 || end > max_len {
            return None;
        }
        len = len.max(end);
    }
    if len == 0 {
        return None;
    }
    let status = if r.slice(start + len - 1, 1).is_none() {
        Status::Truncated
    } else {
        check_jpegs(r, start, &[parts[0]])
    };
    Some(Raw {
        kind: RawKind::Raf,
        len,
        status,
        width: 0,
        height: 0,
        // The CFA data is not checked.
        verified: false,
    })
}

/// Checks the embedded JPEGs at `(offset, length)` from `start`.
fn check_jpegs<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    start: u64,
    jpegs: &[(u64, u64)],
) -> Status {
    for &(o, l) in jpegs {
        if !decodes(r, start + o, l) {
            return Status::Corrupt { at: start + o };
        }
    }
    Status::Complete
}

/// Checks uncompressed raw data `(offset, length, bits)` from `start`: every sampled 16-bit
/// word must fit in `bits`.
fn check_words<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    start: u64,
    words: &[(u64, u64, u32)],
) -> Status {
    for &(o, l, bits) in words {
        let limit = 1u32 << bits;
        let mut at = 0;
        while at + SAMPLE_LEN as u64 <= l {
            let Some(b) = r.slice(start + o + at, SAMPLE_LEN) else {
                return Status::Truncated;
            };
            let fits = b
                .chunks_exact(2)
                .all(|w| u32::from(Order::Little.u16(w)) < limit);
            if !fits {
                return Status::Corrupt { at: start + o + at };
            }
            at += SAMPLE_EVERY;
        }
    }
    Status::Complete
}

/// Whether the JPEG of `len` bytes at `at` decodes completely (lossless JPEG, which the
/// JPEG carver does not decode, only needs its SOI and EOI where they belong).
fn decodes<S: BlockSource + ?Sized>(r: &mut Reader<S>, at: u64, len: u64) -> bool {
    let Ok(n) = usize::try_from(len) else {
        return false;
    };
    if n < 4 {
        return false;
    }
    let mut data = vec![0u8; n];
    if r.read_into(at, &mut data).is_none() {
        return false;
    }
    if !data.starts_with(&[0xFF, 0xD8]) {
        return false;
    }
    // A lossless JPEG (SOF3): just its markers.
    if lossless(&data) {
        return data.windows(2).rev().take(16).any(|w| w == [0xFF, 0xD9]);
    }
    data.resize(n + 4096, 0);
    let src = MemSource::new(data);
    let mut rr = Reader::new(&src);
    // The JPEG carver looks a little past EOI (for appended parts).
    jpeg::carve(&mut rr, 0, len + 4096).is_some_and(|j| j.status == jpeg::Status::Complete)
}

/// Whether the JPEG in `data` is lossless (its frame header is SOF3).
fn lossless(data: &[u8]) -> bool {
    let mut at = 2;
    while let (Some(&0xFF), Some(&m)) = (data.get(at), data.get(at + 1)) {
        if m == 0xC3 {
            return true;
        }
        if matches!(m, 0xC0..=0xC2 | 0xDA) {
            return false;
        }
        let len = data
            .get(at + 2..at + 4)
            .map_or(0, |b| usize::from(Order::Big.u16(b)));
        at += 2 + len;
    }
    false
}

#[derive(Debug, Clone, Copy)]
enum Order {
    Little,
    Big,
}

impl Order {
    fn u16(self, b: &[u8]) -> u16 {
        let b = [
            b.first().copied().unwrap_or(0),
            b.get(1).copied().unwrap_or(0),
        ];
        match self {
            Order::Little => u16::from_le_bytes(b),
            Order::Big => u16::from_be_bytes(b),
        }
    }

    fn u32(self, b: &[u8]) -> u32 {
        let b: [u8; 4] = b
            .get(..4)
            .and_then(|b| b.try_into().ok())
            .unwrap_or_default();
        match self {
            Order::Little => u32::from_le_bytes(b),
            Order::Big => u32::from_be_bytes(b),
        }
    }
}

/// Panasonic fields needed to size the raw data.
#[derive(Debug, Default)]
struct Rw2 {
    offset: u64,
    width: u64,
    height: u64,
    format: u64,
}

struct Tiff<'a, 'b, S: BlockSource + ?Sized> {
    r: &'a mut Reader<'b, S>,
    start: u64,
    max_len: u64,
    order: Order,
    /// The furthest end of anything pointed at, from the file start.
    ends: u64,
    ifds: usize,
    /// Embedded JPEGs to check: (offset, length) from the file start.
    jpegs: Vec<(u64, u64)>,
    /// Uncompressed 16-bit raw data to check: (offset, length, bits per sample).
    words: Vec<(u64, u64, u32)>,
    /// Large image data that no check covers.
    unchecked: bool,
    make: Vec<u8>,
    dng: bool,
    width: u32,
    height: u32,
    rw2: Rw2,
}

/// A directory entry: tag, field type, count and the value/offset field.
struct Entry {
    tag: u16,
    kind: u16,
    count: u64,
    field: [u8; 4],
}

impl Entry {
    /// Bytes the value takes (TIFF 6.0 section 2, plus the BigTIFF-era IFD type 13).
    fn size(&self) -> Option<u64> {
        let unit = match self.kind {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 | 13 => 4,
            5 | 10 | 12 => 8,
            _ => return None,
        };
        self.count.checked_mul(unit)
    }
}

impl<S: BlockSource + ?Sized> Tiff<'_, '_, S> {
    /// Notes that the file reaches at least `end` (from its start), if that is plausible.
    fn reach(&mut self, end: u64) -> bool {
        if end > self.max_len {
            return false;
        }
        self.ends = self.ends.max(end);
        true
    }

    /// Follows a chain of directories from `offset`.
    fn chain(&mut self, mut offset: u64) -> Option<()> {
        while offset != 0 {
            let next = self.ifd(offset)?;
            offset = next;
        }
        Some(())
    }

    /// Reads the directory at `offset`; the next one's offset.
    fn ifd(&mut self, offset: u64) -> Option<u64> {
        if self.ifds >= MAX_IFDS || offset < 8 {
            return None;
        }
        self.ifds += 1;
        let n = u64::from(self.order.u16(self.r.slice(self.start + offset, 2)?));
        if n == 0 || n > MAX_ENTRIES {
            return None;
        }
        let table = self
            .r
            .slice(self.start + offset + 2, usize::try_from(n * 12 + 4).ok()?)?
            .to_vec();
        if !self.reach(offset + 2 + n * 12 + 4) {
            return None;
        }
        let entries: Vec<Entry> = table
            .chunks_exact(12)
            .map(|e| Entry {
                tag: self.order.u16(e),
                kind: self.order.u16(e.get(2..).unwrap_or_default()),
                count: u64::from(self.order.u32(e.get(4..).unwrap_or_default())),
                field: e
                    .get(8..12)
                    .and_then(|b| b.try_into().ok())
                    .unwrap_or_default(),
            })
            .collect();
        let mut parts: [Vec<u64>; 6] = Default::default();
        let mut subs = Vec::new();
        let mut compression = 1;
        let mut bits = 0u64;
        let mut dims = (0u64, 0u64);
        for e in &entries {
            let Some(size) = e.size() else { continue };
            // A value over 4 bytes is stored elsewhere.
            if size > 4 {
                let at = u64::from(self.order.u32(&e.field));
                // Bogus pointers (Panasonic writes 0xFFFFFFFF) are not followed.
                self.reach(at.saturating_add(size));
            }
            match e.tag {
                // Strip and tile offsets and byte counts, JPEGInterchangeFormat and length.
                273 => parts[0] = self.values(e)?,
                279 => parts[1] = self.values(e)?,
                324 => parts[2] = self.values(e)?,
                325 => parts[3] = self.values(e)?,
                513 => parts[4] = self.values(e)?,
                514 => parts[5] = self.values(e)?,
                259 => compression = self.values(e)?.first().copied().unwrap_or(1),
                258 => bits = self.values(e)?.first().copied().unwrap_or(0),
                256 | 257 => {
                    let v = self.values(e)?.first().copied().unwrap_or(0);
                    if e.tag == 256 {
                        dims.0 = v;
                    } else {
                        dims.1 = v;
                    }
                    if self.ifds != 1 {
                        continue;
                    }
                    let v = u32::try_from(v).unwrap_or(0);
                    if e.tag == 256 {
                        self.width = v;
                    } else {
                        self.height = v;
                    }
                }
                271 => self.make = self.bytes(e).unwrap_or_default(),
                // DNGVersion.
                0xC612 => self.dng = true,
                // SubIFDs, Exif IFD, GPS IFD, Interoperability IFD.
                330 | 34665 | 34853 | 40965 => subs.extend(self.values(e)?),
                // Panasonic: sensor size, raw format and raw data offset.
                0x02 => self.rw2.width = self.values(e)?.first().copied().unwrap_or(0),
                0x03 => self.rw2.height = self.values(e)?.first().copied().unwrap_or(0),
                0x2D => self.rw2.format = self.values(e)?.first().copied().unwrap_or(0),
                0x118 => self.rw2.offset = self.values(e)?.first().copied().unwrap_or(0),
                _ => {}
            }
        }
        let [strips, strip_counts, tiles, tile_counts, jpeg_at, jpeg_len] = parts;
        // Uncompressed samples of 9 to 15 bits, each in a 16-bit word (little-endian files
        // only; the check reads them so).
        let total: u64 = strip_counts.iter().sum();
        let unpacked = compression == 1
            && (9..=15).contains(&bits)
            && dims.0 * dims.1 * 2 == total
            && matches!(self.order, Order::Little);
        let jpeg_data = matches!(compression, 6 | 7);
        let big = total >= LARGE || tile_counts.iter().sum::<u64>() >= LARGE;
        if big && !jpeg_data && !unpacked {
            self.unchecked = true;
        }
        if unpacked && strips.len() == strip_counts.len() {
            for (&o, &c) in strips.iter().zip(&strip_counts) {
                if o.checked_add(c).is_some_and(|e| e <= self.max_len) {
                    self.words.push((o, c, u32::try_from(bits).unwrap_or(16)));
                }
            }
        }
        for (offsets, counts) in [(strips, strip_counts), (tiles, tile_counts)] {
            for (&o, &c) in offsets.iter().zip(&counts) {
                if c == 0 {
                    continue;
                }
                let end = o.saturating_add(c);
                // JPEG-compressed strips and tiles (6 old-style, 7 JPEG) are checked.
                if self.reach(end) && matches!(compression, 6 | 7) && offsets.len() <= 4096 {
                    self.jpegs.push((o, c));
                }
            }
        }
        if let (Some(&o), Some(&l)) = (jpeg_at.first(), jpeg_len.first())
            && l > 0
            && self.reach(o.saturating_add(l))
        {
            self.jpegs.push((o, l));
        }
        for s in subs {
            // A sub-directory that does not parse leaves the rest as it is.
            let _ = self.chain_sub(s);
        }
        let next_at = self.start + offset + 2 + n * 12;
        Some(u64::from(self.order.u32(self.r.slice(next_at, 4)?)))
    }

    /// Follows a sub-directory (not its chain, which Exif-style IFDs do not have).
    fn chain_sub(&mut self, offset: u64) -> Option<()> {
        self.ifd(offset).map(|_| ())
    }

    /// The numeric values of an entry (BYTE, SHORT, LONG or IFD).
    fn values(&mut self, e: &Entry) -> Option<Vec<u64>> {
        let size = e.size()?;
        let count = e.count.min(MAX_PARTS);
        let data = if size <= 4 {
            e.field.to_vec()
        } else {
            let at = u64::from(self.order.u32(&e.field));
            let n = usize::try_from(count.checked_mul(size / e.count.max(1))?).ok()?;
            match self.r.slice(self.start + at, n) {
                Some(b) => b.to_vec(),
                None => return Some(Vec::new()),
            }
        };
        let order = self.order;
        let out = match e.kind {
            1 | 7 => data.iter().map(|&b| u64::from(b)).collect(),
            3 => data
                .chunks_exact(2)
                .map(|b| u64::from(order.u16(b)))
                .collect(),
            4 | 13 => data
                .chunks_exact(4)
                .map(|b| u64::from(order.u32(b)))
                .collect(),
            _ => Vec::new(),
        };
        Some(out)
    }

    /// The bytes of an ASCII or UNDEFINED entry.
    fn bytes(&mut self, e: &Entry) -> Option<Vec<u8>> {
        let size = e.size()?;
        if size <= 4 {
            return Some(e.field.get(..usize::try_from(size).ok()?)?.to_vec());
        }
        let at = u64::from(self.order.u32(&e.field));
        Some(
            self.r
                .slice(self.start + at, usize::try_from(size.min(256)).ok()?)?
                .to_vec(),
        )
    }

    /// Panasonic raw data: at RawDataOffset, sized by the sensor and the raw format (format
    /// 4 packs 14 pixels in 16 bytes; the data fills whole 8 KiB blocks).
    fn rw2_data(&mut self) {
        let Rw2 {
            offset,
            width,
            height,
            format,
        } = self.rw2;
        if offset == 0 || width == 0 || height == 0 {
            return;
        }
        let bytes = match format {
            4 => (width * height).div_ceil(14) * 16,
            // Other formats: 16 bits per pixel at most.
            _ => width * height * 2,
        };
        self.reach(offset + bytes.div_ceil(RW2_BLOCK) * RW2_BLOCK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian TIFF: IFD0 (Make, a strip, a JPEG preview), an Exif sub-IFD.
    fn tiff(make: &[u8], strip: usize) -> Vec<u8> {
        let mut v = b"II\x2A\x00\x08\x00\x00\x00".to_vec();
        let entries: u16 = 5;
        let ifd_len = 2 + usize::from(entries) * 12 + 4;
        let make_at = 8 + ifd_len;
        let exif_at = make_at + 16;
        let exif_len = 2 + 12 + 4;
        let preview_at = exif_at + exif_len;
        let preview = tiny_jpeg();
        let strip_at = preview_at + preview.len();
        v.extend(entries.to_le_bytes());
        let mut e = |tag: u16, kind: u16, count: u32, value: u32| {
            v.extend(tag.to_le_bytes());
            v.extend(kind.to_le_bytes());
            v.extend(count.to_le_bytes());
            v.extend(value.to_le_bytes());
        };
        let as32 = |x: usize| u32::try_from(x).unwrap();
        e(271, 2, 16, as32(make_at));
        e(273, 4, 1, as32(strip_at));
        e(279, 4, 1, as32(strip));
        e(513, 4, 1, as32(preview_at));
        e(514, 4, 1, as32(preview.len()));
        v.extend(0u32.to_le_bytes());
        let mut m = make.to_vec();
        m.resize(16, 0);
        v.extend(m);
        // An empty-ish Exif IFD with one entry.
        v.extend(1u16.to_le_bytes());
        v.extend(0x9000u16.to_le_bytes());
        v.extend(7u16.to_le_bytes());
        v.extend(4u32.to_le_bytes());
        v.extend(b"0232");
        v.extend(0u32.to_le_bytes());
        v.extend(preview);
        v.extend(vec![0x33; strip]);
        v
    }

    /// The smallest baseline JPEG the JPEG carver accepts: one 8x8 gray block.
    fn tiny_jpeg() -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        j.extend([0xFF, 0xDB, 0x00, 0x43, 0x00]);
        j.extend([1u8; 64]);
        j.extend([
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00,
        ]);
        j.extend([0xFF, 0xC4, 0x00, 0x14, 0x00, 1]);
        j.extend([0u8; 15]);
        j.push(0);
        j.extend([0xFF, 0xC4, 0x00, 0x14, 0x10, 1]);
        j.extend([0u8; 15]);
        j.push(0);
        j.extend([0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        // DC category 0 (code 0), then EOB (code 0): two bits, padded with ones.
        j.push(0x3F);
        j.extend([0xFF, 0xD9]);
        j
    }

    fn run(data: Vec<u8>) -> Option<Raw> {
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        carve(&mut r, 0, 1 << 30)
    }

    #[test]
    fn length_comes_from_the_furthest_strip() {
        let t = tiff(b"NIKON CORPORATION", 5000);
        let got = run([t.clone(), vec![0x77; 3000]].concat()).unwrap();
        assert_eq!(got.kind, RawKind::Nef);
        assert_eq!(got.len, t.len() as u64);
        assert_eq!(got.status, Status::Complete);
    }

    #[test]
    fn a_broken_preview_is_reported() {
        let mut t = tiff(b"SONY", 5000);
        let at = t.windows(2).position(|w| w == [0xFF, 0xDA]).unwrap();
        t[at + 10] = 0xFF;
        t[at + 11] = 0x00;
        let got = run(t).unwrap();
        assert_eq!(got.kind, RawKind::Arw);
        assert!(matches!(got.status, Status::Corrupt { .. }), "{got:?}");
    }

    #[test]
    fn a_cut_off_file_is_truncated() {
        let t = tiff(b"Canon", 50_000);
        let got = run(t[..t.len() - 1000].to_vec()).unwrap();
        assert_eq!(got.status, Status::Truncated);
    }

    #[test]
    fn not_tiff() {
        assert!(run(vec![0; 64]).is_none());
        assert!(run(b"II\x2A\x00\x00\x00\x00\x00".to_vec()).is_none());
    }
}
