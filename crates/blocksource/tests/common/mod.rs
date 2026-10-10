//! Helpers shared by the raw-device tests.
#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

use blocksource::{Aligned, BlockSource, Device, ImageFile, ReadErrorKind};

/// Deterministic pseudo-random bytes (xorshift32).
pub fn pattern(len: usize) -> Vec<u8> {
    let mut x = 0x1234_5678_u32;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// Checks a raw device holding `data` against the same bytes read from `image`.
pub fn check_device(dev: &Device, data: &[u8], image: &ImageFile) {
    assert_eq!(dev.len(), data.len() as u64);
    let ss = dev.sector_size() as usize;
    assert!(ss == 512 || ss == 4096, "sector size {ss}");

    // Aligned reads work as is.
    let mut buf = vec![0; 1 << 20];
    for off in [0usize, ss, 3 << 20, data.len() - buf.len()] {
        dev.read_exact_at(off as u64, &mut buf).unwrap();
        assert_eq!(buf, data[off..off + buf.len()]);
    }

    // Misaligned reads are rejected without reaching the OS.
    let err = dev.read_at(1, &mut vec![0; ss]).unwrap_err();
    assert!(matches!(err.kind(), ReadErrorKind::Misaligned));
    let err = dev.read_at(0, &mut [0; 100]).unwrap_err();
    assert!(matches!(err.kind(), ReadErrorKind::Misaligned));

    // Reads crossing the end are short.
    let n = dev
        .read_at((data.len() - ss) as u64, &mut vec![0; ss * 8])
        .unwrap();
    assert_eq!(n, ss);

    // Through Aligned, any offset/length matches reading the image file.
    let aligned = Aligned::new(dev);
    let mut seed = 1u64;
    for _ in 0..500 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let off = (seed >> 33) % (data.len() as u64 + 1000);
        let len = ((seed >> 13) % 70_000) as usize;
        let (mut a, mut b) = (vec![0; len], vec![0; len]);
        let na = aligned.read_at(off, &mut a).unwrap();
        let nb = image.read_at(off, &mut b).unwrap();
        assert_eq!(na, nb, "off={off} len={len}");
        assert_eq!(a[..na], b[..nb]);
    }
}
