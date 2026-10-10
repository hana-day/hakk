//! 正解マニフェスト。評価ツールはこれと復元結果を照合する。

use serde::Serialize;

use crate::{
    content::Meta,
    locate::Extent,
    scenario::{ContentKind, FsKind},
};

pub const VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Manifest {
    pub version: u32,
    pub scenario: String,
    pub seed: u64,
    pub fs: FsKind,
    /// マニフェストからの相対パス。
    pub image: String,
    pub image_size: u64,
    /// ファイルシステムの開始位置（区画表があれば最初の区画の位置）。
    pub partition_offset: u64,
    /// ファイルシステムのクラスタサイズ（ブートセクタから読んだもの）。
    pub cluster_size: Option<u64>,
    /// イメージを作った環境（OS ドライバによって配置が変わるため記録する）。
    pub generator: Generator,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Serialize)]
pub struct Generator {
    pub os: String,
    pub os_version: String,
    pub tool_version: String,
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub id: u32,
    /// ボリューム内のパス（`/` 区切り）。
    pub path: String,
    pub kind: ContentKind,
    pub size: u64,
    pub sha256: String,
    /// 書き込んだステップ番号。
    pub written_at: usize,
    /// 削除されたステップ番号（フォーマットも含む）。残っていれば `None`。
    pub deleted_at: Option<usize>,
    /// 書き込んだ直後の配置。特定できなかった場合は `None`。
    pub extents: Option<Vec<Extent>>,
    pub fragmented: bool,
    /// 最終イメージ上で元の中身のまま残っているバイト数。
    pub intact_bytes: u64,
    #[serde(flatten)]
    pub meta: Meta,
}
