use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Result, bail};

use super::{Backend, Format, Mounted, run, temp_mount_dir};
use crate::scenario::FsKind;

/// `mkfs.exfat` / `mkfs.fat` / `mkfs.ntfs` and loop devices (`sudo losetup`, `sudo mount`,
/// `sudo ntfs-3g`). For CI.
pub struct Loop(pub Format);

impl Backend for Loop {
    fn spec(&self) -> &Format {
        &self.0
    }

    fn format(&self, image: &Path) -> Result<()> {
        let f = &self.0;
        // A loop device over just the volume, so the formatter sees only the partition.
        let dev = attach(image, f)?;
        let mut cmd = Command::new("sudo");
        match f.fs {
            FsKind::Exfat => {
                cmd.args(["mkfs.exfat", "-L", &f.label]);
                if let Some(c) = f.cluster {
                    cmd.args(["-c", &c.to_string()]);
                }
            }
            FsKind::Fat12 | FsKind::Fat16 | FsKind::Fat32 => {
                let bits = match f.fs {
                    FsKind::Fat12 => "12",
                    FsKind::Fat16 => "16",
                    _ => "32",
                };
                cmd.args(["mkfs.fat", "-F", bits, "-n", &f.label]);
                if let Some(spc) = f.sectors_per_cluster() {
                    cmd.args(["-s", &spc.to_string()]);
                }
                if let Some(start) = f.partition_start {
                    // Hidden sectors: where the partition starts, as cameras record it.
                    cmd.args(["-h", &start.to_string()]);
                }
            }
            FsKind::Ntfs => {
                // Quick format, and no questions about the loop device's geometry.
                cmd.args(["mkfs.ntfs", "--quick", "--force", "-L", &f.label]);
                if let Some(c) = f.cluster {
                    cmd.args(["-c", &c.to_string()]);
                }
                if let Some(start) = f.partition_start {
                    cmd.args(["--partition-start", &start.to_string()]);
                }
            }
            FsKind::None => bail!("no OS driver for disks without a file system"),
        }
        let result = run(cmd.arg(&dev));
        detach(&dev)?;
        result.map(drop)
    }

    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>> {
        let f = &self.0;
        let dev = attach(image, f)?;
        let root = temp_mount_dir()?;
        let uid = run(Command::new("id").arg("-u"))?;
        let gid = run(Command::new("id").arg("-g"))?;
        let opts = format!("uid={},gid={}", uid.trim(), gid.trim());
        let mut cmd = Command::new("sudo");
        match f.fs {
            FsKind::Exfat => cmd.args(["mount", "-t", "exfat", "-o", &opts]),
            FsKind::Ntfs => cmd.args(["ntfs-3g", "-o", &opts]),
            _ => cmd.args(["mount", "-t", "vfat", "-o", &opts]),
        };
        if let Err(e) = run(cmd.arg(&dev).arg(&root)) {
            detach(&dev)?;
            return Err(e);
        }
        Ok(Box::new(Volume {
            root,
            dev,
            fuse: f.fs == FsKind::Ntfs,
        }))
    }
}

/// Attaches the volume part of `image` as a loop device and returns its path.
fn attach(image: &Path, f: &Format) -> Result<String> {
    let (offset, len) = f.volume();
    let out = run(Command::new("sudo")
        .args(["losetup", "--find", "--show"])
        .args([
            "--offset",
            &offset.to_string(),
            "--sizelimit",
            &len.to_string(),
        ])
        .arg(image))?;
    Ok(out.trim().to_owned())
}

fn detach(dev: &str) -> Result<()> {
    run(Command::new("sudo").args(["losetup", "-d", dev])).map(drop)
}

struct Volume {
    root: PathBuf,
    dev: String,
    /// Mounted by a FUSE daemon (`ntfs-3g`), which may still be writing after `umount`.
    fuse: bool,
}

impl Mounted for Volume {
    fn root(&self) -> &Path {
        &self.root
    }

    fn unmount(self: Box<Self>) -> Result<()> {
        run(&mut Command::new("sync"))?;
        run(Command::new("sudo").arg("umount").arg(&self.root))?;
        if self.fuse {
            wait_for_daemon(&self.dev)?;
        }
        let _ = std::fs::remove_dir(&self.root);
        detach(&self.dev)
    }
}

/// Waits until the FUSE daemon serving `dev` has exited, so its last writes are on disk
/// before the image is read.
fn wait_for_daemon(dev: &str) -> Result<()> {
    let pattern = format!("ntfs-3g .*{dev} ");
    for _ in 0..600 {
        let running = Command::new("pgrep")
            .args(["-f", &pattern])
            .output()
            .is_ok_and(|o| o.status.success());
        if !running {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("ntfs-3g for {dev} did not exit within 60 s of unmounting")
}
