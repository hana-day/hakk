use std::{fs::File, io, path::Path};

use crate::{BlockSource, ReadError, assert_sector_size, pread::read_full, readable_len};

/// ディスクイメージ（通常のファイル）。読み取り専用で開く。
///
/// 物理ディスク（`/dev/rdiskN`、`\\.\PhysicalDriveN`）は大きさの取得方法や
/// 境界の制約が異なるため、別の実装で扱う。
#[derive(Debug)]
pub struct ImageFile {
    file: File,
    len: u64,
    sector_size: u32,
}

impl ImageFile {
    /// 読み取り専用で開く。大きさは開いた時点のものに固定する。セクタサイズは 512。
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            file,
            len,
            sector_size: 512,
        })
    }

    /// # Panics
    /// `sector_size` が2のべき乗でない場合。
    pub fn with_sector_size(mut self, sector_size: u32) -> Self {
        assert_sector_size(sector_size);
        self.sector_size = sector_size;
        self
    }
}

impl BlockSource for ImageFile {
    fn len(&self) -> u64 {
        self.len
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        let want = readable_len(self.len, offset, buf.len());
        match buf.get_mut(..want) {
            Some(dst) => read_full(&self.file, offset, dst),
            None => Ok(0),
        }
    }
}
