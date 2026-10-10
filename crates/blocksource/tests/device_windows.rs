//! Physical disk path test (Windows, for CI).
//!
//! Attaches a fixed VHD with `Mount-DiskImage` and reads `\\.\PhysicalDriveN` with the
//! production [`Device`] code. Ignored by default since it needs admin rights:
//! `cargo test -p blocksource -- --ignored`
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

mod common;

use std::{fs, path::Path, process::Command};

use blocksource::{Device, ImageFile};

fn powershell(script: &str) -> String {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("$ErrorActionPreference = 'Stop'\n{script}"),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// An attached VHD, detached on drop.
struct Vhd {
    path: String,
}

impl Vhd {
    /// Creates a fixed VHD whose disk contents are `data`, then attaches it read-only.
    /// Returns the physical drive number.
    fn new(dir: &Path, data: &[u8]) -> (Self, u32) {
        let path = dir.join("test.vhd");
        let script = dir.join("create.diskpart");
        fs::write(
            &script,
            format!(
                "create vdisk file=\"{}\" maximum={} type=fixed\r\n",
                path.display(),
                data.len() >> 20
            ),
        )
        .unwrap();
        let ok = Command::new("diskpart")
            .arg("/s")
            .arg(&script)
            .output()
            .unwrap();
        assert!(
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stdout)
        );

        // A fixed VHD is the raw disk followed by a 512-byte footer: overwrite the disk part.
        let mut vhd = fs::read(&path).unwrap();
        vhd[..data.len()].copy_from_slice(data);
        fs::write(&path, vhd).unwrap();

        let path = path.to_string_lossy().replace('\'', "''");
        let number = powershell(&format!(
            "(Mount-DiskImage -ImagePath '{path}' -Access ReadOnly -PassThru | Get-DiskImage).Number"
        ));
        (Self { path }, number.trim().parse().unwrap())
    }
}

impl Drop for Vhd {
    fn drop(&mut self) {
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-Command"])
            .arg(format!("Dismount-DiskImage -ImagePath '{}'", self.path))
            .status();
    }
}

#[test]
#[ignore = "attaches a VHD (needs admin)"]
fn physical_drive_matches_image() {
    let data = common::pattern(8 << 20);
    let dir = tempfile::tempdir().unwrap();
    let (_vhd, number) = Vhd::new(dir.path(), &data);
    let image = dir.path().join("image.img");
    fs::write(&image, &data).unwrap();

    let dev = Device::open(format!(r"\\.\PhysicalDrive{number}")).unwrap();
    common::check_device(&dev, &data, &ImageFile::open(&image).unwrap());
}
