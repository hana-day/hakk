//! The interface analyzers (file system parsing, carving) plug into.

use std::ops::Range;

use crate::Finding;

/// Something that looks at the disk data as the scan reads it and reports files.
///
/// # How data arrives
///
/// The scan reads the planned ranges once, front to back, in blocks of 1–4 MiB
/// ([`Block`]). Every analyzer sees every block, all through the same buffer (it is never
/// copied per analyzer). Blocks are processed by a pool of worker threads, so **calls may
/// be concurrent and out of order**; an analyzer keeps any state behind its own lock.
///
/// # Data crossing a block boundary
///
/// A block exposes up to [`Analyzer::lookahead`] bytes past its end
/// ([`Block::with_lookahead`]), copied from the start of the next block when the two are
/// contiguous. The rule that keeps results unique: **report a hit in the block where it
/// starts**, and use the lookahead only to finish looking at it. With a lookahead of at
/// least `signature length - 1`, every signature is seen exactly once. When the next block
/// is not contiguous (free-space scans) or could not be read, the lookahead is shorter or
/// empty.
///
/// # Reading more
///
/// For anything else (file system metadata before the scan reaches it, following a
/// structure past the lookahead, a fragment elsewhere), request a read with
/// [`Context::read`]. The data arrives later in [`Analyzer::read_done`]. All reads,
/// including these, are ordered by the scan's single reader thread, so analyzers never
/// cause the disk head to wander on their own.
pub trait Analyzer: Send + Sync {
    /// Short name for logs and error reports.
    fn name(&self) -> &str;

    /// Bytes past the end of each block this analyzer wants to see. The scan uses the
    /// largest value among its analyzers, capped at the block size.
    fn lookahead(&self) -> usize {
        0
    }

    /// Called once before the scan starts (also when resuming), e.g. to request the file
    /// system metadata with [`Priority::Immediate`] so named files appear first.
    fn start(&self, cx: &mut Context<'_>) {
        let _ = cx;
    }

    /// Called for every block the scan reads.
    fn block(&self, block: &Block, cx: &mut Context<'_>);

    /// Called with the data of a read requested through [`Context::read`].
    fn read_done(&self, reply: &ReadReply, cx: &mut Context<'_>) {
        let _ = (reply, cx);
    }

    /// Called once after everything was read, unless the scan was cancelled. Reads
    /// requested here are ignored.
    fn finish(&self, cx: &mut Context<'_>) {
        let _ = cx;
    }
}

/// A chunk of the disk as read by the scan.
#[derive(Debug)]
pub struct Block {
    pub(crate) offset: u64,
    /// The block's own bytes followed by room for the lookahead.
    pub(crate) buf: Vec<u8>,
    pub(crate) len: usize,
    pub(crate) lookahead: usize,
    pub(crate) holes: Vec<Range<u64>>,
    pub(crate) retry: bool,
}

impl Block {
    /// Position of the first byte on the source.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn end(&self) -> u64 {
        self.offset + self.len as u64
    }

    pub fn range(&self) -> Range<u64> {
        self.offset..self.end()
    }

    /// The block's own bytes. Unreadable parts ([`Block::holes`]) are zero-filled.
    pub fn data(&self) -> &[u8] {
        self.buf.get(..self.len).unwrap_or_default()
    }

    /// The block's bytes followed by the lookahead (bytes of the next block).
    pub fn with_lookahead(&self) -> &[u8] {
        self.buf
            .get(..self.len + self.lookahead)
            .unwrap_or_default()
    }

    /// Just the lookahead bytes.
    pub fn lookahead(&self) -> &[u8] {
        self.buf
            .get(self.len..self.len + self.lookahead)
            .unwrap_or_default()
    }

    /// Ranges of this block (absolute offsets) that could not be read. They are recorded
    /// as bad and retried at the end of the scan; recovered data then arrives again as a
    /// separate block with [`Block::is_retry`] set.
    pub fn holes(&self) -> &[Range<u64>] {
        &self.holes
    }

    /// Whether this block holds data recovered by retrying a bad range. Such blocks
    /// arrive after the main pass, out of disk order, and have no lookahead.
    pub fn is_retry(&self) -> bool {
        self.retry
    }
}

/// How urgently a requested read is needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Before the next scan block. Costs a seek away and back on an HDD, so use it for
    /// things worth showing early (file system metadata) or blocking further analysis.
    Immediate,
    /// After the main pass, in disk order (one sweep). Use for anything that can wait.
    Deferred,
}

/// A read an analyzer asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRequest {
    pub range: Range<u64>,
    pub priority: Priority,
    /// Opaque value handed back in [`ReadReply::tag`].
    pub tag: u64,
}

/// The data of a requested read.
#[derive(Debug)]
pub struct ReadReply {
    pub tag: u64,
    pub offset: u64,
    /// The bytes read. Shorter than requested if the request went past the end of the
    /// source or exceeded the maximum request size ([`crate::MAX_REQUEST_LEN`]).
    pub data: Vec<u8>,
    /// Ranges (absolute offsets) that could not be read; zero-filled in `data`.
    pub holes: Vec<Range<u64>>,
}

/// What an analyzer can do from inside a callback: report files and request reads.
#[derive(Debug)]
pub struct Context<'a> {
    pub(crate) found: &'a mut Vec<Finding>,
    pub(crate) requests: &'a mut Vec<ReadRequest>,
    pub(crate) source_len: u64,
    pub(crate) sector_size: u32,
}

impl Context<'_> {
    /// Reports a file. Only its location is stored; re-reporting the same file is
    /// harmless (the index ignores duplicates).
    pub fn report(&mut self, file: Finding) {
        self.found.push(file);
    }

    /// Requests a read. The data arrives in [`Analyzer::read_done`] with the same `tag`.
    pub fn read(&mut self, range: Range<u64>, priority: Priority, tag: u64) {
        self.requests.push(ReadRequest {
            range,
            priority,
            tag,
        });
    }

    pub fn source_len(&self) -> u64 {
        self.source_len
    }

    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }
}

/// Lets the caller keep a handle to an analyzer (e.g. to read its state after the scan).
impl<T: Analyzer + ?Sized> Analyzer for std::sync::Arc<T> {
    fn name(&self) -> &str {
        (**self).name()
    }

    fn lookahead(&self) -> usize {
        (**self).lookahead()
    }

    fn start(&self, cx: &mut Context<'_>) {
        (**self).start(cx)
    }

    fn block(&self, block: &Block, cx: &mut Context<'_>) {
        (**self).block(block, cx)
    }

    fn read_done(&self, reply: &ReadReply, cx: &mut Context<'_>) {
        (**self).read_done(reply, cx)
    }

    fn finish(&self, cx: &mut Context<'_>) {
        (**self).finish(cx)
    }
}
