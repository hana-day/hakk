//! 本物の OS ドライバでファイルシステムを操作する。
//!
//! - macOS：`hdiutil` ＋ `newfs_exfat`（管理者権限不要）
//! - Linux：`mkfs.exfat` ＋ ループバックマウント（`sudo` が必要。CI 用）
//! - Windows：未対応（毎晩の CI で VHD を使う予定）

// 未対応の OS では実装がなく、共通の補助関数が使われない
#![cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::{path::Path, process::Command};

use anyhow::{Context, Result, bail};

use crate::scenario::FsKind;

/// イメージファイルにファイルシステムを作り、マウントする手段。
pub trait Backend {
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
    #[allow(unreachable_code)]
    {
        bail!("この OS にはまだ対応していない（macOS と Linux のみ）")
    }
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
