// Test code: relax the strict lints meant for library code.
#![allow(clippy::cast_possible_truncation, clippy::indexing_slicing)]

use std::{io::Write, time::Duration};

use blocksource::{
    Aligned, BlockSource, ImageFile, MemSource, ReadErrorKind,
    sim::{FaultKind, Faulty, Latency, StrictAlign},
};
use proptest::prelude::*;

/// Deterministic bytes from a seed.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ (i >> 8) as u8)
        .collect()
}

/// Reads `len` bytes of `src` at `offset`.
fn read(src: &impl BlockSource, offset: u64, len: usize) -> Result<Vec<u8>, ReadErrorKind> {
    let mut buf = vec![0xAA; len];
    let n = src.read_at(offset, &mut buf).map_err(|e| e.into_kind())?;
    buf.truncate(n);
    Ok(buf)
}

/// Expected result: the same range sliced directly.
fn expected(data: &[u8], offset: u64, len: usize) -> Vec<u8> {
    let start = (offset as usize).min(data.len());
    let end = start.saturating_add(len).min(data.len());
    data[start..end].to_vec()
}

fn sector_size() -> impl Strategy<Value = u32> {
    prop_oneof![Just(512u32), Just(4096u32)]
}

proptest! {
    #[test]
    fn mem_matches_slice(len in 0usize..20_000, offset in 0u64..25_000, n in 0usize..10_000) {
        let data = pattern(len, 1);
        let src = MemSource::new(data.clone());
        prop_assert_eq!(read(&src, offset, n).unwrap(), expected(&data, offset, n));
    }

    #[test]
    fn image_file_matches_slice(len in 0usize..20_000, offset in 0u64..25_000, n in 0usize..10_000) {
        let data = pattern(len, 2);
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&data).unwrap();
        let src = ImageFile::open(f.path()).unwrap();
        prop_assert_eq!(src.len(), len as u64);
        prop_assert_eq!(read(&src, offset, n).unwrap(), expected(&data, offset, n));
    }

    /// Misaligned reads through Aligned work even under raw-device constraints.
    #[test]
    fn aligned_over_strict_matches_slice(
        ss in sector_size(),
        sectors in 0usize..24,
        offset in 0u64..110_000,
        n in 0usize..40_000,
    ) {
        let data = pattern(sectors * ss as usize, 3);
        let src = Aligned::new(StrictAlign::new(MemSource::new(data.clone()).with_sector_size(ss)));
        prop_assert_eq!(read(&src, offset, n).unwrap(), expected(&data, offset, n));
    }

    /// With no faults, Faulty is transparent.
    #[test]
    fn faulty_without_faults_is_transparent(len in 0usize..20_000, offset in 0u64..25_000, n in 0usize..10_000) {
        let data = pattern(len, 4);
        let src = Faulty::new(MemSource::new(data.clone())).latency(Latency::HDD);
        prop_assert_eq!(read(&src, offset, n).unwrap(), expected(&data, offset, n));
    }

    /// Only reads touching bad sectors fail, and the reported range lies within the
    /// overlap of the request and the bad range.
    #[test]
    fn faulty_fails_exactly_on_bad_sectors(
        seed: u64,
        offset in 0u64..64 * 512,
        n in 1usize..8 * 512,
    ) {
        let data = pattern(64 * 512, 5);
        let src = Faulty::new(MemSource::new(data.clone())).random_bad(seed, 4, 3);
        let end = (offset + n as u64).min(src.len());
        let (s0, s1) = (offset / 512, end.div_ceil(512));
        let touches = offset < end
            && src.faults().iter().any(|f| f.sectors.start < s1 && s0 < f.sectors.end);

        let mut buf = vec![0; n];
        match src.read_at(offset, &mut buf) {
            Ok(got) => {
                prop_assert!(!touches);
                prop_assert_eq!(&buf[..got], &expected(&data, offset, n)[..]);
            }
            Err(e) => {
                prop_assert!(touches);
                let r = e.range();
                prop_assert!(r.start < r.end);
                prop_assert!(s0 * 512 <= r.start && r.end <= s1 * 512);
                let inside_bad = src.faults().iter()
                    .any(|f| f.sectors.start * 512 <= r.start && r.end <= f.sectors.end * 512);
                prop_assert!(inside_bad);
            }
        }
    }
}

#[test]
fn read_exact_at_reports_eof() {
    let src = MemSource::new(pattern(1000, 0));
    let mut buf = [0; 100];
    src.read_exact_at(900, &mut buf).unwrap();
    let err = src.read_exact_at(950, &mut buf).unwrap_err();
    assert_eq!(err.range(), 1000..1050);
    assert!(matches!(err.kind(), ReadErrorKind::UnexpectedEof));
}

#[test]
fn strict_align_rejects_misaligned() {
    let src = StrictAlign::new(MemSource::new(pattern(4096, 0)));
    assert!(read(&src, 512, 1024).is_ok());
    assert!(matches!(read(&src, 1, 512), Err(ReadErrorKind::Misaligned)));
    assert!(matches!(read(&src, 0, 100), Err(ReadErrorKind::Misaligned)));
}

#[test]
fn weak_sectors_recover_after_retries() {
    let src =
        Faulty::new(MemSource::new(pattern(4096, 0))).fault(2..3, FaultKind::Weak { failures: 2 });
    assert!(read(&src, 1024, 512).is_err());
    assert!(read(&src, 1024, 512).is_err());
    assert_eq!(
        read(&src, 1024, 512).unwrap(),
        expected(&pattern(4096, 0), 1024, 512)
    );
    // Ranges not touching the fault read fine from the start.
    assert!(read(&src, 0, 1024).is_ok());
}

#[test]
fn error_range_is_clamped_to_request() {
    let src = Faulty::new(MemSource::new(pattern(8192, 0))).bad(2..10);
    let err = src.read_at(3 * 512 + 7, &mut [0; 512]).unwrap_err();
    assert_eq!(err.range(), 3 * 512..5 * 512);
}

#[test]
fn timeout_costs_time() {
    let src = Faulty::new(MemSource::new(pattern(4096, 0)))
        .fault(0..1, FaultKind::Timeout)
        .latency(Latency {
            timeout: Duration::from_secs(5),
            ..Latency::NONE
        });
    assert!(matches!(read(&src, 0, 512), Err(ReadErrorKind::Timeout)));
    assert_eq!(src.clock().elapsed(), Duration::from_secs(5));
}

#[test]
fn seek_cost_only_on_jumps() {
    let lat = Latency {
        seek: Duration::from_millis(8),
        seek_threshold: 1 << 20,
        ..Latency::NONE
    };
    let src = Faulty::new(MemSource::new(vec![0; 16 << 20])).latency(lat);
    // Sequential reads from the start: no seeks.
    for i in 0..8u64 {
        read(&src, i << 20, 1 << 20).unwrap();
    }
    assert_eq!(src.clock().elapsed(), Duration::ZERO);
    // Jump far away and back: two seeks.
    read(&src, 15 << 20, 4096).unwrap();
    read(&src, 8 << 20, 4096).unwrap();
    assert_eq!(src.clock().elapsed(), Duration::from_millis(16));
}

#[test]
fn transfer_time_follows_throughput() {
    let lat = Latency {
        bytes_per_sec: 100_000_000,
        ..Latency::NONE
    };
    let src = Faulty::new(MemSource::new(vec![0; 10_000_000])).latency(lat);
    read(&src, 0, 10_000_000).unwrap();
    assert_eq!(src.clock().elapsed(), Duration::from_millis(100));
}

#[test]
fn random_bad_is_deterministic() {
    let make = |seed| Faulty::new(MemSource::new(vec![0; 1 << 20])).random_bad(seed, 20, 8);
    assert_eq!(make(42).faults(), make(42).faults());
    assert_ne!(make(42).faults(), make(43).faults());
    assert!(!make(42).faults().is_empty());
}

#[test]
#[should_panic(expected = "overlapping")]
fn overlapping_faults_are_rejected() {
    let _ = Faulty::new(MemSource::new(vec![0; 4096]))
        .bad(1..4)
        .bad(3..5);
}

#[test]
fn concurrent_reads() {
    let data = pattern(1 << 20, 9);
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&data).unwrap();
    let src = ImageFile::open(f.path()).unwrap();
    std::thread::scope(|s| {
        for t in 0..8u64 {
            let (src, data) = (&src, &data);
            s.spawn(move || {
                for i in 0..64 {
                    let off = (t * 64 + i) * 1024;
                    assert_eq!(read(src, off, 4096).unwrap(), expected(data, off, 4096));
                }
            });
        }
    });
}
