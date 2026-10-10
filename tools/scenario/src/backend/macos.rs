use std::{
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{Backend, Mounted, run};
use crate::scenario::FsKind;

/// Attaches images as disks with `hdiutil`.
pub struct Hdiutil;

/// Attach as a raw image without a partition table.
const RAW: [&str; 2] = ["-imagekey", "diskimage-class=CRawDiskImage"];

impl Backend for Hdiutil {
    fn format(&self, image: &Path, fs: FsKind, label: &str) -> Result<()> {
        let FsKind::Exfat = fs else {
            anyhow::bail!("{fs:?} has no OS driver");
        };
        let dev = attach_nomount(image)?;
        let result = run(Command::new("newfs_exfat").args(["-v", label]).arg(&dev));
        detach(&dev)?;
        result.map(drop)
    }

    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>> {
        // Custom mount points end up unwritable (fskit ownership handling), so use the default /Volumes.
        let out = run(Command::new("hdiutil")
            .arg("attach")
            .args(RAW)
            .args(["-nobrowse", "-noverify", "-noautofsck"])
            .arg(image))?;
        let mut dev = None;
        let mut root = None;
        for line in out.lines() {
            let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
            if let Some(d) = cols.first().filter(|d| d.starts_with("/dev/disk")) {
                dev.get_or_insert_with(|| (*d).to_owned());
            }
            if let Some(m) = cols.get(2).filter(|m| m.starts_with('/')) {
                root = Some(PathBuf::from(m));
            }
        }
        let dev = dev.context("no device in hdiutil attach output")?;
        let Some(root) = root else {
            detach(&dev)?;
            bail!("volume was not mounted: {out}");
        };
        Ok(Box::new(Volume { dev, root }))
    }
}

struct Volume {
    dev: String,
    root: PathBuf,
}

impl Mounted for Volume {
    fn root(&self) -> &Path {
        &self.root
    }

    fn unmount(self: Box<Self>) -> Result<()> {
        detach(&self.dev)
    }
}

fn attach_nomount(image: &Path) -> Result<String> {
    let out = run(Command::new("hdiutil")
        .arg("attach")
        .args(RAW)
        .args(["-nomount", "-noverify"])
        .arg(image))?;
    out.split_whitespace()
        .next()
        .filter(|d| d.starts_with("/dev/disk"))
        .map(str::to_owned)
        .context("no device in hdiutil attach output")
}

/// Detaches. Right after use it may still be busy, so retry after a short wait.
fn detach(dev: &str) -> Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match run(Command::new("hdiutil").args(["detach", dev])) {
            Ok(_) => return Ok(()),
            Err(e) => last = Some(e),
        }
        thread::sleep(Duration::from_millis(500 * (attempt + 1)));
    }
    // Last resort. Every file was already fsynced.
    run(Command::new("hdiutil").args(["detach", "-force", dev]))
        .map(drop)
        .with_context(|| format!("cannot detach {dev}: {last:?}"))
}
