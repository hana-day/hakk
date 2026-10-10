//! 本物の OS ドライバでファイルシステムを操作する。
//!
//! - macOS：`hdiutil` ＋ `newfs_exfat`（管理者権限不要）
//! - Linux：`mkfs.exfat` ＋ ループバックマウント（`sudo` が必要。CI 用）
//! - Windows：固定サイズの VHD ＋ `Mount-DiskImage`（管理者権限が必要。MBR の区画を1つ作る）

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

/// イメージファイルにファイルシステムを作り、マウントする手段。
pub trait Backend {
    /// `dir` に大きさ `size` の空のディスクを作り、ファイルシステムを作る。
    /// 以後の操作に使う作業用ファイルを返す。その先頭 `size` バイトがディスクの中身になる
    /// （VHD のように末尾に管理情報が付く形式もあるため）。
    fn create(&self, dir: &Path, size: u64, fs: FsKind, label: &str) -> Result<PathBuf> {
        let image = dir.join("work.img");
        // 中身ゼロの疎なファイル（ディスクを使い切らない）
        File::create(&image)?.set_len(size)?;
        self.format(&image, fs, label)?;
        Ok(image)
    }

    /// クイックフォーマットする（既存のデータ領域は消さない）。
    fn format(&self, image: &Path, fs: FsKind, label: &str) -> Result<()>;
    fn mount(&self, image: &Path) -> Result<Box<dyn Mounted>>;
}

/// マウント中のボリューム。
pub trait Mounted {
    fn root(&self) -> &Path;
    /// 書き込みをディスクに反映して切り離す。
    fn unmount(self: Box<Self>) -> Result<()>;
}

/// この OS の実装。
pub fn native() -> Result<Box<dyn Backend>> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(macos::Hdiutil));
    #[cfg(target_os = "linux")]
    return Ok(Box::new(linux::Loop));
    #[cfg(windows)]
    return Ok(Box::new(windows::Vhd));
    #[allow(unreachable_code)]
    {
        bail!("この OS には対応していない")
    }
}

/// 作業用ファイルからディスクの中身（先頭 `size` バイト）を読む。
pub fn read_disk(image: &Path, size: u64) -> Result<Vec<u8>> {
    let mut data = fs::read(image).with_context(|| format!("{} を読めない", image.display()))?;
    data.truncate(usize::try_from(size)?);
    Ok(data)
}

/// コマンドを実行して標準出力を返す。失敗したら標準エラーを含めて報告する。
pub(crate) fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("{cmd:?} を起動できない"))?;
    if !out.status.success() {
        bail!(
            "{cmd:?} が失敗した（{}）: {}",
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
