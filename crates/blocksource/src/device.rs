use std::{fs::File, io, path::Path};

use crate::{BlockSource, ReadError, ReadErrorKind, pread::read_full, readable_len};

/// A raw physical disk or volume device, opened read-only.
///
/// - macOS: `/dev/rdiskN`, `/dev/rdiskNsM`
/// - Windows: `\\.\PhysicalDriveN`, `\\.\E:`
/// - Linux: `/dev/sdX`, `/dev/loopN` (for CI tests only; not a product target)
///
/// Raw devices only accept reads aligned to sector boundaries, so misaligned reads are
/// rejected with [`ReadErrorKind::Misaligned`] without reaching the OS.
/// Wrap in [`Aligned`](crate::Aligned) to read at arbitrary offsets.
#[derive(Debug)]
pub struct Device {
    file: File,
    len: u64,
    sector_size: u32,
}

impl Device {
    /// Opens the device read-only and queries its size and sector size from the OS.
    /// Usually requires administrator privileges (except e.g. images attached with
    /// `hdiutil attach`).
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = sys::open(path.as_ref())?;
        let (len, sector_size) = sys::geometry(&file)?;
        if !sector_size.is_power_of_two() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("sector size is not a power of two: {sector_size}"),
            ));
        }
        Ok(Self {
            file,
            len,
            sector_size,
        })
    }
}

impl BlockSource for Device {
    fn len(&self) -> u64 {
        self.len
    }

    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        let want = readable_len(self.len, offset, buf.len());
        let Some(dst) = buf.get_mut(..want).filter(|d| !d.is_empty()) else {
            return Ok(0);
        };
        let ss = u64::from(self.sector_size);
        if !offset.is_multiple_of(ss) || !(want as u64).is_multiple_of(ss) {
            let range = offset..offset + want as u64;
            return Err(ReadError::new(range, ReadErrorKind::Misaligned));
        }
        read_full(&self.file, offset, dst)
    }
}

#[cfg(any(target_os = "macos", windows))]
fn overflow() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "device size overflows u64")
}

#[cfg(target_os = "macos")]
mod sys {
    use std::{fs::File, io, os::fd::AsRawFd, path::Path};

    // Reference: macOS SDK <sys/disk.h>: _IOR('d', 24, uint32_t), _IOR('d', 25, uint64_t)
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
    const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;

    pub fn open(path: &Path) -> io::Result<File> {
        File::open(path)
    }

    pub fn geometry(file: &File) -> io::Result<(u64, u32)> {
        let fd = file.as_raw_fd();
        let mut block_size: u32 = 0;
        let mut block_count: u64 = 0;
        // SAFETY: fd belongs to an open file, and each output matches the type the ioctl writes.
        let rc = unsafe { libc::ioctl(fd, DKIOCGETBLOCKSIZE, &raw mut block_size) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above.
        let rc = unsafe { libc::ioctl(fd, DKIOCGETBLOCKCOUNT, &raw mut block_count) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let len = block_count
            .checked_mul(u64::from(block_size))
            .ok_or_else(super::overflow)?;
        Ok((len, block_size))
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::{fs::File, io, os::fd::AsRawFd, path::Path};

    // Reference: <linux/fs.h>: _IO(0x12, 104), _IOR(0x12, 114, size_t)
    const BLKSSZGET: libc::c_ulong = 0x1268;
    const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;

    pub fn open(path: &Path) -> io::Result<File> {
        File::open(path)
    }

    pub fn geometry(file: &File) -> io::Result<(u64, u32)> {
        let fd = file.as_raw_fd();
        let mut sector_size: libc::c_int = 0;
        let mut len: u64 = 0;
        // SAFETY: fd belongs to an open file, and each output matches the type the ioctl writes.
        let rc = unsafe { libc::ioctl(fd, BLKSSZGET as _, &raw mut sector_size) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: as above.
        let rc = unsafe { libc::ioctl(fd, BLKGETSIZE64 as _, &raw mut len) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let sector_size = u32::try_from(sector_size)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid sector size"))?;
        Ok((len, sector_size))
    }
}

#[cfg(windows)]
mod sys {
    use std::{
        ffi::c_void,
        fs::{File, OpenOptions},
        io, mem,
        os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
        path::Path,
        ptr,
    };

    use windows_sys::Win32::{
        Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE},
        System::{
            IO::DeviceIoControl,
            Ioctl::{
                DISK_GEOMETRY_EX, GET_LENGTH_INFORMATION, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
                IOCTL_DISK_GET_LENGTH_INFO,
            },
        },
    };

    pub fn open(path: &Path) -> io::Result<File> {
        // Share modes let us open the device while other processes (including the OS) use it.
        // We only ever request read access.
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
    }

    /// DeviceIoControl with no input and `T` as output.
    ///
    /// Reference: Microsoft Learn, IOCTL_DISK_GET_LENGTH_INFO / IOCTL_DISK_GET_DRIVE_GEOMETRY_EX.
    fn ioctl_out<T>(file: &File, code: u32) -> io::Result<T> {
        let mut out = mem::MaybeUninit::<T>::zeroed();
        let mut returned = 0u32;
        let size =
            u32::try_from(mem::size_of::<T>()).map_err(|_| io::Error::other("output too large"))?;
        // SAFETY: the handle belongs to an open file, and the output buffer is sized for T.
        let ok = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                code,
                ptr::null(),
                0,
                out.as_mut_ptr().cast::<c_void>(),
                size,
                &raw mut returned,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the call succeeded, so T was written in full (it is also zero-initialized).
        Ok(unsafe { out.assume_init() })
    }

    pub fn geometry(file: &File) -> io::Result<(u64, u32)> {
        let len: GET_LENGTH_INFORMATION = ioctl_out(file, IOCTL_DISK_GET_LENGTH_INFO)?;
        let geo: DISK_GEOMETRY_EX = ioctl_out(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX)?;
        let len = u64::try_from(len.Length).map_err(|_| super::overflow())?;
        Ok((len, geo.Geometry.BytesPerSector))
    }
}
