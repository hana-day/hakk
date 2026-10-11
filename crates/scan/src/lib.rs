//! The scan pipeline.
//!
//! One reader thread reads the disk front to back in 1–4 MiB blocks through a
//! [`BlockSource`] and hands each block to a pool of workers over `crossbeam` channels.
//! Every [`Analyzer`] (file system parsing, carving) sees every block through the same
//! buffer. Findings and progress go to a coordinator thread, which saves them to a
//! [`Store`] (the SQLite [`index`]) in checkpoints and emits [`Event`]s for the GUI.
//!
//! - Reads are ordered in one place (the reader thread), so an HDD head never wanders:
//!   restores ([`ScanHandle::foreground`]) pause the scan and it resumes where it stopped.
//! - Unreadable ranges are skipped, recorded, and retried at the end (ddrescue style).
//! - Pause, resume, cancel; a cancelled or interrupted scan resumes from its saved state
//!   ([`Scan::resume_from`]).
//! - Free-space-only scans read just the given ranges ([`Scan::ranges`]).

mod analyzer;
mod clock;
mod coord;
mod msg;
mod progress;
mod ranges;
mod reader;
mod store;
mod worker;

use std::{
    error::Error,
    fmt, io,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use blocksource::{BlockSource, ReadError};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
pub use index;
use index::{Media, ScanState, SourceInfo};

pub use analyzer::{Analyzer, Block, Context, Priority, ReadReply, ReadRequest};
pub use clock::{Clock, SystemClock};
pub use progress::{
    CellState, DiskMap, Event, FoundCounts, Outcome, Phase, Progress, RetryProgress, RunState,
    Summary,
};
pub use ranges::{RangeMap, RangeSet};
pub use reader::MAX_REQUEST_LEN;
pub use store::{MemStore, MemStoreData, Store, StoreError};

use crate::{
    coord::{CoordConfig, CoordInit, Coordinator},
    msg::{Command, FgMsg},
    reader::{Channels, PendingRequest, Reader, ReaderConfig},
    worker::{Analyzers, Worker},
};

/// A file found by an analyzer. Only its location is recorded, never the data.
pub type Finding = index::NewFile;

const MIN_BLOCK: usize = 64 << 10;
const MAX_BLOCK: usize = 64 << 20;

/// Configures and starts a scan.
pub struct Scan {
    source: Arc<dyn BlockSource>,
    analyzers: Vec<Box<dyn Analyzer>>,
    ranges: Option<Vec<Range<u64>>>,
    resume: Option<ScanState>,
    label: String,
    known_files: Vec<(Option<u64>, Media)>,
    block_size: usize,
    workers: usize,
    retry_passes: u32,
    retry_chunk: u64,
    max_errors_per_block: u32,
    clock: Arc<dyn Clock>,
    progress_interval: Duration,
    checkpoint_interval: Duration,
    map_cells: usize,
}

impl fmt::Debug for Scan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scan")
            .field(
                "analyzers",
                &self.analyzers.iter().map(|a| a.name()).collect::<Vec<_>>(),
            )
            .field("block_size", &self.block_size)
            .field("workers", &self.workers)
            .finish_non_exhaustive()
    }
}

impl Scan {
    pub fn new(source: Arc<dyn BlockSource>) -> Self {
        let cpus = thread::available_parallelism().map_or(2, |n| n.get());
        Self {
            source,
            analyzers: Vec::new(),
            ranges: None,
            resume: None,
            label: String::new(),
            known_files: Vec::new(),
            block_size: 4 << 20,
            workers: cpus.saturating_sub(1).max(1),
            retry_passes: 2,
            retry_chunk: 64 << 10,
            max_errors_per_block: 4,
            clock: Arc::new(SystemClock::default()),
            progress_interval: Duration::from_millis(200),
            checkpoint_interval: Duration::from_secs(2),
            map_cells: 576,
        }
    }

    pub fn analyzer(mut self, analyzer: impl Analyzer + 'static) -> Self {
        self.analyzers.push(Box::new(analyzer));
        self
    }

    /// Reads only these ranges (e.g. free space from the allocation bitmap) instead of the
    /// whole source. Ranges are clipped to the source and widened to sector boundaries.
    pub fn ranges(mut self, ranges: impl IntoIterator<Item = Range<u64>>) -> Self {
        self.ranges = Some(ranges.into_iter().collect());
        self
    }

    /// Continues a saved scan: reads only what it had not read, and retries its bad
    /// ranges. [`Scan::ranges`] is ignored; the saved plan is used.
    pub fn resume_from(mut self, state: ScanState) -> Self {
        self.resume = Some(state);
        self
    }

    /// Description of the source saved with a new scan.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Files found by an earlier session of a resumed scan (offset if known, media), so
    /// counts and the disk map include them.
    pub fn known_files(mut self, files: impl IntoIterator<Item = (Option<u64>, Media)>) -> Self {
        self.known_files = files.into_iter().collect();
        self
    }

    /// Bytes per block (default 4 MiB). Must be a multiple of the sector size, between
    /// 64 KiB and 64 MiB.
    pub fn block_size(mut self, bytes: usize) -> Self {
        self.block_size = bytes;
        self
    }

    /// Number of analyzer worker threads (default: CPUs - 1).
    pub fn workers(mut self, n: usize) -> Self {
        self.workers = n;
        self
    }

    /// How many times bad ranges are retried at the end (default 2).
    pub fn retry_passes(mut self, n: u32) -> Self {
        self.retry_passes = n;
        self
    }

    /// Size of each read while retrying (default 64 KiB). Failed chunks are narrowed down
    /// to single sectors.
    pub fn retry_chunk(mut self, bytes: u64) -> Self {
        self.retry_chunk = bytes;
        self
    }

    /// Errors tolerated inside one block in the main pass before the rest of the block is
    /// skipped and left to the retries (default 4). Keeps a weak area from stalling the
    /// main pass.
    pub fn max_errors_per_block(mut self, n: u32) -> Self {
        self.max_errors_per_block = n;
        self
    }

    /// The clock used for speed, remaining time and checkpoint timing.
    pub fn clock(mut self, clock: impl Clock + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    pub fn progress_interval(mut self, d: Duration) -> Self {
        self.progress_interval = d;
        self
    }

    pub fn checkpoint_interval(mut self, d: Duration) -> Self {
        self.checkpoint_interval = d;
        self
    }

    /// Number of cells in the [`DiskMap`] (default 576).
    pub fn map_cells(mut self, n: usize) -> Self {
        self.map_cells = n;
        self
    }

    /// Starts the scan in background threads.
    pub fn start(self, store: impl Store + 'static) -> Result<ScanHandle, ScanError> {
        self.start_boxed(Box::new(store))
    }

    fn start_boxed(self, mut store: Box<dyn Store>) -> Result<ScanHandle, ScanError> {
        let source = self.source;
        let len = source.len();
        let sector_size = source.sector_size();
        let sector = u64::from(sector_size).max(1);
        if !(MIN_BLOCK..=MAX_BLOCK).contains(&self.block_size)
            || !(self.block_size as u64).is_multiple_of(sector)
        {
            return Err(ScanError::InvalidConfig(
                "block size must be a multiple of the sector size between 64 KiB and 64 MiB",
            ));
        }
        if self.workers == 0 {
            return Err(ScanError::InvalidConfig("at least one worker is needed"));
        }
        if self.retry_chunk == 0 || !self.retry_chunk.is_multiple_of(sector) {
            return Err(ScanError::InvalidConfig(
                "retry chunk must be a non-zero multiple of the sector size",
            ));
        }

        // What to read.
        let (planned, done, bad, retry_passes_done) = match self.resume {
            Some(state) => {
                if state.source.len != len || state.source.sector_size != sector_size {
                    return Err(ScanError::SourceMismatch {
                        saved: state.source,
                        actual_len: len,
                        actual_sector_size: sector_size,
                    });
                }
                let planned: RangeSet = state.planned.into_iter().collect();
                let done: RangeSet = state.progress.done.into_iter().collect();
                let mut bad = RangeMap::new();
                for b in state.progress.bad {
                    bad.insert(b.range, b.attempts);
                }
                (planned, done, bad, state.progress.retry_passes)
            }
            None => {
                let requested = self
                    .ranges
                    .unwrap_or_else(|| std::iter::once(0..len).collect());
                let planned: RangeSet = requested
                    .into_iter()
                    .map(|r| {
                        let start = (r.start / sector * sector).min(len);
                        let end = r.end.div_ceil(sector).saturating_mul(sector).min(len);
                        start..end.max(start)
                    })
                    .collect();
                let info = SourceInfo {
                    len,
                    sector_size,
                    label: self.label,
                };
                store
                    .begin(&info, &planned.to_vec())
                    .map_err(ScanError::Store)?;
                (planned, RangeSet::new(), RangeMap::new(), 0)
            }
        };
        let mut remaining = planned.clone();
        for r in done.ranges() {
            remaining.remove(r);
        }
        for (r, _) in bad.iter() {
            remaining.remove(r);
        }

        let analyzers: Analyzers = self.analyzers.into();
        let lookahead = analyzers
            .iter()
            .map(|a| a.lookahead())
            .max()
            .unwrap_or(0)
            .min(self.block_size);

        // Let analyzers queue their first reads (e.g. file system metadata).
        let mut initial_findings = Vec::new();
        let mut initial_requests = Vec::new();
        for (i, a) in analyzers.iter().enumerate() {
            let mut requests = Vec::new();
            let mut cx = Context {
                found: &mut initial_findings,
                requests: &mut requests,
                source_len: len,
                sector_size,
            };
            a.start(&mut cx);
            initial_requests.extend(requests.into_iter().map(|r| {
                let pending = PendingRequest {
                    analyzer: i,
                    origin: None,
                    range: r.range,
                    tag: r.tag,
                };
                (pending, r.priority)
            }));
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let (job_tx, job_rx) = bounded(self.workers * 2);
        let (req_tx, req_rx) = unbounded();
        let (ctrl_tx, ctrl_rx) = unbounded();
        let (fg_tx, fg_rx) = unbounded();
        let (coord_tx, coord_rx) = unbounded();
        let (event_tx, event_rx) = bounded(256);

        let mut threads = Vec::new();
        let spawned =
            (|| -> io::Result<JoinHandle<Result<Summary, ScanError>>> {
                for i in 0..self.workers {
                    let worker = Worker {
                        analyzers: analyzers.clone(),
                        jobs: job_rx.clone(),
                        reader: req_tx.clone(),
                        coord: coord_tx.clone(),
                        cancel: cancel.clone(),
                        source_len: len,
                        sector_size,
                    };
                    threads.push(
                        thread::Builder::new()
                            .name(format!("scan-worker-{i}"))
                            .spawn(move || worker.run())?,
                    );
                }
                let reader = Reader::new(
                    source.clone(),
                    ReaderConfig {
                        block_size: self.block_size as u64,
                        lookahead,
                        retry_passes: self.retry_passes,
                        retry_chunk: self.retry_chunk,
                        max_errors_per_block: self.max_errors_per_block.max(1),
                    },
                    Channels {
                        jobs: job_tx,
                        requests: req_rx,
                        control: ctrl_rx,
                        foreground: fg_rx,
                        coord: coord_tx.clone(),
                    },
                    cancel.clone(),
                    &remaining,
                    planned.clone(),
                    bad.clone(),
                    retry_passes_done,
                    initial_requests,
                );
                threads.push(thread::Builder::new().name("scan-reader".into()).spawn(
                    move || {
                        reader.run();
                    },
                )?);
                let coordinator = Coordinator::new(
                    CoordConfig {
                        progress_interval: self.progress_interval,
                        checkpoint_interval: self.checkpoint_interval,
                        source_len: len,
                        sector_size,
                        retry_passes: self.retry_passes,
                    },
                    coord_rx,
                    event_tx,
                    ctrl_tx.clone(),
                    cancel.clone(),
                    store,
                    self.clock,
                    analyzers.clone(),
                    CoordInit {
                        planned,
                        done,
                        bad,
                        retry_passes: retry_passes_done,
                        known_files: self.known_files,
                        initial_findings,
                        map_cells: self.map_cells,
                    },
                );
                drop(coord_tx);
                drop(req_tx);
                thread::Builder::new()
                    .name("scan-coordinator".into())
                    .spawn(move || coordinator.run())
            })();

        let coordinator = match spawned {
            Ok(c) => c,
            Err(e) => {
                // Stop whatever did start.
                cancel.store(true, Ordering::Relaxed);
                let _ = ctrl_tx.send(Command::Cancel);
                for t in threads {
                    let _ = t.join();
                }
                return Err(ScanError::Spawn(e));
            }
        };

        Ok(ScanHandle {
            source,
            control: ctrl_tx,
            foreground: fg_tx,
            events: event_rx,
            cancel,
            threads,
            coordinator: Some(coordinator),
        })
    }
}

/// Controls a running scan. Dropping it cancels the scan and waits for it to stop.
pub struct ScanHandle {
    source: Arc<dyn BlockSource>,
    control: Sender<Command>,
    foreground: Sender<FgMsg>,
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    coordinator: Option<JoinHandle<Result<Summary, ScanError>>>,
}

impl fmt::Debug for ScanHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScanHandle")
            .field("finished", &self.is_finished())
            .finish_non_exhaustive()
    }
}

impl ScanHandle {
    /// Stops reading (the current read finishes). Progress is saved.
    pub fn pause(&self) {
        let _ = self.control.send(Command::Pause);
    }

    pub fn resume(&self) {
        let _ = self.control.send(Command::Resume);
    }

    /// Stops the scan. What was done so far is saved, so it can be resumed later.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.control.send(Command::Cancel);
    }

    /// Progress and other events. Progress events are dropped while the receiver is full,
    /// so a slow consumer only sees fewer, newer snapshots.
    pub fn events(&self) -> Receiver<Event> {
        self.events.clone()
    }

    /// Opens a high-priority reader for restoring or previewing files during the scan.
    ///
    /// While any [`Foreground`] exists the scan reads nothing (on an HDD, interleaving the
    /// two would make both slow), and it continues from the same position once all are
    /// dropped. Keep it for a whole restore, not per read.
    pub fn foreground(&self) -> Foreground {
        let _ = self.foreground.send(FgMsg::Begin);
        Foreground {
            tx: self.foreground.clone(),
            source: self.source.clone(),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.coordinator.as_ref().is_none_or(|c| c.is_finished())
    }

    /// Waits for the scan to end.
    pub fn wait(mut self) -> Result<Summary, ScanError> {
        self.join()
    }

    fn join(&mut self) -> Result<Summary, ScanError> {
        let mut panicked = false;
        for t in self.threads.drain(..) {
            panicked |= t.join().is_err();
        }
        let result = match self.coordinator.take() {
            Some(c) => c.join().unwrap_or(Err(ScanError::ThreadPanicked)),
            None => Err(ScanError::ThreadPanicked),
        };
        if panicked && result.is_ok() {
            return Err(ScanError::ThreadPanicked);
        }
        result
    }
}

impl Drop for ScanHandle {
    fn drop(&mut self) {
        if self.coordinator.is_some() {
            self.cancel();
            let _ = self.join();
        }
    }
}

/// Reads routed ahead of the scan (see [`ScanHandle::foreground`]). Implements
/// [`BlockSource`], so restore code can use it like any source. After the scan ends it
/// reads the source directly.
pub struct Foreground {
    tx: Sender<FgMsg>,
    source: Arc<dyn BlockSource>,
}

impl fmt::Debug for Foreground {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Foreground").finish_non_exhaustive()
    }
}

impl BlockSource for Foreground {
    fn len(&self) -> u64 {
        self.source.len()
    }

    fn sector_size(&self) -> u32 {
        self.source.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        let (reply_tx, reply_rx) = bounded(1);
        let msg = FgMsg::Read {
            offset,
            len: buf.len(),
            reply: reply_tx,
        };
        if self.tx.send(msg).is_ok()
            && let Ok(result) = reply_rx.recv()
        {
            let data = result?;
            let n = data.len().min(buf.len());
            if let (Some(dst), Some(src)) = (buf.get_mut(..n), data.get(..n)) {
                dst.copy_from_slice(src);
            }
            return Ok(n);
        }
        // The scan has ended; nothing to coordinate with any more.
        self.source.read_at(offset, buf)
    }
}

impl Drop for Foreground {
    fn drop(&mut self) {
        let _ = self.tx.send(FgMsg::End);
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ScanError {
    InvalidConfig(&'static str),
    /// The saved scan was made on a different source.
    SourceMismatch {
        saved: SourceInfo,
        actual_len: u64,
        actual_sector_size: u32,
    },
    /// Saving results failed; the scan was stopped.
    Store(StoreError),
    Spawn(io::Error),
    ThreadPanicked,
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::InvalidConfig(why) => write!(f, "invalid scan configuration: {why}"),
            ScanError::SourceMismatch {
                saved,
                actual_len,
                actual_sector_size,
            } => write!(
                f,
                "saved scan is for a different source ({} bytes, {}-byte sectors; this one \
                 has {actual_len} bytes, {actual_sector_size}-byte sectors)",
                saved.len, saved.sector_size
            ),
            ScanError::Store(e) => write!(f, "failed to save scan results: {e}"),
            ScanError::Spawn(e) => write!(f, "failed to start scan threads: {e}"),
            ScanError::ThreadPanicked => f.write_str("a scan thread panicked"),
        }
    }
}

impl Error for ScanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            ScanError::Store(e) => Some(e.as_ref()),
            ScanError::Spawn(e) => Some(e),
            _ => None,
        }
    }
}
