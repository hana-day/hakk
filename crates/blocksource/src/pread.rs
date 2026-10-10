use std::{fs::File, io};

use crate::{ReadError, ReadErrorKind};

/// `offset` から `buf` を満たすまで位置指定で読む。途中で終端に達したら読めた分だけ返す。
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
    // ファイルポインタは動くが、毎回位置を指定して読むので影響しない
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}
