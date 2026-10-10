use std::{fs::File, io, path::Path};

use crate::{BlockSource, ReadError, assert_sector_size, pread::read_full, readable_len};

/// A disk image (regular file), opened read-only.
///
/// Physical disks (`/dev/rdiskN`, `\\.\PhysicalDriveN`) differ in how their size is
/// obtained and in alignment constraints, so they are handled by [`crate::Device`].
#[derive(Debug)]
pub struct ImageFile {
    file: File,
    len: u64,
    sector_size: u32,
}

impl ImageFile {
    /// Opens the file read-only. The size is fixed at open time. The sector size is 512.
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
    /// If `sector_size` is not a power of two.
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
