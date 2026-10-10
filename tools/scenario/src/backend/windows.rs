//! Windows：固定サイズの VHD を `diskpart` で作り、`Mount-DiskImage` で接続する。
//!
//! Windows は区画表の無いディスクにボリュームを作れないので、MBR の区画を1つ作る。
//! 固定サイズの VHD は「ディスクの中身 ＋ 末尾 512 バイトのフッタ」なので、
//! 先頭からディスクの大きさ分がそのままディスクイメージになる。
//! 管理者権限が必要（GitHub Actions の Windows ランナーは管理者で動く）。

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use super::{Backend, Mounted, run};
use crate::scenario::FsKind;

pub struct Vhd;

impl Backend for Vhd {
    fn create(&self, dir: &Path, size: u64, fs: FsKind, label: &str) -> Result<PathBuf> {
        if !size.is_multiple_of(1 << 20) {
            bail!("Windows では大きさを MiB 単位にする");
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

        let FsKind::Exfat = fs;
        powershell(&format!(
            "$d = Mount-DiskImage -ImagePath {img} -PassThru | Get-Disk
             Initialize-Disk -Number $d.Number -PartitionStyle MBR
             $p = New-Partition -DiskNumber $d.Number -UseMaximumSize -AssignDriveLetter
             Format-Volume -Partition $p -FileSystem exFAT -NewFileSystemLabel {label} -Force -Confirm:$false | Out-Null
             Dismount-DiskImage -ImagePath {img} | Out-Null",
            img = quote(&image),
            label = quote(Path::new(label)),
        ))?;
        Ok(image)
    }

    fn format(&self, image: &Path, fs: FsKind, label: &str) -> Result<()> {
        let FsKind::Exfat = fs;
        let m = self.mount(image)?;
        // 既定でクイックフォーマット（-Full を付けない限りデータ領域は消さない）
        let result = powershell(&format!(
            "Format-Volume -DriveLetter {letter} -FileSystem exFAT -NewFileSystemLabel {label} -Force -Confirm:$false | Out-Null",
            letter = m.root().to_string_lossy().chars().next().unwrap_or('Z'),
            label = quote(Path::new(label)),
        ));
        m.unmount()?;
        result.map(drop)
    }

    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>> {
        // ドライブ文字は自動では付かないことがあるので、無ければ付ける
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
            .with_context(|| format!("ドライブ文字が分からない: {out:?}"))?;
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

/// PowerShell の単一引用符の文字列にする。
fn quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "''"))
}
