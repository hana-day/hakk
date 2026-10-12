//! PNG (`.png`) carving: screenshots and edited images.
//!
//! A PNG file is an 8-byte signature and a run of chunks (a 32-bit big-endian length, a
//! four-letter type, the data and a CRC-32 of type and data), from `IHDR` to `IEND`. Every
//! chunk's CRC is checked, so a file is complete only if all of its bytes are as written;
//! the first chunk that fails is where the file breaks (on disk, usually where a fragment
//! ends).
//!
//! A fragmented file is joined back together: the chunk that fails is looked for with its
//! remainder a whole number of clusters away, and taken where its CRC comes out right
//! (which also fixes on which cluster the cut lies) and the next chunk header follows.
//!
//! Reference: W3C, "Portable Network Graphics (PNG) Specification (Third Edition)" 5.2
//! (signature), 5.3 (chunk layout), 5.4 (chunk naming), 5.5 and Annex D (CRC-32), 11.2.1
//! (IHDR), 11.2.5 (IEND).

use blocksource::BlockSource;

use crate::{fragments, pieces::Pieces, reader::Reader};

/// The PNG signature.
pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// How far from the end of a fragment (either way) to look for the next one.
const JOIN_REACH: u64 = 64 << 20;
/// Most fragments to join.
const MAX_FRAGMENTS: usize = 256;
/// Bytes hashed per read.
const STEP: usize = 64 << 10;

/// A PNG file found at some offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Png {
    /// Bytes from the start that hold together (all of them if complete): for a broken
    /// file, up to the end of the last chunk whose CRC is right.
    pub len: u64,
    pub status: Status,
    /// `(source offset, length)` of each piece in file order; one piece if contiguous.
    pub fragments: Vec<(u64, u64)>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Every chunk through `IEND` is intact.
    Complete,
    /// A chunk that is not intact at this source offset.
    Corrupt { at: u64 },
    /// The data ran out.
    Truncated,
}

/// Whether `head` starts a PNG file.
pub fn looks_like_png(head: &[u8]) -> bool {
    head.starts_with(&SIGNATURE)
}

/// Follows the file at `start` without joining fragments. `None` if it is not a PNG file.
pub fn carve<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Png> {
    follow(r, start, max_len, None)
}

/// Follows the file at `start`, joining the fragments it finds when `join` is set (its grid;
/// the reach and fragment count are this module's own).
pub fn follow<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    start: u64,
    max_len: u64,
    join: Option<&fragments::Options>,
) -> Option<Png> {
    if !looks_like_png(r.slice(start, 8)?) {
        return None;
    }
    let mut pieces = Pieces::new(start);
    // The first chunk must be IHDR: that tells a PNG from bytes that merely start alike.
    let ihdr = header(r, &pieces, 8)?;
    if &ihdr.kind != b"IHDR" || ihdr.len != 13 || !intact(r, &pieces, 8, ihdr.len)? {
        return None;
    }
    let mut dims = [0u8; 8];
    pieces.read(r, 16, &mut dims)?;
    let [w0, w1, w2, w3, h0, h1, h2, h3] = dims;
    let (width, height) = (
        u32::from_be_bytes([w0, w1, w2, w3]),
        u32::from_be_bytes([h0, h1, h2, h3]),
    );

    let mut pos = 8u64;
    let status = loop {
        let Some(h) = header(r, &pieces, pos) else {
            break Status::Truncated;
        };
        let fits = h.valid() && pos + 12 + h.len <= max_len;
        let good = if fits {
            match intact(r, &pieces, pos, h.len) {
                Some(ok) => ok,
                None => break Status::Truncated,
            }
        } else {
            false
        };
        if good {
            pos += 12 + h.len;
            if &h.kind == b"IEND" {
                break Status::Complete;
            }
            continue;
        }
        let joined = join
            .filter(|_| pieces.count() < MAX_FRAGMENTS)
            .and_then(|o| join_at(r, &pieces, pos, fits.then_some(h.len), max_len, o));
        match joined {
            Some(next) => pieces = next,
            None => {
                break Status::Corrupt {
                    at: pieces.physical(pos),
                };
            }
        }
    };
    Some(Png {
        len: pos,
        status,
        fragments: pieces.fragments(pos),
        width,
        height,
    })
}

/// After the chunk at logical offset `at` failed (its length `len` if its header looked
/// right), looks for the fragment the file goes on in: the cut lies on the grid within the
/// chunk, and a candidate is taken if the chunk comes out intact through it and the next
/// chunk header follows.
fn join_at<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    pieces: &Pieces,
    at: u64,
    len: Option<u64>,
    max_len: u64,
    o: &fragments::Options,
) -> Option<Pieces> {
    let grid = o.grid.max(512);
    // A header that does not look right was cut before its type field; otherwise the cut
    // lies anywhere up to the CRC's end.
    let last = match len {
        Some(l) => at + 12 + l,
        None => at + 4,
    };
    let first = at.max(pieces.open_from() + 1);
    let cuts: Vec<u64> = (first.div_ceil(grid)..=last / grid)
        .map(|k| k * grid)
        .collect();
    if cuts.is_empty() {
        return None;
    }
    // Where to look first: a type field that lies past every cut.
    let probe = match len {
        Some(l) => at + 12 + l + 4,
        None => at + 4,
    };
    let from = pieces.physical(probe);
    for k in 1..=JOIN_REACH.max(o.reach) / grid {
        for up in [true, false] {
            let moved = |p: u64| {
                if up {
                    p.checked_add(k * grid)
                } else {
                    p.checked_sub(k * grid)
                }
            };
            let Some(probe_at) = moved(from) else {
                continue;
            };
            let kind = r
                .slice(probe_at, 4)
                .and_then(|b| <[u8; 4]>::try_from(b).ok());
            if !kind.is_some_and(|k| valid_type(&k)) {
                continue;
            }
            for &c in &cuts {
                let Some(to) = moved(pieces.physical(c)) else {
                    continue;
                };
                if pieces.overlaps(to, at) {
                    continue;
                }
                let cand = pieces.cut(c, to);
                let Some(h) = header(r, &cand, at) else {
                    continue;
                };
                if !h.valid() || at + 12 + h.len > max_len {
                    continue;
                }
                if intact(r, &cand, at, h.len) != Some(true) {
                    continue;
                }
                let next = at + 12 + h.len;
                let follows =
                    &h.kind == b"IEND" || header(r, &cand, next).is_some_and(|n| n.valid());
                if follows {
                    return Some(cand);
                }
            }
        }
    }
    None
}

/// A chunk header: data length and type.
struct Header {
    len: u64,
    kind: [u8; 4],
}

impl Header {
    fn valid(&self) -> bool {
        self.len <= 0x7FFF_FFFF && valid_type(&self.kind)
    }
}

/// Chunk types are four ASCII letters (5.4).
fn valid_type(kind: &[u8; 4]) -> bool {
    kind.iter().all(u8::is_ascii_alphabetic)
}

fn header<S: BlockSource + ?Sized>(r: &mut Reader<S>, pieces: &Pieces, at: u64) -> Option<Header> {
    let mut h = [0u8; 8];
    pieces.read(r, at, &mut h)?;
    let [l0, l1, l2, l3, t0, t1, t2, t3] = h;
    Some(Header {
        len: u64::from(u32::from_be_bytes([l0, l1, l2, l3])),
        kind: [t0, t1, t2, t3],
    })
}

/// Whether the chunk at `at` with `len` data bytes has the right CRC; `None` if the data
/// runs out.
fn intact<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    pieces: &Pieces,
    at: u64,
    len: u64,
) -> Option<bool> {
    let mut crc = Crc::new();
    let mut buf = vec![0u8; STEP];
    let mut pos = at + 4;
    let end = at + 8 + len;
    while pos < end {
        let n = usize::try_from((end - pos).min(STEP as u64)).ok()?;
        let part = buf.get_mut(..n)?;
        pieces.read(r, pos, part)?;
        crc.update(part);
        pos += n as u64;
    }
    let mut stored = [0u8; 4];
    pieces.read(r, end, &mut stored)?;
    Some(crc.finish() == u32::from_be_bytes(stored))
}

/// CRC-32 as PNG uses it (Annex D): polynomial 0xEDB88320 (reflected), initial value and
/// final XOR all ones.
struct Crc(u32);

// Built at compile time over a fixed range, so the indexing and the casts are in bounds.
#[allow(clippy::indexing_slicing, clippy::cast_possible_truncation)]
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

impl Crc {
    fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    fn update(&mut self, data: &[u8]) {
        let mut c = self.0;
        for &b in data {
            let i = usize::from(u8::try_from(c & 0xFF).unwrap_or(0) ^ b);
            c = CRC_TABLE.get(i).copied().unwrap_or(0) ^ (c >> 8);
        }
        self.0 = c;
    }

    fn finish(&self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        v.extend(kind);
        v.extend(data);
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(data);
        v.extend(crc.finish().to_be_bytes());
        v
    }

    /// A PNG with `idat` IDAT chunks of `size` pseudo-random bytes each.
    fn png(idat: usize, size: usize) -> Vec<u8> {
        let mut ihdr = Vec::new();
        ihdr.extend(640u32.to_be_bytes());
        ihdr.extend(480u32.to_be_bytes());
        ihdr.extend([8, 2, 0, 0, 0]);
        let mut v = SIGNATURE.to_vec();
        v.extend(chunk(b"IHDR", &ihdr));
        let mut x = 0x1234_5678u32;
        for _ in 0..idat {
            let data: Vec<u8> = (0..size)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x.to_le_bytes()[0]
                })
                .collect();
            v.extend(chunk(b"IDAT", &data));
        }
        v.extend(chunk(b"IEND", &[]));
        v
    }

    fn run(data: Vec<u8>, join: bool) -> Option<Png> {
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        let o = fragments::Options::default();
        follow(&mut r, 0, 1 << 30, join.then_some(&o))
    }

    fn noise(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| u8::try_from(i * 7 % 251).unwrap() ^ 0x5A)
            .collect()
    }

    #[test]
    fn crc_matches_the_spec_example() {
        // The CRC of an empty IEND chunk is the well-known AE 42 60 82.
        let mut c = Crc::new();
        c.update(b"IEND");
        assert_eq!(c.finish(), 0xAE42_6082);
    }

    #[test]
    fn a_whole_file_is_complete() {
        let p = png(10, 8192);
        let got = run([p.clone(), vec![0; 1000]].concat(), false).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(got.len, p.len() as u64);
        assert_eq!((got.width, got.height), (640, 480));
    }

    #[test]
    fn a_damaged_chunk_is_found() {
        let mut p = png(10, 8192);
        p[30_000] ^= 1;
        let got = run(p, false).unwrap();
        assert!(matches!(got.status, Status::Corrupt { .. }));
        assert!(got.len < 30_000);
    }

    #[test]
    fn fragments_are_joined_in_order_and_out_of_order() {
        // Parts 0, 2, 1 on disk, each starting on a 4 KiB cluster.
        let p = png(20, 8192);
        let (a, b) = (5 * 4096, 20 * 4096);
        let tail = p.len() - b;
        let gap = 4096 + (4096 - tail % 4096);
        let data = [
            &p[..a],
            &noise(4096),
            &p[b..],
            &noise(gap),
            &p[a..b],
            &noise(4096),
        ]
        .concat();
        let got = run(data, true).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(
            got.fragments,
            vec![
                (0, a as u64),
                ((a + 4096 + tail + gap) as u64, (b - a) as u64),
                ((a + 4096) as u64, tail as u64),
            ]
        );
    }

    #[test]
    fn not_png() {
        assert!(run(vec![0; 100], false).is_none());
        // The signature but no IHDR.
        assert!(run([&SIGNATURE[..], &[0; 100]].concat(), false).is_none());
    }
}
