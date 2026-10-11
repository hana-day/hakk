//! A windowed reader over a [`BlockSource`] for parsers that walk forward byte by byte.

use blocksource::BlockSource;

/// Default window size.
const WINDOW: usize = 1 << 20;

/// Reads a source through a window that refills on demand.
///
/// Read errors and the end of the source both look like missing bytes (`None`), so parsers
/// treat an unreadable region as the end of the file. Bad sectors are the scanner's job.
pub struct Reader<'a, S: BlockSource + ?Sized> {
    src: &'a S,
    buf: Vec<u8>,
    /// Source offset of `buf[0]`.
    start: u64,
    /// Valid bytes in `buf`.
    filled: usize,
}

impl<'a, S: BlockSource + ?Sized> Reader<'a, S> {
    pub fn new(src: &'a S) -> Self {
        Self::with_window(src, WINDOW)
    }

    pub fn with_window(src: &'a S, window: usize) -> Self {
        Self {
            src,
            buf: vec![0; window.max(4096)],
            start: 0,
            filled: 0,
        }
    }

    pub fn len(&self) -> u64 {
        self.src.len()
    }

    pub fn is_empty(&self) -> bool {
        self.src.is_empty()
    }

    pub fn source(&self) -> &'a S {
        self.src
    }

    /// The byte at `pos`.
    pub fn byte(&mut self, pos: u64) -> Option<u8> {
        self.slice(pos, 1).and_then(|s| s.first().copied())
    }

    /// `len` bytes at `pos` (at most the window size), or `None` if they are not all
    /// available.
    pub fn slice(&mut self, pos: u64, len: usize) -> Option<&[u8]> {
        if len > self.buf.len() {
            return None;
        }
        let end = pos.checked_add(len as u64)?;
        if pos < self.start || end > self.start + self.filled as u64 {
            self.fill(pos)?;
        }
        if end > self.start + self.filled as u64 {
            return None;
        }
        let at = usize::try_from(pos - self.start).ok()?;
        self.buf.get(at..at + len)
    }

    /// A big-endian `u16` at `pos`.
    pub fn u16_be(&mut self, pos: u64) -> Option<u16> {
        self.slice(pos, 2)
            .and_then(|s| <[u8; 2]>::try_from(s).ok())
            .map(u16::from_be_bytes)
    }

    /// A big-endian `u32` at `pos`.
    pub fn u32_be(&mut self, pos: u64) -> Option<u32> {
        self.slice(pos, 4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .map(u32::from_be_bytes)
    }

    /// A big-endian `u64` at `pos`.
    pub fn u64_be(&mut self, pos: u64) -> Option<u64> {
        self.slice(pos, 8)
            .and_then(|s| <[u8; 8]>::try_from(s).ok())
            .map(u64::from_be_bytes)
    }

    /// Refills the window to start at `pos` (rounded down to a sector).
    fn fill(&mut self, pos: u64) -> Option<()> {
        let ss = u64::from(self.src.sector_size()).max(1);
        let start = pos / ss * ss;
        let n = self.src.read_at(start, &mut self.buf).ok()?;
        self.start = start;
        self.filled = n;
        (pos < start + n as u64).then_some(())
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    #[test]
    fn reads_across_window_refills() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        let src = MemSource::new(data.clone());
        let mut r = Reader::with_window(&src, 4096);
        for pos in [0u64, 4095, 4096, 10_000, 19_999] {
            assert_eq!(r.byte(pos), data.get(pos as usize).copied());
        }
        assert_eq!(
            r.slice(4094, 4).map(<[u8]>::to_vec),
            Some(data[4094..4098].to_vec())
        );
        assert_eq!(r.byte(20_000), None);
        assert_eq!(r.slice(19_998, 4), None);
        assert_eq!(r.u16_be(0), Some(u16::from_be_bytes([data[0], data[1]])));
    }
}
