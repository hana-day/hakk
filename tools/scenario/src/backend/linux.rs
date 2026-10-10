use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;

use super::{Backend, Mounted, run, temp_mount_dir};
use crate::scenario::FsKind;

/// `mkfs.exfat` and a loop mount (`sudo mount -o loop`). For CI.
pub struct Loop;

impl Backend for Loop {
    fn format(&self, image: &Path, fs: FsKind, label: &str) -> Result<()> {
        let FsKind::Exfat = fs;
        run(Command::new("mkfs.exfat").args(["-L", label]).arg(image)).map(drop)
    }

    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>> {
        let root = temp_mount_dir()?;
        let uid = run(Command::new("id").arg("-u"))?;
        let gid = run(Command::new("id").arg("-g"))?;
        let opts = format!("loop,uid={},gid={}", uid.trim(), gid.trim());
        run(Command::new("sudo")
            .args(["mount", "-t", "exfat", "-o", &opts])
            .arg(image)
            .arg(&root))?;
        Ok(Box::new(Volume { root }))
    }
}

struct Volume {
    root: PathBuf,
}

impl Mounted for Volume {
    fn root(&self) -> &Path {
        &self.root
    }

    fn unmount(self: Box<Self>) -> Result<()> {
        run(&mut Command::new("sync"))?;
        run(Command::new("sudo").arg("umount").arg(&self.root))?;
        let _ = std::fs::remove_dir(&self.root);
        Ok(())
    }
}
