//! 読み込み層の抽象化。
//!
//! 対象ディスクの読み込みはすべてこのクレートの [`BlockSource`] を経由する。
//! 書き込みの手段は一切提供しない（元ディスクへの書き込みを型の上で不可能にする）。
//!
//! - [`ImageFile`]：ディスクイメージ（ファイル）
//! - [`MemSource`]：メモリ上のバッファ（テスト用）
//! - [`Aligned`]：任意の位置・長さの読み込みを、セクタ境界に揃えた読み込みに変換する
//! - [`sim`]：物理条件のシミュレーション（不良セクタ・遅延・シークコスト）

mod aligned;
mod error;
mod image;
mod mem;
pub mod sim;

use std::sync::Arc;

pub use aligned::Aligned;
pub use error::{ReadError, ReadErrorKind};
pub use image::ImageFile;
pub use mem::MemSource;

/// 読み取り専用のブロックデバイス。
///
/// # 契約
///
/// - `read_at` は `offset` から最大 `buf.len()` バイトを読み、読めたバイト数を返す。
///   `buf.len()` より少なくなるのは終端に達した場合だけ（`offset >= len()` なら `Ok(0)`）。
/// - 失敗した場合 `buf` の内容は不定。[`ReadError::range`] に読めなかった範囲を入れる。
///   この範囲はセクタ境界まで広がることがあり、要求範囲に収まるとは限らない。
/// - `&self` で読むので、複数スレッドから同時に呼べる。
pub trait BlockSource: Send + Sync {
    /// 全体の大きさ（バイト）。
    fn len(&self) -> u64;

    /// 論理セクタの大きさ（バイト）。2のべき乗。
    fn sector_size(&self) -> u32;

    /// `offset` から読む。詳細はトレイトの説明を参照。
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `buf` を満たすまで読む。終端に達して満たせなければ [`ReadErrorKind::UnexpectedEof`]。
    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        let n = self.read_at(offset, buf)?;
        if n == buf.len() {
            Ok(())
        } else {
            let start = offset.saturating_add(n as u64);
            let end = offset.saturating_add(buf.len() as u64);
            Err(ReadError::new(start..end, ReadErrorKind::UnexpectedEof))
        }
    }
}

macro_rules! forward_impl {
    ($($ty:ty),*) => {$(
        impl<T: BlockSource + ?Sized> BlockSource for $ty {
            fn len(&self) -> u64 {
                (**self).len()
            }
            fn sector_size(&self) -> u32 {
                (**self).sector_size()
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
                (**self).read_at(offset, buf)
            }
        }
    )*};
}
forward_impl!(&T, Box<T>, Arc<T>);

/// `offset` から読める長さを `buf_len` で頭打ちにしたもの。
pub(crate) fn readable_len(source_len: u64, offset: u64, buf_len: usize) -> usize {
    clamp_usize(source_len.saturating_sub(offset)).min(buf_len)
}

/// `u64` を `usize` に変換する。収まらなければ `usize::MAX`。
/// 結果を必ずバッファ長などと `min` を取る場面でだけ使う。
pub(crate) fn clamp_usize(x: u64) -> usize {
    usize::try_from(x).unwrap_or(usize::MAX)
}

pub(crate) fn assert_sector_size(sector_size: u32) {
    assert!(
        sector_size.is_power_of_two(),
        "セクタサイズは2のべき乗でなければならない: {sector_size}"
    );
}
