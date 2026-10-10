//! Physical disk path test (Linux, for CI).
//!
//! Attaches an image as a loop device and reads it with the production [`Device`] code.
//! Ignored by default since it needs `sudo`: `cargo test -p blocksource -- --ignored`
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

mod common;

use std::{io::Write, path::Path, process::Command};

use blocksource::{Device, ImageFile};

/// An attached loop device, detached on drop.
struct Loop {
    dev: String,
}

impl Loop {
    fn new(image: &Path) -> Self {
        let out = Command::new("sudo")
            .args(["losetup", "--find", "--show", "--read-only"])
            .arg(image)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let dev = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        let ok = Command::new("sudo")
            .args(["chmod", "a+r", &dev])
            .status()
            .unwrap();
        assert!(ok.success());
        Self { dev }
    }
}

impl Drop for Loop {
    fn drop(&mut self) {
        let _ = Command::new("sudo")
            .args(["losetup", "-d", &self.dev])
            .status();
    }
}

#[test]
#[ignore = "attaches a loop device with sudo"]
fn loop_device_matches_image() {
    let data = common::pattern(8 << 20);
    let mut img = tempfile::NamedTempFile::new().unwrap();
    img.write_all(&data).unwrap();
    img.flush().unwrap();

    let attached = Loop::new(img.path());
    let dev = Device::open(&attached.dev).unwrap();
    common::check_device(&dev, &data, &ImageFile::open(img.path()).unwrap());
}
