//! Carving as an analyzer.

use std::sync::Mutex;

use carver::{
    Found,
    reader::Reader,
    scan::{Options, carve_at, looks_like_header},
};
use index::{FileKind, NewFile, Preview, Status};

use crate::{Analyzer, Block, Context, RangeSet};

/// Finds JPEG and MP4/QuickTime files by their headers in each block and carves them with
/// `carver::scan::carve_at` (the same per-header step as `carver::scan`).
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
const HEADER: usize = carver::scan::HEADER_LEN;

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

    fn claim_span(&self, span: std::ops::Range<u64>) {
        self.claimed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .add(span);
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
            if let Some(found) = carve_at(reader, at, &self.opts) {
                // A transport stream repeats table headers throughout; once followed, its
                // fragments are not searched again even if it is broken.
                for &(off, len) in found.claimed() {
                    self.claim_span(off..off.saturating_add(len));
                }
                cx.report(to_new_file(found));
            }
        }
    }
}

/// A carver result as an index record.
fn to_new_file(f: Found) -> NewFile {
    let kind = match f.kind {
        carver::Kind::Jpeg => FileKind::Jpeg,
        carver::Kind::Mp4 => FileKind::Mp4,
        carver::Kind::Mov => FileKind::Mov,
        carver::Kind::Heic => FileKind::Heic,
        carver::Kind::Mts => FileKind::Mts,
    };
    let mut file = NewFile::carved(kind, f.offset, f.len);
    file.extents = f
        .fragments
        .iter()
        .map(|&(off, len)| off..off.saturating_add(len))
        .collect();
    file.status = match f.status {
        carver::Status::Complete => Status::Recoverable,
        carver::Status::Corrupt { .. } => Status::Damaged,
        // A video whose `moov` is missing (e.g. the recording was interrupted): the data
        // is usually fine, but it cannot be played until the index is rebuilt.
        carver::Status::Truncated if f.has_index == Some(false) => Status::MissingIndex,
        carver::Status::Truncated => Status::Damaged,
    };
    file.preview = Preview {
        width: f.width.filter(|w| *w > 0),
        height: f.height.filter(|h| *h > 0),
        ..Preview::default()
    };
    file
}
