//! カービング：ファイルシステムを使わず、生データからファイルを探して切り出す。
//!
//! 今は比較の基準にする最小限の実装（[`naive`]）だけがある。フェーズ1で本実装に置き換える。

pub mod naive;

/// 見つかったファイルの候補。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub offset: u64,
    pub len: u64,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
}

impl Kind {
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Jpeg => "jpg",
        }
    }
}
