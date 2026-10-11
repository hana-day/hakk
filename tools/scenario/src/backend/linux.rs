use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Result, bail};

use super::{Backend, Format, Mounted, run, temp_mount_dir};
use crate::scenario::FsKind;

/// `mkfs.exfat` / `mkfs.fat` and loop devices (`sudo losetup`, `sudo mount`). For CI.
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
        let ty = if f.fs == FsKind::Exfat {
            "exfat"
        } else {
            "vfat"
        };
        let opts = format!("uid={},gid={}", uid.trim(), gid.trim());
        if let Err(e) = run(Command::new("sudo")
            .args(["mount", "-t", ty, "-o", &opts])
            .arg(&dev)
            .arg(&root))
        {
            detach(&dev)?;
            return Err(e);
        }
        Ok(Box::new(Volume { root, dev }))
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
}

impl Mounted for Volume {
    fn root(&self) -> &Path {
        &self.root
    }

    fn unmount(self: Box<Self>) -> Result<()> {
        run(&mut Command::new("sync"))?;
        run(Command::new("sudo").arg("umount").arg(&self.root))?;
        let _ = std::fs::remove_dir(&self.root);
        detach(&self.dev)
    }
}
