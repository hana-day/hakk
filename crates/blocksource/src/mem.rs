use crate::{BlockSource, ReadError, assert_sector_size};

/// メモリ上のバッファ。主にテスト用。
#[derive(Debug, Clone)]
pub struct MemSource {
    data: Vec<u8>,
    sector_size: u32,
}

impl MemSource {
    /// セクタサイズ 512 で作る。
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            data: data.into(),
            sector_size: 512,
        }
    }

    /// # Panics
    /// `sector_size` が2のべき乗でない場合。
    pub fn with_sector_size(mut self, sector_size: u32) -> Self {
        assert_sector_size(sector_size);
        self.sector_size = sector_size;
        self
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

impl BlockSource for MemSource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        let src = usize::try_from(offset)
            .ok()
            .and_then(|start| self.data.get(start..))
            .unwrap_or_default();
        let n = src.len().min(buf.len());
        if let (Some(dst), Some(src)) = (buf.get_mut(..n), src.get(..n)) {
            dst.copy_from_slice(src);
        }
        Ok(n)
    }
}
