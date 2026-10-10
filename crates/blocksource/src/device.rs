use std::{fs::File, io, path::Path};

use crate::{BlockSource, ReadError, ReadErrorKind, pread::read_full, readable_len};

/// 物理ディスクまたはボリュームの生デバイス。読み取り専用で開く。
///
/// - macOS：`/dev/rdiskN`、`/dev/rdiskNsM`
/// - Windows：`\\.\PhysicalDriveN`、`\\.\E:`
/// - Linux：`/dev/sdX`、`/dev/loopN`（CIでのテスト用。製品の対象外）
///
/// 生デバイスは位置と長さがセクタ境界に揃った読み込みしか受け付けないので、
/// 揃っていない読み込みは OS に渡さず [`ReadErrorKind::Misaligned`] で返す。
/// 任意の位置を読むときは [`Aligned`](crate::Aligned) を被せる。
#[derive(Debug)]
pub struct Device {
    file: File,
    len: u64,
    sector_size: u32,
}

impl Device {
    /// 読み取り専用で開き、大きさとセクタサイズを OS から取得する。
    /// 通常は管理者権限が必要（`hdiutil attach` したイメージなどは除く）。
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = sys::open(path.as_ref())?;
        let (len, sector_size) = sys::geometry(&file)?;
        if !sector_size.is_power_of_two() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("セクタサイズが2のべき乗でない: {sector_size}"),
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
    io::Error::new(
        io::ErrorKind::InvalidData,
        "デバイスの大きさが u64 を超える",
    )
}

#[cfg(target_os = "macos")]
mod sys {
    use std::{fs::File, io, os::fd::AsRawFd, path::Path};

    // <sys/disk.h>：_IOR('d', 24, uint32_t)、_IOR('d', 25, uint64_t)
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
    const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;

    pub fn open(path: &Path) -> io::Result<File> {
        File::open(path)
    }

    pub fn geometry(file: &File) -> io::Result<(u64, u32)> {
        let fd = file.as_raw_fd();
        let mut block_size: u32 = 0;
        let mut block_count: u64 = 0;
        // SAFETY: fd は開いているファイルのもの。出力先はそれぞれの ioctl が書き込む型と一致する
        let rc = unsafe { libc::ioctl(fd, DKIOCGETBLOCKSIZE, &raw mut block_size) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 同上
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

    // <linux/fs.h>：_IO(0x12, 104)、_IOR(0x12, 114, size_t)
    const BLKSSZGET: libc::c_ulong = 0x1268;
    const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;

    pub fn open(path: &Path) -> io::Result<File> {
        File::open(path)
    }

    pub fn geometry(file: &File) -> io::Result<(u64, u32)> {
        let fd = file.as_raw_fd();
        let mut sector_size: libc::c_int = 0;
        let mut len: u64 = 0;
        // SAFETY: fd は開いているファイルのもの。出力先はそれぞれの ioctl が書き込む型と一致する
        let rc = unsafe { libc::ioctl(fd, BLKSSZGET as _, &raw mut sector_size) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 同上
        let rc = unsafe { libc::ioctl(fd, BLKGETSIZE64 as _, &raw mut len) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let sector_size = u32::try_from(sector_size)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "不正なセクタサイズ"))?;
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
        // 他のプロセス（OS自身を含む）がデバイスを使っていても開けるよう共有モードを指定する。
        // こちらは読み取りしか要求しない
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
    }

    /// `T` を出力とする引数なしの DeviceIoControl。
    fn ioctl_out<T>(file: &File, code: u32) -> io::Result<T> {
        let mut out = mem::MaybeUninit::<T>::zeroed();
        let mut returned = 0u32;
        let size =
            u32::try_from(mem::size_of::<T>()).map_err(|_| io::Error::other("出力が大きすぎる"))?;
        // SAFETY: ハンドルは開いているファイルのもの。出力バッファは T の大きさで確保してある
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
        // SAFETY: 成功したので T 全体が書き込まれている（ゼロ初期化済みでもある）
        Ok(unsafe { out.assume_init() })
    }

    pub fn geometry(file: &File) -> io::Result<(u64, u32)> {
        let len: GET_LENGTH_INFORMATION = ioctl_out(file, IOCTL_DISK_GET_LENGTH_INFO)?;
        let geo: DISK_GEOMETRY_EX = ioctl_out(file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX)?;
        let len = u64::try_from(len.Length).map_err(|_| super::overflow())?;
        Ok((len, geo.Geometry.BytesPerSector))
    }
}
