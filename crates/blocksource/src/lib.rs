//! Read layer abstraction.
//!
//! Every read from a target disk goes through [`BlockSource`]. This crate offers
//! no way to write, so writing to the source disk is impossible at the type level.
//!
//! - [`ImageFile`]: a disk image (regular file)
//! - [`Device`]: a raw physical disk or volume device
//! - [`MemSource`]: an in-memory buffer (for tests)
//! - [`Aligned`]: turns reads at any offset/length into sector-aligned reads
//! - [`sim`]: simulation of physical conditions (bad sectors, latency, seek cost)

mod aligned;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod device;
mod error;
mod image;
mod mem;
mod pread;
pub mod sim;

use std::sync::Arc;

pub use aligned::Aligned;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub use device::Device;
pub use error::{ReadError, ReadErrorKind};
pub use image::ImageFile;
pub use mem::MemSource;

/// A read-only block device.
///
/// # Contract
///
/// - `read_at` reads up to `buf.len()` bytes starting at `offset` and returns the number
///   of bytes read. It returns fewer than `buf.len()` only at the end of the source
///   (`Ok(0)` if `offset >= len()`).
/// - On failure the contents of `buf` are unspecified. [`ReadError::range`] holds the
///   range that could not be read; it may be widened to sector boundaries and is not
///   guaranteed to lie within the requested range.
/// - Reads take `&self`, so they may be issued from multiple threads concurrently.
pub trait BlockSource: Send + Sync {
    /// Total size in bytes.
    fn len(&self) -> u64;

    /// Logical sector size in bytes. Always a power of two.
    fn sector_size(&self) -> u32;

    /// Reads at `offset`. See the trait documentation for details.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads until `buf` is full. Fails with [`ReadErrorKind::UnexpectedEof`] if the end
    /// of the source is reached first.
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

/// The number of bytes readable at `offset`, capped at `buf_len`.
pub(crate) fn readable_len(source_len: u64, offset: u64, buf_len: usize) -> usize {
    clamp_usize(source_len.saturating_sub(offset)).min(buf_len)
}

/// Converts `u64` to `usize`, saturating at `usize::MAX`.
/// Only use where the result is then `min`-ed with a buffer length or similar.
pub(crate) fn clamp_usize(x: u64) -> usize {
    usize::try_from(x).unwrap_or(usize::MAX)
}

pub(crate) fn assert_sector_size(sector_size: u32) {
    assert!(
        sector_size.is_power_of_two(),
        "sector size must be a power of two: {sector_size}"
    );
}
