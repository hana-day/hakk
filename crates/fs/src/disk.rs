//! Reads within one volume, aligned to the source's sectors.

use blocksource::{BlockSource, ReadError, ReadErrorKind};

pub(crate) struct Disk<'a> {
    src: &'a dyn BlockSource,
    start: u64,
    len: u64,
}

impl<'a> Disk<'a> {
    /// A view of `len` bytes at `start`, clipped to the end of the source.
    pub(crate) fn new(src: &'a dyn BlockSource, start: u64, len: u64) -> Self {
        let len = len.min(src.len().saturating_sub(start));
        Self { src, start, len }
    }

    pub(crate) fn start(&self) -> u64 {
        self.start
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Reads `len` bytes at `offset` (relative to the volume).
    pub(crate) fn read(&self, offset: u64, len: usize) -> Result<Vec<u8>, ReadError> {
        let abs = self.start.saturating_add(offset);
        let end = offset.saturating_add(len as u64);
        if end > self.len {
            let s = abs.saturating_add(self.len.saturating_sub(offset));
            return Err(ReadError::new(
                s..abs.saturating_add(len as u64),
                ReadErrorKind::UnexpectedEof,
            ));
        }
        read_aligned(self.src, abs, len)
    }

    /// Like [`Disk::read`], but unreadable sectors (and anything past the end of the
    /// source) read as zeros. Each failure is described in `warnings`.
    pub(crate) fn read_lossy(
        &self,
        offset: u64,
        len: usize,
        warnings: &mut Vec<String>,
    ) -> Vec<u8> {
        if let Ok(b) = self.read(offset, len) {
            return b;
        }
        // Retry piecewise so one bad sector does not take the whole range with it.
        let step = usize::try_from(self.src.sector_size())
            .unwrap_or(512)
            .max(512);
        let mut out = vec![0u8; len];
        let mut failed: Option<ReadError> = None;
        let mut failures = 0u64;
        for (i, chunk) in out.chunks_mut(step).enumerate() {
            let at = offset.saturating_add((i as u64).saturating_mul(step as u64));
            match self.read(at, chunk.len()) {
                Ok(b) => chunk.copy_from_slice(&b),
                Err(e) => {
                    failures += 1;
                    failed.get_or_insert(e);
                }
            }
        }
        if let Some(e) = failed {
            warnings.push(format!(
                "{e} ({failures} unreadable sector(s) read as zeros)"
            ));
        }
        out
    }
}

fn read_aligned(src: &dyn BlockSource, abs: u64, len: usize) -> Result<Vec<u8>, ReadError> {
    let ss = u64::from(src.sector_size().max(1));
    let a = abs - abs % ss;
    let end = abs.saturating_add(len as u64);
    let b = end.div_ceil(ss).saturating_mul(ss).min(src.len().max(end));
    let total = usize::try_from(b - a)
        .map_err(|_| ReadError::new(abs..end, ReadErrorKind::UnexpectedEof))?;
    let head = usize::try_from(abs - a).unwrap_or(0);
    if head == 0 && total == len {
        let mut buf = vec![0u8; len];
        src.read_exact_at(abs, &mut buf)?;
        return Ok(buf);
    }
    let mut buf = vec![0u8; total];
    // The aligned tail may run past the end of the source; only the requested part must
    // be read.
    let n = src.read_at(a, &mut buf)?;
    if n < head.saturating_add(len) {
        return Err(ReadError::new(
            a.saturating_add(n as u64)..end,
            ReadErrorKind::UnexpectedEof,
        ));
    }
    buf.truncate(head + len);
    buf.drain(..head);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    #[test]
    fn reads_unaligned_ranges() {
        let data: Vec<u8> = (0..2048u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let src = MemSource::new(data.clone());
        let d = Disk::new(&src, 512, 1536);
        assert_eq!(d.read(3, 600).unwrap(), data[515..1115]);
        assert!(d.read(1500, 100).is_err());
        let mut w = Vec::new();
        let b = d.read_lossy(1500, 100, &mut w);
        assert_eq!(b.len(), 100);
        assert_eq!(w.len(), 1);
    }
}
