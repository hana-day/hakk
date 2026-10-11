//! Messages between the scan's threads.
//!
//! ```text
//!            Command (pause / resume / cancel)
//!  handle ─────────────────────────────┐
//!  Foreground ── FgMsg (restore reads) ┤
//!                                      ▼
//!                               ┌────────────┐  Job (blocks, read replies)  ┌─────────┐
//!   BlockSource ◀── all I/O ─── │   reader   │ ───────────────────────────▶ │ workers │
//!                               └────────────┘ ◀─────────────────────────── └─────────┘
//!                                      │        ToReader (read requests)       │
//!                                      │ CoordMsg                     CoordMsg │
//!                                      ▼                                       ▼
//!                               ┌─────────────────────────────────────────────────┐
//!                               │ coordinator: progress events, Store checkpoints │
//!                               └─────────────────────────────────────────────────┘
//! ```

use std::ops::Range;

use blocksource::ReadError;
use crossbeam_channel::Sender;

use crate::{
    Finding,
    analyzer::{Block, ReadReply, ReadRequest},
    progress::{Outcome, Phase, RunState},
};

/// Work for the analyzer workers.
#[derive(Debug)]
pub(crate) enum Job {
    Block(Block),
    Reply {
        analyzer: usize,
        origin: Option<Range<u64>>,
        reply: ReadReply,
    },
}

/// From workers to the reader.
#[derive(Debug)]
pub(crate) enum ToReader {
    Request {
        analyzer: usize,
        /// The block whose analysis led to this request (see [`CoordMsg::JobFinished`]).
        origin: Option<Range<u64>>,
        req: ReadRequest,
    },
    /// A job finished; sent after the requests it made.
    JobDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    Pause,
    Resume,
    Cancel,
}

/// From [`crate::Foreground`] to the reader.
#[derive(Debug)]
pub(crate) enum FgMsg {
    Begin,
    End,
    Read {
        offset: u64,
        len: usize,
        reply: Sender<Result<Vec<u8>, ReadError>>,
    },
}

/// To the coordinator.
#[derive(Debug)]
pub(crate) enum CoordMsg {
    State(RunState),
    Phase(Phase),
    /// The main pass read this planned range (bad parts included).
    Read(Range<u64>),
    Bad {
        range: Range<u64>,
        attempts: u32,
    },
    Recovered(Range<u64>),
    /// Where a retry pass is reading.
    RetryPosition(u64),
    RetryPassDone(u32),
    /// A worker finished a job.
    ///
    /// A block counts as done only when it and every read requested while analyzing it
    /// (transitively) have been analyzed. Otherwise a resumed scan would skip the block
    /// while the files those reads would have revealed were never saved.
    JobFinished {
        origin: Option<Range<u64>>,
        /// Set for block jobs: the block's unreadable parts (not done).
        block_holes: Option<Vec<Range<u64>>>,
        /// Reads requested while handling this job.
        spawned: usize,
        found: Vec<Finding>,
    },
    AnalyzerPanicked {
        analyzer: String,
        offset: Option<u64>,
    },
    ReaderDone(Outcome),
}
