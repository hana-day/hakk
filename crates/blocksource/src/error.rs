use std::{error::Error, fmt, io, ops::Range};

/// A failed read, carrying the range that could not be read.
///
/// Upper layers record this range as bad sectors and move on (retrying them all at the end).
#[derive(Debug)]
pub struct ReadError {
    range: Range<u64>,
    kind: ReadErrorKind,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ReadErrorKind {
    /// An I/O error reported by the OS (e.g. a bad sector).
    Io(io::Error),
    /// The device did not respond.
    Timeout,
    /// The device rejected a read that was not aligned to sector boundaries.
    Misaligned,
    /// The end of the source was reached before the requested length was read.
    UnexpectedEof,
}

impl ReadError {
    pub fn new(range: Range<u64>, kind: ReadErrorKind) -> Self {
        Self { range, kind }
    }

    /// The byte range that could not be read.
    pub fn range(&self) -> Range<u64> {
        self.range.clone()
    }

    pub fn kind(&self) -> &ReadErrorKind {
        &self.kind
    }

    pub fn into_kind(self) -> ReadErrorKind {
        self.kind
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Range { start, end } = self.range;
        write!(f, "failed to read {start:#x}..{end:#x}: ")?;
        match &self.kind {
            ReadErrorKind::Io(e) => write!(f, "{e}"),
            ReadErrorKind::Timeout => f.write_str("timed out"),
            ReadErrorKind::Misaligned => f.write_str("not aligned to sector boundaries"),
            ReadErrorKind::UnexpectedEof => f.write_str("unexpected end of source"),
        }
    }
}

impl Error for ReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            ReadErrorKind::Io(e) => Some(e),
            _ => None,
        }
    }
}
