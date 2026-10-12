use std::{
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{Backend, Format, Mounted, run};
use crate::scenario::FsKind;

/// Attaches images as disks with `hdiutil`.
pub struct Hdiutil(pub Format);

/// Attach as a raw image without a partition table.
const RAW: [&str; 2] = ["-imagekey", "diskimage-class=CRawDiskImage"];

impl Backend for Hdiutil {
    fn spec(&self) -> &Format {
        &self.0
    }

    fn format(&self, image: &Path) -> Result<()> {
        let f = &self.0;
        let dev = attach_nomount(image)?;
        // With a partition table, hdiutil exposes the partition as the first slice.
        let target = if f.partition_start.is_some() {
            format!("{dev}s1")
        } else {
            dev.clone()
        };
        let mut cmd = match f.fs {
            FsKind::Exfat => Command::new("newfs_exfat"),
            FsKind::Fat12 | FsKind::Fat16 | FsKind::Fat32 => {
                let mut c = Command::new("newfs_msdos");
                c.args(["-F", fat_bits(f.fs)]);
                c
            }
            FsKind::Ntfs => {
                bail!("macOS cannot write NTFS; build NTFS scenarios on Linux or Windows")
            }
            FsKind::None => bail!("no OS driver for disks without a file system"),
        };
        cmd.args(["-v", &f.label]);
        if let Some(spc) = f.sectors_per_cluster() {
            cmd.args(["-c", &spc.to_string()]);
        }
        let result = run(cmd.arg(&target));
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

fn fat_bits(fs: FsKind) -> &'static str {
    match fs {
        FsKind::Fat12 => "12",
        FsKind::Fat16 => "16",
        _ => "32",
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
