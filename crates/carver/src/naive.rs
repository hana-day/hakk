//! The most naive carving possible (a baseline for comparison).
//!
//! Finds JPEG starts (`FF D8 FF`) on sector boundaries and cuts up to the first end marker
//! (`FF D9`). It does not handle fragmentation, nor false end markers (embedded thumbnails,
//! or a stray `FF D9` inside a comment).
//!
//! Reference: ITU-T T.81 Annex B (JPEG markers).

use blocksource::BlockSource;

use crate::{Found, Kind, Status};

const SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];
const EOI: [u8; 2] = [0xFF, 0xD9];
const CHUNK: usize = 4 << 20;

/// Searches all of `src` for JPEGs, skipping ranges that cannot be read.
pub fn scan(src: &impl BlockSource, max_len: u64) -> Vec<Found> {
    let ss = u64::from(src.sector_size());
    let mut found = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    let mut pos = 0u64;
    // End of the last file found (we do not search inside it).
    let mut skip_until = 0u64;
    while pos < src.len() {
        let Ok(n) = src.read_at(pos, &mut buf) else {
            pos += CHUNK as u64;
            continue;
        };
        if n == 0 {
            break;
        }
        let data = buf.get(..n).unwrap_or_default();
        let mut off = 0usize;
        while off < n {
            let at = pos + off as u64;
            if at >= skip_until
                && data.get(off..off + SOI.len()) == Some(&SOI)
                && let Some(len) = find_eoi(src, at, max_len)
            {
                found.push(Found {
                    offset: at,
                    len,
                    kind: Kind::Jpeg,
                    status: Status::Complete,
                });
                skip_until = at + len;
            }
            off += usize::try_from(ss).unwrap_or(512);
        }
        pos += n as u64;
    }
    found
}

/// Length from `start` through the first `FF D9`, or `None` if not found within `max_len`.
fn find_eoi(src: &impl BlockSource, start: u64, max_len: u64) -> Option<u64> {
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    let mut prev_ff = false;
    while done < max_len {
        let n = src.read_at(start + done, &mut buf).ok()?;
        if n == 0 {
            return None;
        }
        for (i, &b) in buf.get(..n)?.iter().enumerate() {
            let at = done + i as u64;
            // Skip the leading FF D8 itself.
            if prev_ff && b == EOI[1] && at >= 2 {
                return Some(at + 1);
            }
            prev_ff = b == EOI[0];
        }
        done += n as u64;
    }
    None
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    #[test]
    fn finds_contiguous_jpegs() {
        let mut disk = vec![0u8; 64 * 1024];
        let jpeg = |len: usize| {
            let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
            v.resize(len - 2, 0x11);
            v.extend_from_slice(&EOI);
            v
        };
        let a = jpeg(3000);
        let b = jpeg(5000);
        disk[1024..1024 + a.len()].copy_from_slice(&a);
        disk[8192..8192 + b.len()].copy_from_slice(&b);
        // Starts that are not on a sector boundary are ignored.
        disk[20_001..20_004].copy_from_slice(&SOI);

        let found = scan(&MemSource::new(disk), 1 << 20);
        assert_eq!(
            found,
            [
                Found {
                    offset: 1024,
                    len: 3000,
                    kind: Kind::Jpeg,
                    status: Status::Complete,
                },
                Found {
                    offset: 8192,
                    len: 5000,
                    kind: Kind::Jpeg,
                    status: Status::Complete,
                },
            ]
        );
    }
}
