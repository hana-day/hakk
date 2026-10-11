//! Carving as an analyzer.

use std::sync::Mutex;

use blocksource::BlockSource;
use carver::{fragments, jpeg, mp4, reader::Reader, scan::Options};
use index::{FileKind, NewFile, Preview, Status};

use crate::{Analyzer, Block, Context, RangeSet};

/// Finds JPEG and MP4/QuickTime files by their headers in each block and follows their
/// structure with the `carver` parsers.
///
/// Headers are checked at sector starts (files start on cluster boundaries in any file
/// system), using the block's lookahead for a header that crosses the block end. A file
/// that runs past its block is followed through [`Context::source`], which serves it from
/// the main pass (no seek, no second read) as long as it lies within the scan's read-ahead.
///
/// Positions inside a complete file already found are skipped, like `carver::scan` does,
/// so JPEG frames inside a video are not reported again. Blocks are analyzed in parallel,
/// so a block may be analyzed before the file covering it is found; those leftovers are
/// folded into their container afterwards by [`index::Index::merge_duplicates`].
#[derive(Debug)]
pub struct Carving {
    opts: Options,
    /// Ranges of complete, contiguous files found so far.
    claimed: Mutex<RangeSet>,
}

/// Bytes needed at a sector start to recognize a header.
const HEADER: usize = 8;

impl Default for Carving {
    fn default() -> Self {
        Self::with_options(Options::default())
    }
}

impl Carving {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_options(opts: Options) -> Self {
        Self {
            opts,
            claimed: Mutex::new(RangeSet::new()),
        }
    }

    fn is_claimed(&self, pos: u64) -> bool {
        self.claimed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(pos)
    }

    fn claim(&self, file: &NewFile) {
        if let [only] = file.extents.as_slice()
            && file.status == Status::Recoverable
        {
            self.claimed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .add(only.clone());
        }
    }
}

impl Analyzer for Carving {
    fn name(&self) -> &str {
        "carving"
    }

    fn lookahead(&self) -> usize {
        HEADER - 1
    }

    fn block(&self, block: &Block, cx: &mut Context<'_>) {
        let step = usize::try_from(cx.sector_size().max(512)).unwrap_or(512);
        let data = block.with_lookahead();
        let mut parse = None;
        for off in (0..block.data().len()).step_by(step) {
            let at = block.offset() + off as u64;
            let Some(head) = data.get(off..off + HEADER) else {
                break;
            };
            if !looks_like_header(head) || self.is_claimed(at) {
                continue;
            }
            let src = cx.source();
            let reader = parse.get_or_insert_with(|| Reader::new(src));
            if let Some(file) = carve_at(reader, at, head, &self.opts) {
                self.claim(&file);
                cx.report(file);
            }
        }
    }
}

fn looks_like_header(head: &[u8]) -> bool {
    head.starts_with(&[0xFF, 0xD8, 0xFF]) || head.get(4..8) == Some(b"ftyp")
}

/// Carves the file whose header is at `at`, the way `carver::scan` does for each header
/// it meets (JPEG with fragment reassembly when broken; MP4/QuickTime by its boxes).
fn carve_at<S: BlockSource + ?Sized>(
    r: &mut Reader<'_, S>,
    at: u64,
    head: &[u8],
    opts: &Options,
) -> Option<NewFile> {
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        let j = jpeg::carve(r, at, opts.max_jpeg)?;
        let mut file = NewFile::carved(FileKind::Jpeg, at, j.len);
        file.status = match j.status {
            jpeg::Status::Complete => Status::Recoverable,
            jpeg::Status::Corrupt { .. } | jpeg::Status::Truncated => Status::Damaged,
        };
        if let jpeg::Status::Corrupt { .. } = j.status
            && let Some(o) = opts.reassemble
            && let Some(joined) = fragments::reassemble(r, at, opts.max_jpeg, &o)
            && joined.complete
        {
            file.extents = joined
                .fragments
                .iter()
                .map(|&(off, len)| off..off.saturating_add(len))
                .collect();
            file.size = joined.fragments.iter().map(|f| f.1).sum();
            file.status = Status::Recoverable;
        }
        file.preview = Preview {
            width: Some(u32::from(j.width)).filter(|w| *w > 0),
            height: Some(u32::from(j.height)).filter(|h| *h > 0),
            ..Preview::default()
        };
        Some(file)
    } else {
        let m = mp4::carve(r, at, opts.max_video)?;
        let kind = if &m.brand == b"qt  " {
            FileKind::Mov
        } else {
            FileKind::Mp4
        };
        let mut file = NewFile::carved(kind, at, m.len);
        file.status = match m.status {
            mp4::Status::Complete => Status::Recoverable,
            mp4::Status::Corrupt { .. } => Status::Damaged,
            // No chunk could be checked: there is no `moov` (e.g. the recording was
            // interrupted), so the data may be fine but cannot be played as is.
            mp4::Status::Truncated if m.chunks_checked == 0 => Status::MissingIndex,
            mp4::Status::Truncated => Status::Damaged,
        };
        Some(file)
    }
}
