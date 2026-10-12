//! The reader thread: the one place that decides what the disk reads next.
//!
//! On an HDD every jump costs a seek (~8 ms), so the order of reads matters more than
//! anything else. All reads go through this thread, in this order of priority:
//!
//! 1. Foreground reads (restore / preview, see [`crate::Foreground`]). While one is open
//!    the scan does not read at all, then continues where it stopped.
//! 2. Blocking analyzer reads ([`crate::Context::source`]): served from recently read
//!    blocks, by moving the main pass forward when the data is just ahead, or directly.
//! 3. [`Priority::Immediate`] analyzer requests.
//! 4. Analyzer `start` (metadata): the main pass waits for it.
//! 5. The main pass: the planned ranges front to back, one block at a time.
//! 6. [`Priority::Deferred`] analyzer requests, sorted by offset (one sweep).
//! 7. Retrying bad ranges (ddrescue style: skip them in the main pass, retry at the end).
//!
//! Reference: GNU ddrescue manual, "Algorithm" (copying, trimming, scraping and retrying
//! phases); the approach is reimplemented here from the description, not from its code.

use std::{
    cell::Cell,
    collections::{BTreeMap, VecDeque},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use blocksource::{BlockSource, ReadError, ReadErrorKind};
use crossbeam_channel::{Receiver, Select, Sender};

use crate::{
    analyzer::{Block, Priority, ReadReply},
    msg::{Command, CoordMsg, FgMsg, Job, SyncRead, ToReader},
    progress::{Outcome, Phase, RunState},
    ranges::{RangeMap, RangeSet},
};

/// Largest read an analyzer may request at once; longer requests are truncated.
pub const MAX_REQUEST_LEN: u64 = 64 << 20;

#[derive(Debug, Clone)]
pub(crate) struct ReaderConfig {
    pub(crate) block_size: u64,
    pub(crate) lookahead: usize,
    pub(crate) retry_passes: u32,
    pub(crate) retry_chunk: u64,
    /// After this many errors inside one block, the rest of the block is skipped (marked
    /// bad and left to the retry passes) instead of probing it further.
    pub(crate) max_errors_per_block: u32,
    /// Main-pass data kept in memory after it is read, for blocking analyzer reads.
    pub(crate) cache_bytes: u64,
    /// How far ahead of the main pass a blocking read may pull it.
    pub(crate) read_ahead: u64,
}

#[derive(Debug)]
pub(crate) struct PendingRequest {
    pub(crate) analyzer: usize,
    pub(crate) origin: Option<Range<u64>>,
    pub(crate) range: Range<u64>,
    pub(crate) tag: u64,
}

pub(crate) struct Channels {
    pub(crate) jobs: Sender<Job>,
    pub(crate) requests: Receiver<ToReader>,
    pub(crate) control: Receiver<Command>,
    pub(crate) foreground: Receiver<FgMsg>,
    pub(crate) sync: Receiver<SyncRead>,
    pub(crate) coord: Sender<CoordMsg>,
}

/// A message taken from one of the reader's channels.
enum Got {
    Command(Command),
    Foreground(FgMsg),
    Worker(ToReader),
    Sync(SyncRead),
    Nothing,
}

/// Why the loop stopped early.
#[derive(Debug)]
struct Stop;

pub(crate) struct Reader {
    source: Arc<dyn BlockSource>,
    cfg: ReaderConfig,
    sector: u64,
    ch: Channels,
    cancel: Arc<AtomicBool>,

    /// Remaining ranges of the main pass, in order.
    plan: VecDeque<Range<u64>>,
    /// Everything planned (retries are limited to it).
    planned: RangeSet,
    /// Unreadable ranges and how many times each has failed.
    bad: RangeMap<u32>,
    /// A block read but not yet sent, waiting for the next block to fill its lookahead.
    pending: Option<Block>,
    /// Blocks ready to be sent to the workers, in disk order.
    ready: VecDeque<Arc<Block>>,
    ready_bytes: u64,
    /// Recently read main-pass blocks, oldest first.
    cache: VecDeque<Arc<Block>>,
    cached_bytes: u64,
    /// Analyzers whose `start` has not been sent yet / has not finished yet.
    start_queue: VecDeque<usize>,
    starting: usize,

    immediate: VecDeque<PendingRequest>,
    /// Keyed by (offset, arrival) so they are served in disk order.
    deferred: BTreeMap<(u64, u64), PendingRequest>,
    arrivals: u64,
    /// Jobs sent to workers and not finished yet.
    outstanding: usize,

    retry_passes_done: u32,
    retry_queue: VecDeque<Range<u64>>,
    in_retry_pass: bool,

    paused: bool,
    leases: usize,
    state: RunState,
    phase: Option<Phase>,
    main_pass_started: bool,
    /// Whether the source has shown that its errors name just the failing sectors (rather
    /// than the whole request). Learned from the first error that is narrower than the
    /// read. Decides whether a retry that fails as a whole needs narrowing down.
    precise_errors: Cell<bool>,
}

impl Reader {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source: Arc<dyn BlockSource>,
        cfg: ReaderConfig,
        ch: Channels,
        cancel: Arc<AtomicBool>,
        plan: &RangeSet,
        planned: RangeSet,
        bad: RangeMap<u32>,
        retry_passes_done: u32,
        analyzers: usize,
    ) -> Self {
        let sector = u64::from(source.sector_size()).max(1);
        Self {
            source,
            cfg,
            sector,
            ch,
            cancel,
            plan: plan.ranges().collect(),
            planned,
            bad,
            pending: None,
            ready: VecDeque::new(),
            ready_bytes: 0,
            cache: VecDeque::new(),
            cached_bytes: 0,
            start_queue: (0..analyzers).collect(),
            starting: 0,
            immediate: VecDeque::new(),
            deferred: BTreeMap::new(),
            arrivals: 0,
            outstanding: 0,
            retry_passes_done,
            retry_queue: VecDeque::new(),
            in_retry_pass: false,
            paused: false,
            leases: 0,
            state: RunState::Running,
            phase: None,
            main_pass_started: false,
            precise_errors: Cell::new(false),
        }
    }

    pub(crate) fn run(mut self) -> Outcome {
        let outcome = match self.main_loop() {
            Ok(()) => {
                self.set_phase(Phase::Done);
                Outcome::Completed
            }
            Err(Stop) => Outcome::Cancelled,
        };
        let _ = self.ch.coord.send(CoordMsg::ReaderDone(outcome));
        outcome
    }

    fn main_loop(&mut self) -> Result<(), Stop> {
        loop {
            self.poll_control()?;
            if self.paused || self.leases > 0 {
                self.wait(false)?;
                continue;
            }
            self.drain_requests();
            while let Ok(req) = self.ch.sync.try_recv() {
                self.serve_sync(req);
            }

            if let Some(i) = self.start_queue.pop_front() {
                self.set_phase(Phase::Metadata);
                self.send_job(Job::Start(i))?;
                self.starting += 1;
            } else if let Some(block) = self.ready.pop_front() {
                self.ready_bytes = self.ready_bytes.saturating_sub(block.len as u64);
                self.send_job(Job::Block(block))?;
            } else if let Some(req) = self.immediate.pop_front() {
                if !self.main_pass_started {
                    self.set_phase(Phase::Metadata);
                }
                self.serve_request(req)?;
            } else if self.starting > 0 {
                // Metadata first: the main pass waits for the analyzers' `start`.
                self.set_phase(Phase::Metadata);
                self.wait(true)?;
            } else if !self.plan.is_empty() {
                self.set_phase(Phase::Scanning);
                self.scan_step();
            } else if let Some(block) = self.pending.take() {
                self.finalize(block);
            } else if let Some(req) = self.deferred.pop_first().map(|(_, r)| r) {
                self.set_phase(Phase::Finishing);
                self.serve_request(req)?;
            } else if self.outstanding > 0 {
                self.set_phase(Phase::Finishing);
                self.wait(true)?;
            } else if !self.retry_step()? {
                return Ok(());
            }
        }
    }

    // ---- Control -----------------------------------------------------------------

    /// Handles queued commands and foreground messages without blocking.
    fn poll_control(&mut self) -> Result<(), Stop> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Stop);
        }
        while let Ok(cmd) = self.ch.control.try_recv() {
            self.on_command(cmd)?;
        }
        while let Ok(msg) = self.ch.foreground.try_recv() {
            self.on_foreground(msg);
        }
        self.update_state();
        Ok(())
    }

    /// Blocks until a command or foreground message arrives (and, if `requests`, a
    /// message from a worker), then handles it.
    fn wait(&mut self, requests: bool) -> Result<(), Stop> {
        let got = {
            let mut sel = Select::new();
            let control = sel.recv(&self.ch.control);
            let foreground = sel.recv(&self.ch.foreground);
            let from_workers = requests.then(|| sel.recv(&self.ch.requests));
            let sync = requests.then(|| sel.recv(&self.ch.sync));
            let op = sel.select();
            let i = op.index();
            if i == control {
                // Disconnected means the handle is gone, which cancels the scan.
                Got::Command(op.recv(&self.ch.control).unwrap_or(Command::Cancel))
            } else if i == foreground {
                op.recv(&self.ch.foreground)
                    .map_or(Got::Nothing, Got::Foreground)
            } else if Some(i) == from_workers {
                // Disconnected: all workers are gone, so nothing outstanding will finish.
                op.recv(&self.ch.requests)
                    .map_or(Got::Command(Command::Cancel), Got::Worker)
            } else if Some(i) == sync {
                op.recv(&self.ch.sync).map_or(Got::Nothing, Got::Sync)
            } else {
                Got::Nothing
            }
        };
        self.on_got(got)?;
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Stop);
        }
        Ok(())
    }

    fn on_got(&mut self, got: Got) -> Result<(), Stop> {
        match got {
            Got::Command(cmd) => self.on_command(cmd)?,
            Got::Foreground(msg) => self.on_foreground(msg),
            Got::Worker(msg) => self.on_worker(msg),
            Got::Sync(req) => self.serve_sync(req),
            Got::Nothing => {}
        }
        self.update_state();
        Ok(())
    }

    fn on_command(&mut self, cmd: Command) -> Result<(), Stop> {
        match cmd {
            Command::Pause => self.paused = true,
            Command::Resume => self.paused = false,
            Command::Cancel => {
                self.cancel.store(true, Ordering::Relaxed);
                return Err(Stop);
            }
        }
        Ok(())
    }

    fn on_foreground(&mut self, msg: FgMsg) {
        match msg {
            FgMsg::Begin => self.leases += 1,
            FgMsg::End => self.leases = self.leases.saturating_sub(1),
            FgMsg::Read { offset, len, reply } => {
                let mut buf = vec![0u8; len];
                let result = self.source.read_at(offset, &mut buf).map(|n| {
                    buf.truncate(n);
                    buf
                });
                // The requester may have given up; nothing to do then.
                let _ = reply.send(result);
            }
        }
    }

    fn update_state(&mut self) {
        let state = if self.paused {
            RunState::Paused
        } else if self.leases > 0 {
            RunState::PausedForRestore
        } else {
            RunState::Running
        };
        if state != self.state {
            self.state = state;
            let _ = self.ch.coord.send(CoordMsg::State(state));
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        if self.phase != Some(phase) {
            self.phase = Some(phase);
            let _ = self.ch.coord.send(CoordMsg::Phase(phase));
        }
    }

    // ---- Workers -----------------------------------------------------------------

    fn drain_requests(&mut self) {
        while let Ok(msg) = self.ch.requests.try_recv() {
            self.on_worker(msg);
        }
    }

    fn on_worker(&mut self, msg: ToReader) {
        match msg {
            ToReader::Request {
                analyzer,
                origin,
                req,
            } => {
                let pending = PendingRequest {
                    analyzer,
                    origin,
                    range: req.range,
                    tag: req.tag,
                };
                self.enqueue(pending, req.priority);
            }
            ToReader::JobDone { start } => {
                self.outstanding = self.outstanding.saturating_sub(1);
                if start {
                    self.starting = self.starting.saturating_sub(1);
                }
            }
        }
    }

    fn enqueue(&mut self, req: PendingRequest, priority: Priority) {
        match priority {
            Priority::Immediate => self.immediate.push_back(req),
            Priority::Deferred => {
                self.arrivals += 1;
                self.deferred.insert((req.range.start, self.arrivals), req);
            }
        }
    }

    /// Hands a job to the workers. While the queue is full, keeps serving control and
    /// foreground messages so a restore never waits for the analyzers.
    fn send_job(&mut self, job: Job) -> Result<(), Stop> {
        let mut job = job;
        loop {
            let got = {
                let mut sel = Select::new();
                let send = sel.send(&self.ch.jobs);
                let control = sel.recv(&self.ch.control);
                let foreground = sel.recv(&self.ch.foreground);
                // Workers may be blocked on a read; serve them unless the scan is paused.
                let running = !self.paused && self.leases == 0;
                let sync = running.then(|| sel.recv(&self.ch.sync));
                let op = sel.select();
                let i = op.index();
                if i == send {
                    match op.send(&self.ch.jobs, job) {
                        Ok(()) => None,
                        Err(_) => return Err(Stop),
                    }
                } else if i == control {
                    Some((
                        job,
                        Got::Command(op.recv(&self.ch.control).unwrap_or(Command::Cancel)),
                    ))
                } else if i == foreground {
                    let got = op
                        .recv(&self.ch.foreground)
                        .map_or(Got::Nothing, Got::Foreground);
                    Some((job, got))
                } else if Some(i) == sync {
                    let got = op.recv(&self.ch.sync).map_or(Got::Nothing, Got::Sync);
                    Some((job, got))
                } else {
                    Some((job, Got::Nothing))
                }
            };
            match got {
                None => {
                    self.outstanding += 1;
                    return Ok(());
                }
                Some((j, got)) => {
                    job = j;
                    self.on_got(got)?;
                }
            }
        }
    }

    // ---- Reads -------------------------------------------------------------------

    /// One step of the main pass: read the next block. It is queued in `ready` (not sent
    /// here) so this can also run while serving a blocking read.
    fn scan_step(&mut self) {
        let Some(front) = self.plan.front_mut() else {
            return;
        };
        self.main_pass_started = true;
        // Blocks end on multiples of the block size, so they stay aligned after an
        // unaligned range start.
        let block_end = (front.start / self.cfg.block_size + 1)
            .saturating_mul(self.cfg.block_size)
            .min(front.end);
        let range = front.start..block_end;
        front.start = block_end;
        if front.is_empty() {
            self.plan.pop_front();
        }

        let (mut buf, holes) = self.read_skipping(
            range.clone(),
            self.cfg.lookahead,
            self.cfg.max_errors_per_block,
        );
        let _ = self.ch.coord.send(CoordMsg::Read(range.clone()));
        for h in &holes {
            self.bad.insert(h.clone(), 1);
            let _ = self.ch.coord.send(CoordMsg::Bad {
                range: h.clone(),
                attempts: 1,
            });
        }
        let len = usize::try_from(range.end - range.start).unwrap_or(0);
        buf.resize(len + self.cfg.lookahead, 0);
        let block = Block {
            offset: range.start,
            buf,
            len,
            lookahead: 0,
            holes,
            retry: false,
        };

        if self.cfg.lookahead == 0 {
            self.finalize(block);
            return;
        }
        if let Some(mut prev) = self.pending.take() {
            if prev.end() == block.offset {
                fill_lookahead(&mut prev, &block);
            }
            self.finalize(prev);
        }
        self.pending = Some(block);
    }

    /// A main-pass block is complete: keep it in the cache and queue it for the workers.
    fn finalize(&mut self, block: Block) {
        let block = Arc::new(block);
        let len = block.len as u64;
        self.cache.push_back(block.clone());
        self.cached_bytes += len;
        while self.cached_bytes > self.cfg.cache_bytes
            && let Some(old) = self.cache.pop_front()
        {
            self.cached_bytes = self.cached_bytes.saturating_sub(old.len as u64);
        }
        self.ready.push_back(block);
        self.ready_bytes += len;
    }

    // ---- Blocking analyzer reads -------------------------------------------------

    fn serve_sync(&mut self, req: SyncRead) {
        let SyncRead { offset, len, reply } = req;
        let end = offset
            .saturating_add(len as u64)
            .min(self.source.len())
            .max(offset);
        let result = self.sync_read(offset..end);
        // The worker may have given up (cancelled); nothing to do then.
        let _ = reply.send(result);
    }

    fn sync_read(&mut self, range: Range<u64>) -> Result<Vec<u8>, ReadError> {
        let mut out = vec![0u8; usize::try_from(range.end - range.start).unwrap_or(0)];
        if out.is_empty() {
            return Ok(out);
        }
        loop {
            if let Some((bad, _)) = self.bad.overlapping(range.clone()).next() {
                // Known to be unreadable: do not spend another error timeout on it now.
                return Err(ReadError::new(
                    bad,
                    ReadErrorKind::Io(std::io::Error::other("recorded as unreadable")),
                ));
            }
            if self.in_memory(range.clone(), Some(&mut out)) {
                return Ok(out);
            }
            if !self.can_pull_forward(&range) {
                break;
            }
            // Pulling forward can take many blocks; a pause (or cancel) must not wait for
            // them. Then only the range asked for is read.
            if self.poll_control().is_err() || self.paused || self.leases > 0 {
                break;
            }
            // The data is just ahead: read the main pass up to it instead of seeking.
            self.scan_step();
        }
        let n = self.source.read_at(range.start, &mut out)?;
        out.truncate(n);
        Ok(out)
    }

    /// Whether `range` is entirely in recently read blocks (copying it into `out` if
    /// given). A block's unreadable parts do not count.
    fn in_memory(&self, range: Range<u64>, mut out: Option<&mut [u8]>) -> bool {
        let mut covered = 0;
        for b in self
            .cache
            .iter()
            .map(Arc::as_ref)
            .chain(self.pending.as_ref())
        {
            let (s, e) = (range.start.max(b.offset), range.end.min(b.end()));
            if s >= e {
                continue;
            }
            if b.holes.iter().any(|h| h.start < e && s < h.end) {
                return false;
            }
            if let Some(out) = out.as_deref_mut() {
                let rel = |x: u64, base: u64| usize::try_from(x - base).unwrap_or(usize::MAX);
                let src = b.data().get(rel(s, b.offset)..rel(e, b.offset));
                let dst = out.get_mut(rel(s, range.start)..rel(e, range.start));
                match (src, dst) {
                    (Some(src), Some(dst)) => dst.copy_from_slice(src),
                    _ => return false,
                }
            }
            covered += e - s;
        }
        covered == range.end - range.start
    }

    /// Whether reading the main pass forward will bring `range` into memory.
    fn can_pull_forward(&self, range: &Range<u64>) -> bool {
        let Some(front) = self.plan.front() else {
            return false;
        };
        range.end > front.start
            && range.end <= front.end
            && range.end <= front.start.saturating_add(self.cfg.read_ahead)
            && self.ready_bytes < self.cfg.read_ahead
            && (range.start >= front.start || self.in_memory(range.start..front.start, None))
    }

    fn serve_request(&mut self, req: PendingRequest) -> Result<(), Stop> {
        let end = req
            .range
            .end
            .min(self.source.len())
            .min(req.range.start.saturating_add(MAX_REQUEST_LEN));
        let range = req.range.start..end.max(req.range.start);
        // Probe a request fully: it is small and someone is waiting for it.
        let (data, holes) = self.read_skipping(range.clone(), 0, u32::MAX);
        let reply = ReadReply {
            tag: req.tag,
            offset: range.start,
            data,
            holes,
        };
        self.send_job(Job::Reply {
            analyzer: req.analyzer,
            origin: req.origin,
            reply,
        })
    }

    /// Reads `range`, skipping unreadable parts instead of failing. Returns the data
    /// (holes zero-filled; shorter only at the end of the source) with spare capacity for
    /// `extra` more bytes, and the holes. After `max_errors` errors the rest of the range
    /// is treated as one hole.
    fn read_skipping(
        &self,
        range: Range<u64>,
        extra: usize,
        max_errors: u32,
    ) -> (Vec<u8>, Vec<Range<u64>>) {
        let start = range.start;
        let total = usize::try_from(range.end.saturating_sub(start)).unwrap_or(0);
        let mut buf = Vec::with_capacity(total + extra);
        buf.resize(total, 0);
        let end = start + total as u64;
        let rel = |x: u64| usize::try_from(x - start).unwrap_or(usize::MAX);

        let mut holes = Vec::new();
        // Holes found ahead of `limit`, nearest last.
        let mut ahead: Vec<Range<u64>> = Vec::new();
        let mut pos = start;
        let mut limit = end;
        let mut errors = 0u32;
        loop {
            if pos >= limit {
                match ahead.pop() {
                    Some(h) => {
                        pos = h.end;
                        holes.push(h);
                        limit = ahead.last().map_or(end, |h| h.start);
                        continue;
                    }
                    None => break,
                }
            }
            let Some(dst) = buf.get_mut(rel(pos)..rel(limit)) else {
                break;
            };
            match self.source.read_at(pos, dst) {
                Ok(0) => {
                    // End of the source.
                    buf.truncate(rel(pos));
                    break;
                }
                Ok(n) => pos += n as u64,
                Err(e) => {
                    errors = errors.saturating_add(1);
                    let mut h = self.error_range(e.range(), pos..limit);
                    if h.is_empty() {
                        // The error does not say where; blame the whole attempt.
                        h = pos..limit;
                    }
                    if errors >= max_errors {
                        h.end = end;
                        ahead.clear();
                    }
                    if h.start == pos {
                        pos = h.end;
                        holes.push(h);
                        limit = ahead.last().map_or(end, |h| h.start);
                    } else {
                        // Read the good part before the hole first.
                        limit = h.start;
                        ahead.push(h);
                    }
                }
            }
        }
        (buf, holes)
    }

    /// The part of `attempt` an error blames, widened to sectors. Empty if the error
    /// names nothing inside the attempt.
    fn error_range(&self, error: Range<u64>, attempt: Range<u64>) -> Range<u64> {
        let h = self.align_out(error);
        let h = h.start.max(attempt.start)..h.end.min(attempt.end);
        if !h.is_empty() && h != attempt {
            self.precise_errors.set(true);
        }
        h
    }

    fn align_out(&self, r: Range<u64>) -> Range<u64> {
        let s = self.sector;
        (r.start / s * s)..r.end.div_ceil(s).saturating_mul(s)
    }

    // ---- Retries -----------------------------------------------------------------

    /// One step of the retry phase. Returns false when there is nothing left to do.
    fn retry_step(&mut self) -> Result<bool, Stop> {
        let Some(range) = self.retry_queue.pop_front() else {
            if self.in_retry_pass {
                self.in_retry_pass = false;
                self.retry_passes_done += 1;
                let _ = self
                    .ch
                    .coord
                    .send(CoordMsg::RetryPassDone(self.retry_passes_done));
            }
            if self.retry_passes_done >= self.cfg.retry_passes {
                return Ok(false);
            }
            let queue: VecDeque<_> = self
                .planned
                .ranges()
                .flat_map(|p| self.bad.overlapping(p).map(|(r, _)| r).collect::<Vec<_>>())
                .collect();
            if queue.is_empty() {
                return Ok(false);
            }
            self.retry_queue = queue;
            self.in_retry_pass = true;
            self.set_phase(Phase::Retrying {
                pass: self.retry_passes_done + 1,
            });
            return Ok(true);
        };

        let chunk_end = range
            .start
            .saturating_add(self.cfg.retry_chunk)
            .min(range.end);
        let chunk = range.start..chunk_end;
        let _ = self.ch.coord.send(CoordMsg::RetryPosition(chunk.start));
        if chunk_end < range.end {
            self.retry_queue.push_front(chunk_end..range.end);
        }

        let len = usize::try_from(chunk.end - chunk.start).unwrap_or(0);
        let mut buf = vec![0u8; len];
        match self.source.read_exact_at(chunk.start, &mut buf) {
            Ok(()) => {
                self.bad.remove(chunk.clone());
                let _ = self.ch.coord.send(CoordMsg::Recovered(chunk.clone()));
                let block = Block {
                    offset: chunk.start,
                    buf,
                    len,
                    lookahead: 0,
                    holes: Vec::new(),
                    retry: true,
                };
                self.send_job(Job::Block(Arc::new(block)))?;
            }
            Err(e) => {
                let h = self.error_range(e.range(), chunk.clone());
                if !h.is_empty() && h != chunk {
                    // The error says exactly where: that part stays bad, retry the rest.
                    self.mark_still_bad(h.clone());
                    if h.end < chunk.end {
                        self.retry_queue.push_front(h.end..chunk.end);
                    }
                    if chunk.start < h.start {
                        self.retry_queue.push_front(chunk.start..h.start);
                    }
                } else if !self.precise_errors.get() && chunk.end - chunk.start > self.sector {
                    // The source only says "this request failed": split in half to narrow
                    // it down to the bad sectors. (A source with precise errors has
                    // already told us the whole chunk is bad.)
                    let half =
                        chunk.start + (chunk.end - chunk.start) / self.sector / 2 * self.sector;
                    self.retry_queue.push_front(half..chunk.end);
                    self.retry_queue.push_front(chunk.start..half);
                } else {
                    self.mark_still_bad(chunk);
                }
            }
        }
        Ok(true)
    }

    fn mark_still_bad(&mut self, range: Range<u64>) {
        let attempts = self
            .bad
            .overlapping(range.clone())
            .map(|(_, a)| a)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.bad.insert(range.clone(), attempts);
        let _ = self.ch.coord.send(CoordMsg::Bad { range, attempts });
    }
}

/// Copies the start of `next` into the lookahead area of `prev`, stopping at the first
/// hole of `next`.
fn fill_lookahead(prev: &mut Block, next: &Block) {
    let room = prev.buf.len().saturating_sub(prev.len);
    let readable = next
        .holes
        .first()
        .map_or(next.len as u64, |h| h.start - next.offset);
    let n = room.min(usize::try_from(readable).unwrap_or(usize::MAX));
    if let (Some(dst), Some(src)) = (prev.buf.get_mut(prev.len..prev.len + n), next.buf.get(..n)) {
        dst.copy_from_slice(src);
        prev.lookahead = n;
    }
}
