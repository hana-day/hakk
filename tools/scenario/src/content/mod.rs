//! テスト用ファイルの中身を、シードから決定的に作る。
//!
//! どのファイルも 512 バイトごとの中身がほぼ一意になるようにする（配置を中身の照合で
//! 特定するため）。そのために圧縮データ・サンプルデータは乱数で埋める。

mod exif;
mod jpeg;
mod mp4;

use serde::Serialize;

use crate::{rng::Rng, scenario::ContentKind};

pub use exif::Exif;

/// 生成したファイル。
pub struct Generated {
    pub bytes: Vec<u8>,
    pub meta: Meta,
}

/// 正解マニフェストに残すメタデータ（ファイル名の復元などの評価に使う）。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exif: Option<Exif>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// 動画のサンプル数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
}

/// おおよそ `target` バイトのファイルを作る（大きさは目安。種類により前後する）。
pub fn generate(kind: ContentKind, target: u64, rng: &mut Rng) -> anyhow::Result<Generated> {
    match kind {
        ContentKind::Jpeg => jpeg::generate(target, rng),
        ContentKind::Mp4 => Ok(mp4::generate(target, rng)),
    }
}

pub fn extension(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Jpeg => "JPG",
        ContentKind::Mp4 => "MP4",
    }
}
