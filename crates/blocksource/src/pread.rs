use std::{fs::File, io};

use crate::{ReadError, ReadErrorKind};

/// Positional read until `buf` is full. Returns the bytes read so far if the end is reached.
pub(crate) fn read_full(file: &File, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
    let mut done = 0;
    while let Some(rest) = buf.get_mut(done..).filter(|r| !r.is_empty()) {
        let pos = offset + done as u64;
        match pread(file, rest, pos) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                let end = offset + buf.len() as u64;
                return Err(ReadError::new(pos..end, ReadErrorKind::Io(e)));
            }
        }
    }
    Ok(done)
}

#[cfg(unix)]
fn pread(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn pread(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    // This moves the file pointer, which is harmless since every read passes an offset.
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}
