//! 物理ディスク経路のテスト（macOS）。
//!
//! `hdiutil attach -nomount` でイメージを生デバイスとして接続し、本番と同じ
//! [`Device`] のコードで読む。ディスクを接続するので既定では実行しない：
//! `cargo test -p blocksource -- --ignored`
#![cfg(target_os = "macos")]
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

use std::{io::Write, path::Path, process::Command};

use blocksource::{Aligned, BlockSource, Device, ImageFile, ReadErrorKind};

/// 接続したディスク。drop で切り離す。
struct Attached {
    disk: String,
}

impl Attached {
    fn new(image: &Path) -> Self {
        let out = Command::new("hdiutil")
            .args([
                "attach",
                "-nomount",
                "-imagekey",
                "diskimage-class=CRawDiskImage",
            ])
            .arg(image)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let disk = String::from_utf8(out.stdout)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        assert!(disk.starts_with("/dev/disk"), "{disk}");
        Self { disk }
    }

    fn raw(&self) -> String {
        self.disk.replace("/dev/disk", "/dev/rdisk")
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force", &self.disk])
            .status();
    }
}

fn pattern(len: usize) -> Vec<u8> {
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

#[test]
#[ignore = "hdiutil でディスクを接続する"]
fn raw_device_matches_image() {
    let data = pattern(8 << 20);
    let mut img = tempfile::NamedTempFile::new().unwrap();
    img.write_all(&data).unwrap();
    img.flush().unwrap();

    let attached = Attached::new(img.path());
    let dev = Device::open(attached.raw()).unwrap();
    assert_eq!(dev.len(), data.len() as u64);
    assert_eq!(dev.sector_size(), 512);

    // 揃った読み込みはそのまま読める
    let mut buf = vec![0; 1 << 20];
    for off in [0u64, 512, 3 << 20, (8 << 20) - (1 << 20)] {
        dev.read_exact_at(off, &mut buf).unwrap();
        assert_eq!(buf, data[off as usize..off as usize + buf.len()]);
    }

    // 揃っていない読み込みは OS に渡さず拒否する
    let err = dev.read_at(1, &mut [0; 512]).unwrap_err();
    assert!(matches!(err.kind(), ReadErrorKind::Misaligned));
    let err = dev.read_at(0, &mut [0; 100]).unwrap_err();
    assert!(matches!(err.kind(), ReadErrorKind::Misaligned));

    // 終端をまたぐ読み込みは短く返る
    let n = dev.read_at((8 << 20) - 512, &mut [0; 4096]).unwrap();
    assert_eq!(n, 512);

    // Aligned を被せれば、任意の位置・長さでイメージファイルと同じ結果になる
    let aligned = Aligned::new(&dev);
    let image = ImageFile::open(img.path()).unwrap();
    let mut seed = 1u64;
    for _ in 0..500 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let off = (seed >> 33) % (data.len() as u64 + 1000);
        let len = ((seed >> 13) % 70_000) as usize;
        let (mut a, mut b) = (vec![0; len], vec![0; len]);
        let na = aligned.read_at(off, &mut a).unwrap();
        let nb = image.read_at(off, &mut b).unwrap();
        assert_eq!(na, nb, "off={off} len={len}");
        assert_eq!(a[..na], b[..nb]);
    }
}

#[test]
fn opening_a_regular_file_as_device_fails() {
    let img = tempfile::NamedTempFile::new().unwrap();
    assert!(Device::open(img.path()).is_err());
}
