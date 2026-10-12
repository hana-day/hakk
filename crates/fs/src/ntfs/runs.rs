//! Data runs ("mapping pairs") of non-resident NTFS attributes, and reading through them.
//!
//! Reference: Brian Carrier, "File System Forensic Analysis" (2005), chapter 13, "Non-
//! Resident Attributes" (the run list), and Microsoft "ATTRIBUTE_RECORD_HEADER structure"
//! (learn.microsoft.com, MappingPairsOffset).
//!
//! Each run starts with a header byte: the low 4 bits give the size in bytes of the run's
//! length, the high 4 bits the size of its offset; a header of zero ends the list. The
//! length (unsigned) counts clusters. The offset (signed, two's complement) is the run's
//! first cluster relative to the previous run's first cluster; a run without an offset is
//! sparse (not stored, reads as zeros).

use crate::disk::Disk;

/// A run of a stream: `len` clusters from virtual cluster `vcn`, stored at logical cluster
/// `lcn`, or not stored at all (`None`: sparse).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) vcn: u64,
    pub(crate) len: u64,
    pub(crate) lcn: Option<u64>,
}

/// Why a run list could not be decoded completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunError {
    /// A field ran past the end of the attribute, or a header had invalid field sizes.
    Malformed,
    /// A run starts before cluster 0 or past `u64`, or has length zero.
    OutOfRange,
    /// The runs cover more clusters than the attribute says it spans.
    TooLong,
}

/// Decodes the run list in `pairs` for an attribute spanning `vcns` clusters from
/// `start_vcn`. Returns the runs decoded so far along with the first error, so a damaged
/// list still yields its valid prefix.
pub(crate) fn decode(pairs: &[u8], start_vcn: u64, vcns: u64) -> (Vec<Run>, Option<RunError>) {
    let mut runs = Vec::new();
    let mut at = 0usize;
    let mut vcn = start_vcn;
    let end_vcn = start_vcn.saturating_add(vcns);
    let mut lcn: i128 = 0;
    loop {
        let Some(&header) = pairs.get(at) else {
            // The list must end with a zero header; running off the attribute is damage
            // unless every cluster has been accounted for.
            let err = (vcn < end_vcn).then_some(RunError::Malformed);
            return (runs, err);
        };
        if header == 0 {
            return (runs, None);
        }
        let len_size = usize::from(header & 0x0F);
        let off_size = usize::from(header >> 4);
        if !(1..=8).contains(&len_size) || off_size > 8 {
            return (runs, Some(RunError::Malformed));
        }
        let fields = at + 1;
        let Some(len) = read_unsigned(pairs, fields, len_size) else {
            return (runs, Some(RunError::Malformed));
        };
        let delta = if off_size == 0 {
            None
        } else {
            match read_signed(pairs, fields + len_size, off_size) {
                Some(d) => Some(d),
                None => return (runs, Some(RunError::Malformed)),
            }
        };
        at = fields + len_size + off_size;
        if len == 0 {
            return (runs, Some(RunError::OutOfRange));
        }
        if vcn.checked_add(len).is_none_or(|e| e > end_vcn) {
            return (runs, Some(RunError::TooLong));
        }
        let start = match delta {
            Some(d) => {
                lcn += i128::from(d);
                match u64::try_from(lcn) {
                    Ok(l) if l.checked_add(len).is_some() => Some(l),
                    _ => return (runs, Some(RunError::OutOfRange)),
                }
            }
            None => None,
        };
        runs.push(Run {
            vcn,
            len,
            lcn: start,
        });
        vcn += len;
    }
}

fn read_unsigned(b: &[u8], at: usize, size: usize) -> Option<u64> {
    let bytes = b.get(at..at.checked_add(size)?)?;
    Some(
        bytes
            .iter()
            .rev()
            .fold(0u64, |v, &x| (v << 8) | u64::from(x)),
    )
}

fn read_signed(b: &[u8], at: usize, size: usize) -> Option<i64> {
    let v = read_unsigned(b, at, size)?;
    let shift = 64 - 8 * u32::try_from(size).ok()?;
    // Sign-extend from the top bit of the field.
    #[allow(clippy::cast_possible_wrap)]
    Some(((v << shift) as i64) >> shift)
}

/// Total clusters covered by `runs`.
pub(crate) fn clusters(runs: &[Run]) -> u64 {
    runs.iter().fold(0u64, |t, r| t.saturating_add(r.len))
}

/// Reads `len` bytes at byte `offset` of the stream described by `runs`. Sparse runs,
/// ranges no run covers and unreadable sectors read as zeros; unreadable sectors are
/// reported in `warnings`.
pub(crate) fn read(
    disk: &Disk,
    runs: &[Run],
    cluster_size: u64,
    offset: u64,
    len: usize,
    warnings: &mut Vec<String>,
) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let end = offset.saturating_add(len as u64);
    for r in runs {
        let r_start = r.vcn.saturating_mul(cluster_size);
        let r_end = r_start.saturating_add(r.len.saturating_mul(cluster_size));
        let (from, to) = (r_start.max(offset), r_end.min(end));
        if from >= to {
            continue;
        }
        let Some(lcn) = r.lcn else {
            continue;
        };
        let disk_at = lcn
            .saturating_mul(cluster_size)
            .saturating_add(from - r_start);
        let (Ok(dst), Ok(n)) = (usize::try_from(from - offset), usize::try_from(to - from)) else {
            continue;
        };
        let bytes = disk.read_lossy(disk_at, n, warnings);
        if let Some(slot) = dst.checked_add(n).and_then(|e| out.get_mut(dst..e)) {
            slot.copy_from_slice(&bytes);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    #[test]
    fn decodes_runs() {
        // 0x18 clusters at 0x5634, then 0x10 sparse, then 0x20 at 0x5634 - 0x100.
        let pairs = [
            0x21, 0x18, 0x34, 0x56, 0x01, 0x10, 0x21, 0x20, 0x00, 0xFF, 0x00,
        ];
        let (runs, err) = decode(&pairs, 0, 0x48);
        assert_eq!(err, None);
        assert_eq!(
            runs,
            [
                Run {
                    vcn: 0,
                    len: 0x18,
                    lcn: Some(0x5634)
                },
                Run {
                    vcn: 0x18,
                    len: 0x10,
                    lcn: None
                },
                Run {
                    vcn: 0x28,
                    len: 0x20,
                    lcn: Some(0x5534)
                },
            ]
        );
    }

    #[test]
    fn rejects_bad_runs() {
        // Runs before cluster 0.
        let (runs, err) = decode(&[0x11, 0x04, 0x10, 0x11, 0x04, 0xE0, 0x00], 0, 8);
        assert_eq!((runs.len(), err), (1, Some(RunError::OutOfRange)));
        // More clusters than the attribute spans.
        let (_, err) = decode(&[0x11, 0x09, 0x10, 0x00], 0, 8);
        assert_eq!(err, Some(RunError::TooLong));
        // Truncated field, and a bad header.
        assert_eq!(
            decode(&[0x21, 0x04, 0x10], 0, 8).1,
            Some(RunError::Malformed)
        );
        assert_eq!(decode(&[0x09, 0x04], 0, 8).1, Some(RunError::Malformed));
        // A missing terminator is fine once every cluster is accounted for.
        assert_eq!(decode(&[0x11, 0x08, 0x10], 0, 8).1, None);
        assert_eq!(decode(&[], 0, 0), (Vec::new(), None));
    }

    #[test]
    fn reads_across_runs() {
        let data: Vec<u8> = (0..4096u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let src = MemSource::new(data.clone());
        let disk = Disk::new(&src, 0, 4096);
        let runs = [
            Run {
                vcn: 0,
                len: 1,
                lcn: Some(3),
            },
            Run {
                vcn: 1,
                len: 1,
                lcn: None,
            },
            Run {
                vcn: 2,
                len: 1,
                lcn: Some(1),
            },
        ];
        let mut w = Vec::new();
        let got = read(&disk, &runs, 512, 256, 1024, &mut w);
        assert_eq!(&got[..256], &data[1792..2048]);
        assert!(got[256..768].iter().all(|&b| b == 0));
        assert_eq!(&got[768..], &data[512..768]);
        assert!(w.is_empty());
    }
}
