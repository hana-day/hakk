//! Where a fragmented file's bytes are while it is being followed: closed fragments, then
//! an open one running on from its last cut. Offsets in the file ("logical") map to source
//! offsets through it.

use blocksource::BlockSource;

use crate::reader::Reader;

#[derive(Debug, Clone)]
pub struct Pieces {
    /// `(source offset, length)` in file order.
    closed: Vec<(u64, u64)>,
    /// `(source offset, logical offset)` of the open fragment.
    open: (u64, u64),
}

impl Pieces {
    /// A file starting at source offset `start`, in one piece so far.
    pub fn new(start: u64) -> Self {
        Self {
            closed: Vec::new(),
            open: (start, 0),
        }
    }

    /// Number of fragments, the open one included.
    pub fn count(&self) -> usize {
        self.closed.len() + 1
    }

    /// Logical offset where the open fragment begins.
    pub fn open_from(&self) -> u64 {
        self.open.1
    }

    /// Source offset of logical offset `logical`.
    pub fn physical(&self, logical: u64) -> u64 {
        if logical >= self.open.1 {
            return self.open.0 + (logical - self.open.1);
        }
        let mut from = 0u64;
        for &(o, l) in &self.closed {
            if logical < from + l {
                return o + (logical - from);
            }
            from += l;
        }
        self.open.0
    }

    /// The pieces with the open fragment closed at logical offset `cut` and a new one from
    /// source offset `to`.
    pub fn cut(&self, cut: u64, to: u64) -> Self {
        let mut closed = self.closed.clone();
        closed.push((self.open.0, cut.saturating_sub(self.open.1)));
        Self {
            closed,
            open: (to, cut),
        }
    }

    /// Whether source offset `at` lies in the file's own data (up to logical offset `end`).
    pub fn overlaps(&self, at: u64, end: u64) -> bool {
        (self.open.0..self.physical(end)).contains(&at)
            || self
                .closed
                .iter()
                .any(|&(o, l)| (o..o.saturating_add(l)).contains(&at))
    }

    /// The fragments of the file's first `len` bytes.
    pub fn fragments(&self, len: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut from = 0u64;
        for &(o, l) in &self.closed {
            if from >= len {
                break;
            }
            out.push((o, l.min(len - from)));
            from += l;
        }
        if len > self.open.1 {
            out.push((self.open.0, len - self.open.1));
        }
        out
    }

    /// Fills `buf` from logical offset `logical`, across cuts. `None` if the data runs out.
    pub fn read<S: BlockSource + ?Sized>(
        &self,
        r: &mut Reader<S>,
        logical: u64,
        buf: &mut [u8],
    ) -> Option<()> {
        let mut done = 0usize;
        while done < buf.len() {
            let at = logical + done as u64;
            // Bytes left in the piece holding `at`.
            let room = if at >= self.open.1 {
                u64::MAX
            } else {
                let mut from = 0u64;
                let mut room = 0u64;
                for &(_, l) in &self.closed {
                    if at < from + l {
                        room = from + l - at;
                        break;
                    }
                    from += l;
                }
                room
            };
            let n = usize::try_from(room)
                .unwrap_or(usize::MAX)
                .min(buf.len() - done);
            if n == 0 {
                return None;
            }
            let src = r.slice(self.physical(at), n)?;
            buf.get_mut(done..done + n)?.copy_from_slice(src);
            done += n;
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    #[test]
    fn reads_across_cuts() {
        let data: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        // Logical 0..100 at 1000, 100..150 at 3000, then from 200 onwards.
        let p = Pieces::new(1000).cut(100, 3000).cut(150, 200);
        let mut buf = [0u8; 200];
        p.read(&mut r, 50, &mut buf).unwrap();
        assert_eq!(buf[0], u8::try_from(1050 % 256).unwrap());
        assert_eq!(buf[50], u8::try_from(3000 % 256).unwrap());
        assert_eq!(buf[100], 200);
        assert_eq!(p.physical(120), 3020);
        assert_eq!(p.fragments(160), vec![(1000, 100), (3000, 50), (200, 10)]);
    }
}
