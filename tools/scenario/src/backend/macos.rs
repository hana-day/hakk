use std::{
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};

use super::{Backend, Mounted, run};
use crate::scenario::FsKind;

/// `hdiutil` でイメージをディスクとして接続する。
pub struct Hdiutil;

/// 区画表のない生イメージとして接続するための指定。
const RAW: [&str; 2] = ["-imagekey", "diskimage-class=CRawDiskImage"];

impl Backend for Hdiutil {
    fn format(&self, image: &Path, fs: FsKind, label: &str) -> Result<()> {
        let FsKind::Exfat = fs;
        let dev = attach_nomount(image)?;
        let result = run(Command::new("newfs_exfat").args(["-v", label]).arg(&dev));
        detach(&dev)?;
        result.map(drop)
    }

    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>> {
        // マウント先を自分で指定すると書き込めない（fskit の所有者の扱い）ので、既定の /Volumes を使う
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
        let dev = dev.context("hdiutil attach の出力にデバイスがない")?;
        let Some(root) = root else {
            detach(&dev)?;
            bail!("ボリュームがマウントされなかった: {out}");
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
        .context("hdiutil attach の出力にデバイスがない")
}

/// 切り離す。直後は使用中で失敗することがあるので、少し待って再試行する。
fn detach(dev: &str) -> Result<()> {
    let mut last = None;
    for attempt in 0..5 {
        match run(Command::new("hdiutil").args(["detach", dev])) {
            Ok(_) => return Ok(()),
            Err(e) => last = Some(e),
        }
        thread::sleep(Duration::from_millis(500 * (attempt + 1)));
    }
    // 最後の手段。書き込みは各ファイルで fsync 済み
    run(Command::new("hdiutil").args(["detach", "-force", dev]))
        .map(drop)
        .with_context(|| format!("{dev} を切り離せない: {last:?}"))
}
