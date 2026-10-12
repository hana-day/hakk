//! A windowed reader over a [`BlockSource`] for parsers that walk forward byte by byte.

use blocksource::BlockSource;

/// Default window size.
const WINDOW: usize = 128 << 10;
/// Number of windows kept.
const WINDOWS: usize = 8;

struct Window {
    buf: Vec<u8>,
    /// Source offset of `buf[0]`.
    start: u64,
    /// Valid bytes in `buf`.
    filled: usize,
    /// When it was last used (for least-recently-used replacement).
    used: u64,
}

impl Window {
    fn contains(&self, pos: u64, end: u64) -> bool {
        self.filled > 0 && pos >= self.start && end <= self.start + self.filled as u64
    }
}

/// Reads a source through a few windows that refill on demand.
///
/// Several windows let parsers that jump between places (such as fragment search, which
/// alternates between the end of one fragment and candidates far away) avoid re-reading.
/// Read errors and the end of the source both look like missing bytes (`None`), so parsers
/// treat an unreadable region as the end of the file. Bad sectors are the scanner's job.
pub struct Reader<'a, S: BlockSource + ?Sized> {
    src: &'a S,
    windows: Vec<Window>,
    /// The window used last (checked first).
    current: usize,
    clock: u64,
}

impl<'a, S: BlockSource + ?Sized> Reader<'a, S> {
    pub fn new(src: &'a S) -> Self {
        Self::with_window(src, WINDOW)
    }

    pub fn with_window(src: &'a S, window: usize) -> Self {
        let window = window.max(4096);
        Self {
            src,
            windows: (0..WINDOWS)
                .map(|_| Window {
                    buf: vec![0; window],
                    start: 0,
                    filled: 0,
                    used: 0,
                })
                .collect(),
            current: 0,
            clock: 0,
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

    /// Fills `buf` from `pos`, however long it is (in window-sized steps). `None` if the
    /// bytes are not all available.
    pub fn read_into(&mut self, pos: u64, buf: &mut [u8]) -> Option<()> {
        let step = self
            .windows
            .first()
            .map_or(4096, |w| w.buf.len() / 2)
            .max(1);
        for (i, part) in buf.chunks_mut(step).enumerate() {
            let at = pos.checked_add((i * step) as u64)?;
            part.copy_from_slice(self.slice(at, part.len())?);
        }
        Some(())
    }

    /// The byte at `pos`.
    pub fn byte(&mut self, pos: u64) -> Option<u8> {
        self.slice(pos, 1).and_then(|s| s.first().copied())
    }

    /// `len` bytes at `pos` (at most the window size), or `None` if they are not all
    /// available.
    pub fn slice(&mut self, pos: u64, len: usize) -> Option<&[u8]> {
        let end = pos.checked_add(len as u64)?;
        let i = self.window_for(pos, end)?;
        let w = self.windows.get(i)?;
        let at = usize::try_from(pos - w.start).ok()?;
        w.buf.get(at..at + len)
    }

    /// Finds or loads a window holding `[pos, end)`.
    fn window_for(&mut self, pos: u64, end: u64) -> Option<usize> {
        self.clock += 1;
        if self
            .windows
            .get(self.current)
            .is_some_and(|w| w.contains(pos, end))
        {
            let clock = self.clock;
            if let Some(w) = self.windows.get_mut(self.current) {
                w.used = clock;
            }
            return Some(self.current);
        }
        let hit = self.windows.iter().position(|w| w.contains(pos, end));
        let i = match hit {
            Some(i) => i,
            None => {
                let lru = self
                    .windows
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, w)| w.used)
                    .map(|(i, _)| i)?;
                self.fill(lru, pos)?;
                if !self.windows.get(lru)?.contains(pos, end) {
                    return None;
                }
                lru
            }
        };
        self.current = i;
        let clock = self.clock;
        if let Some(w) = self.windows.get_mut(i) {
            w.used = clock;
        }
        Some(i)
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

    /// Refills window `i` to start at `pos` (rounded down to a sector).
    fn fill(&mut self, i: usize, pos: u64) -> Option<()> {
        let ss = u64::from(self.src.sector_size()).max(1);
        let start = pos / ss * ss;
        let w = self.windows.get_mut(i)?;
        w.filled = 0;
        let n = self.src.read_at(start, &mut w.buf).ok()?;
        w.start = start;
        w.filled = n;
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
