//! Drives file systems through the real OS drivers.
//!
//! - macOS: `hdiutil` + `newfs_exfat` / `newfs_msdos` (no admin rights needed)
//! - Linux: `mkfs.exfat` / `mkfs.fat` + loop devices (needs `sudo`; for CI)
//! - Windows: fixed-size VHD + `Mount-DiskImage` (needs admin; always one MBR partition)

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::scenario::FsKind;

/// How the disk is partitioned and formatted.
#[derive(Debug, Clone)]
pub struct Format {
    pub fs: FsKind,
    pub label: String,
    /// Disk size in bytes.
    pub size: u64,
    /// Cluster size in bytes; the formatter's default if absent.
    pub cluster: Option<u64>,
    /// Start sector of a single MBR partition; the whole disk is one volume if absent.
    pub partition_start: Option<u64>,
    /// MBR partition type byte; the usual one for `fs` if absent.
    pub partition_type: Option<u8>,
}

impl Format {
    /// The volume's byte offset and length on the disk.
    pub fn volume(&self) -> (u64, u64) {
        match self.partition_start {
            Some(start) => (start * 512, self.size - start * 512),
            None => (0, self.size),
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    pub fn sectors_per_cluster(&self) -> Option<u64> {
        self.cluster.map(|c| (c / 512).max(1))
    }

    /// MBR partition type byte. Reference: the conventional MBR partition type IDs.
    fn partition_type(&self) -> u8 {
        let (_, len) = self.volume();
        if let Some(t) = self.partition_type {
            return t;
        }
        match self.fs {
            FsKind::Fat12 => 0x01,
            FsKind::Fat16 if len < 32 << 20 => 0x04,
            FsKind::Fat16 => 0x06,
            FsKind::Fat32 => 0x0C,
            FsKind::Exfat | FsKind::None => 0x07,
        }
    }
}

/// Creates file systems on image files and mounts them.
pub trait Backend {
    /// Creates an empty disk in `dir`, partitions it if asked, and makes a file system.
    /// Returns the working file used from then on; its first `size` bytes are the disk
    /// contents (some formats, like VHD, append metadata at the end).
    fn create(&self, dir: &Path) -> Result<PathBuf> {
        let f = self.spec();
        let image = dir.join("work.img");
        // A sparse, zero-filled file (does not eat disk space).
        File::create(&image)?.set_len(f.size)?;
        if f.partition_start.is_some() {
            write_mbr(&image, f)?;
        }
        self.format(&image)?;
        Ok(image)
    }

    fn spec(&self) -> &Format;

    /// Quick-formats the volume (leaving existing data areas intact).
    fn format(&self, image: &Path) -> Result<()>;
    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>>;
}

/// A mounted volume.
pub trait Mounted {
    fn root(&self) -> &Path;
    /// Flushes writes to disk and detaches.
    fn unmount(self: Box<Self>) -> Result<()>;
}

/// The implementation for this OS.
pub fn native(format: Format) -> Result<Box<dyn Backend>> {
    if format.fs == FsKind::None {
        bail!("no OS driver for disks without a file system");
    }
    if format
        .partition_start
        .is_some_and(|s| s == 0 || s * 512 >= format.size)
    {
        bail!("partition_start must be inside the disk");
    }
    #[cfg(target_os = "macos")]
    return Ok(Box::new(macos::Hdiutil(format)));
    #[cfg(target_os = "linux")]
    return Ok(Box::new(linux::Loop(format)));
    #[cfg(windows)]
    return Ok(Box::new(windows::Vhd(format)));
    #[allow(unreachable_code)]
    {
        bail!("unsupported OS")
    }
}

/// Writes an MBR with one partition covering the rest of the disk.
/// Reference: the classic MBR layout (entries at 446, signature 0x55AA at 510).
fn write_mbr(image: &Path, f: &Format) -> Result<()> {
    let (start, len) = f.volume();
    let mut mbr = [0u8; 512];
    let entry = [
        0x00,
        0xFE,
        0xFF,
        0xFF, // not bootable; CHS start unused (LBA only)
        f.partition_type(),
        0xFE,
        0xFF,
        0xFF, // CHS end unused
    ];
    mbr[446..454].copy_from_slice(&entry);
    mbr[454..458].copy_from_slice(&u32::try_from(start / 512)?.to_le_bytes());
    mbr[458..462].copy_from_slice(&u32::try_from(len / 512)?.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    let mut file = OpenOptions::new().write(true).open(image)?;
    file.write_all(&mbr)?;
    file.sync_all()?;
    Ok(())
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
            "{cmd:?} failed ({}): {}{}",
            out.status,
            String::from_utf8_lossy(&out.stdout).trim(),
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
