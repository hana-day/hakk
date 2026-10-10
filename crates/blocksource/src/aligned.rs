use crate::{BlockSource, ReadError, clamp_usize, readable_len};

/// 任意の位置・長さの読み込みを、セクタ境界に揃えた読み込みに変換する。
///
/// 生デバイスは位置と長さがセクタの倍数でない読み込みを受け付けないため、
/// その上に被せて使う。すでに揃っている読み込みはそのまま通す。
#[derive(Debug)]
pub struct Aligned<S> {
    inner: S,
}

impl<S: BlockSource> Aligned<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: BlockSource> BlockSource for Aligned<S> {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        let want = readable_len(self.len(), offset, buf.len());
        let Some(dst) = buf.get_mut(..want).filter(|d| !d.is_empty()) else {
            return Ok(0);
        };
        let ss = u64::from(self.sector_size());
        let end = offset + want as u64;

        if offset.is_multiple_of(ss) && end.is_multiple_of(ss) {
            return self.inner.read_at(offset, dst);
        }

        // 前後をセクタ境界まで広げて読み、必要な部分だけ写す
        let start = offset / ss * ss;
        let aligned_end = end.div_ceil(ss) * ss;
        let mut bounce = vec![0u8; clamp_usize(aligned_end - start)];
        let got = self.inner.read_at(start, &mut bounce)?;
        let head = clamp_usize(offset - start);
        let n = got.saturating_sub(head).min(want);
        if let (Some(dst), Some(src)) = (dst.get_mut(..n), bounce.get(head..head + n)) {
            dst.copy_from_slice(src);
        }
        Ok(n)
    }
}
