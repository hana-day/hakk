//! Windows: creates a fixed-size VHD with `diskpart` and attaches it with `Mount-DiskImage`.
//! Formats with `Format-Volume` (exFAT, FAT32, FAT, NTFS).
//!
//! Windows cannot create volumes on a disk without a partition table, so one MBR
//! partition is created. A fixed VHD is "disk contents + a 512-byte footer", so its first
//! `size` bytes are the disk image as is.
//! Requires admin rights (GitHub Actions Windows runners run as admin).

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use super::{Backend, Format, Mounted, run};
use crate::scenario::FsKind;

pub struct Vhd(pub Format);

impl Vhd {
    /// A `Format-Volume` command for `target` (e.g. `-DriveLetter E`). Windows picks FAT12
    /// or FAT16 itself for "FAT" and rejects cluster sizes that do not fit its choice, so a
    /// requested cluster size falls back to the default (the manifest records what was
    /// actually made).
    fn format_cmd(&self, target: &str) -> Result<String> {
        let f = &self.0;
        let fs = match f.fs {
            FsKind::Exfat => "exFAT",
            FsKind::Fat32 => "FAT32",
            FsKind::Fat12 | FsKind::Fat16 => "FAT",
            FsKind::Ntfs => "NTFS",
            FsKind::None => bail!("no OS driver for disks without a file system"),
        };
        let base = format!(
            "Format-Volume {target} -FileSystem {fs} -NewFileSystemLabel {} -Force -Confirm:$false",
            quote(Path::new(&f.label))
        );
        Ok(match f.cluster {
            Some(c) => format!(
                "try {{ {base} -AllocationUnitSize {c} | Out-Null }} catch {{ {base} | Out-Null }}"
            ),
            None => format!("{base} | Out-Null"),
        })
    }
}

impl Backend for Vhd {
    fn spec(&self) -> &Format {
        &self.0
    }

    fn create(&self, dir: &Path) -> Result<PathBuf> {
        let size = self.0.size;
        if !size.is_multiple_of(1 << 20) {
            bail!("on Windows the size must be a whole number of MiB");
        }
        let image = fs::canonicalize(dir)?.join("work.vhd");
        if image.exists() {
            fs::remove_file(&image)?;
        }
        let script = dir.join("create.diskpart");
        fs::write(
            &script,
            format!(
                "create vdisk file=\"{}\" maximum={} type=fixed\r\n",
                image.display(),
                size >> 20
            ),
        )?;
        let result = run(Command::new("diskpart").arg("/s").arg(&script));
        let _ = fs::remove_file(&script);
        result?;

        if self.0.partition_start.is_some() {
            // New-Partition only accepts aligned offsets, so write the MBR ourselves (a
            // fixed VHD starts with the raw disk) and format the existing partition.
            super::write_mbr(&image, &self.0)?;
            // The partition is unformatted, so formatting it is the same as a quick format.
            self.format(&image)?;
        } else {
            powershell(&format!(
                "$d = Mount-DiskImage -ImagePath {img} -PassThru | Get-Disk
                 Initialize-Disk -Number $d.Number -PartitionStyle MBR
                 $p = New-Partition -DiskNumber $d.Number -UseMaximumSize -AssignDriveLetter
                 {cmd}
                 Dismount-DiskImage -ImagePath {img} | Out-Null",
                img = quote(&image),
                cmd = self.format_cmd("-Partition $p")?,
            ))?;
        }
        Ok(image)
    }

    fn format(&self, image: &Path) -> Result<()> {
        let m = self.mount(image)?;
        // Quick format by default (data areas are kept unless -Full is given).
        let letter = m.root().to_string_lossy().chars().next().unwrap_or('Z');
        let result = self
            .format_cmd(&format!("-DriveLetter {letter}"))
            .and_then(|cmd| powershell(&cmd));
        m.unmount()?;
        result.map(drop)
    }

    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>> {
        // A drive letter is not always assigned automatically; assign one if missing.
        let out = powershell(&format!(
            "$d = Mount-DiskImage -ImagePath {img} -PassThru | Get-Disk
             $p = Get-Partition -DiskNumber $d.Number | Where-Object Type -ne 'Reserved' | Select-Object -First 1
             if (-not $p.DriveLetter) {{
               $p | Add-PartitionAccessPath -AssignDriveLetter
               $p = Get-Partition -DiskNumber $d.Number -PartitionNumber $p.PartitionNumber
             }}
             Write-Output $p.DriveLetter",
            img = quote(image),
        ))?;
        let letter = out
            .trim()
            .chars()
            .next()
            .filter(char::is_ascii_alphabetic)
            .with_context(|| format!("no drive letter found: {out:?}"))?;
        Ok(Box::new(Volume {
            image: image.to_path_buf(),
            root: PathBuf::from(format!("{letter}:\\")),
        }))
    }
}

struct Volume {
    image: PathBuf,
    root: PathBuf,
}

impl Mounted for Volume {
    fn root(&self) -> &Path {
        &self.root
    }

    fn unmount(self: Box<Self>) -> Result<()> {
        powershell(&format!(
            "Dismount-DiskImage -ImagePath {} | Out-Null",
            quote(&self.image)
        ))
        .map(drop)
    }
}

fn powershell(script: &str) -> Result<String> {
    run(Command::new("powershell").args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &format!(
            "$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'\n{script}"
        ),
    ]))
}

/// Quotes as a PowerShell single-quoted string.
fn quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "''"))
}
