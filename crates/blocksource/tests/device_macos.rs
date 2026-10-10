//! Physical disk path test (macOS).
//!
//! Attaches an image as a raw device with `hdiutil attach -nomount` and reads it with the
//! same [`Device`] code as in production. Ignored by default since it attaches a disk:
//! `cargo test -p blocksource -- --ignored`
#![cfg(target_os = "macos")]
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

mod common;

use std::{io::Write, path::Path, process::Command};

use blocksource::{Device, ImageFile};

/// An attached disk, detached on drop.
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

#[test]
#[ignore = "attaches a disk with hdiutil"]
fn raw_device_matches_image() {
    let data = common::pattern(8 << 20);
    let mut img = tempfile::NamedTempFile::new().unwrap();
    img.write_all(&data).unwrap();
    img.flush().unwrap();

    let attached = Attached::new(img.path());
    let dev = Device::open(attached.raw()).unwrap();
    common::check_device(&dev, &data, &ImageFile::open(img.path()).unwrap());
}

#[test]
fn opening_a_regular_file_as_device_fails() {
    let img = tempfile::NamedTempFile::new().unwrap();
    assert!(Device::open(img.path()).is_err());
}
