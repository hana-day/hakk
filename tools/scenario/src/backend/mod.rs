//! Drives file systems through the real OS drivers.
//!
//! - macOS: `hdiutil` + `newfs_exfat` (no admin rights needed)
//! - Linux: `mkfs.exfat` + loop mount (needs `sudo`; for CI)
//! - Windows: fixed-size VHD + `Mount-DiskImage` (needs admin; creates one MBR partition)

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::scenario::FsKind;

/// Creates file systems on image files and mounts them.
pub trait Backend {
    /// Creates an empty disk of `size` bytes in `dir` and makes a file system on it.
    /// Returns the working file used from then on; its first `size` bytes are the disk
    /// contents (some formats, like VHD, append metadata at the end).
    fn create(&self, dir: &Path, size: u64, fs: FsKind, label: &str) -> Result<PathBuf> {
        let image = dir.join("work.img");
        // A sparse, zero-filled file (does not eat disk space).
        File::create(&image)?.set_len(size)?;
        self.format(&image, fs, label)?;
        Ok(image)
    }

    /// Quick-formats the volume (leaving existing data areas intact).
    fn format(&self, image: &Path, fs: FsKind, label: &str) -> Result<()>;
    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>>;
}

/// A mounted volume.
pub trait Mounted {
    fn root(&self) -> &Path;
    /// Flushes writes to disk and detaches.
    fn unmount(self: Box<Self>) -> Result<()>;
}

/// The implementation for this OS.
pub fn native() -> Result<Box<dyn Backend>> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(macos::Hdiutil));
    #[cfg(target_os = "linux")]
    return Ok(Box::new(linux::Loop));
    #[cfg(windows)]
    return Ok(Box::new(windows::Vhd));
    #[allow(unreachable_code)]
    {
        bail!("unsupported OS")
    }
}

/// Reads the disk contents (the first `size` bytes) from the working file.
pub fn read_disk(image: &Path, size: u64) -> Result<Vec<u8>> {
    let mut data = fs::read(image).with_context(|| format!("cannot read {}", image.display()))?;
    data.truncate(usize::try_from(size)?);
    Ok(data)
}

/// Runs a command and returns its stdout. On failure, reports stderr too.
pub(crate) fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("cannot start {cmd:?}"))?;
    if !out.status.success() {
        bail!(
            "{cmd:?} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(target_os = "linux")]
pub(crate) fn temp_mount_dir() -> Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("hakk-mnt-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
