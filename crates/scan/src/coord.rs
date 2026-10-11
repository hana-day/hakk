//! The coordinator thread: tracks progress, saves checkpoints and emits events.

use std::{
    collections::HashMap,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use index::{BadRange, Media, ScanPhase, ScanProgress};

use crate::{
    Finding, ScanError,
    analyzer::Context,
    clock::Clock,
    msg::{Command, CoordMsg},
    progress::{
        Event, FoundCounts, MapTracker, Outcome, Phase, Progress, RateMeter, RetryProgress,
        RunState, Summary,
    },
    ranges::{RangeMap, RangeSet},
    source::ScanSource,
    store::{Store, StoreError},
    worker::Analyzers,
};

/// Flush buffered findings at least this often (in number of files).
const MAX_BUFFERED: usize = 1000;

pub(crate) struct CoordConfig {
    pub(crate) progress_interval: Duration,
    pub(crate) checkpoint_interval: Duration,
    pub(crate) source_len: u64,
    pub(crate) sector_size: u32,
    pub(crate) retry_passes: u32,
    /// For analyzers' `finish` (the reader is gone by then, so it reads directly).
    pub(crate) source: ScanSource,
}

/// A block whose analysis (including reads it led to) is not finished yet.
struct OpenBlock {
    /// Jobs still to finish: the block itself plus the replies to its requests.
    open_jobs: i64,
    holes: Vec<Range<u64>>,
}

pub(crate) struct Coordinator {
    cfg: CoordConfig,
    rx: Receiver<CoordMsg>,
    events: Sender<Event>,
    control: Sender<Command>,
    cancel: Arc<AtomicBool>,
    store: Box<dyn Store>,
    clock: Arc<dyn Clock>,
    analyzers: Analyzers,

    started: Duration,
    planned_bytes: u64,
    scanned_bytes: u64,
    position: u64,
    state: RunState,
    phase: Phase,
    found: FoundCounts,
    done: RangeSet,
    bad: RangeMap<u32>,
    retry_passes: u32,
    open: HashMap<(u64, u64), OpenBlock>,
    buffer: Vec<Finding>,
    map: MapTracker,
    rate: RateMeter,
    last_progress: Option<Duration>,
    last_checkpoint: Duration,
    outcome: Option<Outcome>,
    error: Option<StoreError>,
}

pub(crate) struct CoordInit {
    pub(crate) planned: RangeSet,
    pub(crate) done: RangeSet,
    pub(crate) bad: RangeMap<u32>,
    pub(crate) retry_passes: u32,
    pub(crate) known_files: Vec<(Option<u64>, Media)>,
    pub(crate) map_cells: usize,
}

impl Coordinator {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        cfg: CoordConfig,
        rx: Receiver<CoordMsg>,
        events: Sender<Event>,
        control: Sender<Command>,
        cancel: Arc<AtomicBool>,
        store: Box<dyn Store>,
        clock: Arc<dyn Clock>,
        analyzers: Analyzers,
        init: CoordInit,
    ) -> Self {
        let mut map = MapTracker::new(cfg.source_len, init.map_cells, &init.planned);
        let mut scanned_bytes = 0;
        for r in init.done.ranges() {
            scanned_bytes += init.planned.overlap_len(r.clone());
            map.mark_read(r);
        }
        for (r, _) in init.bad.iter() {
            scanned_bytes += init.planned.overlap_len(r.clone());
            map.mark_read(r.clone());
            map.refresh_errors(r, &init.bad);
        }
        let mut found = FoundCounts::default();
        for (offset, media) in init.known_files {
            if let Some(offset) = offset {
                map.mark_found(offset);
            }
            count(&mut found, media);
        }
        let now = clock.now();
        Self {
            cfg,
            rx,
            events,
            control,
            cancel,
            store,
            clock,
            analyzers,
            started: now,
            planned_bytes: init.planned.total_len(),
            scanned_bytes,
            position: init.planned.first().map_or(0, |(r, _)| r.start),
            state: RunState::Running,
            phase: Phase::Scanning,
            found,
            done: init.done,
            bad: init.bad,
            retry_passes: init.retry_passes,
            open: HashMap::new(),
            buffer: Vec::new(),
            map,
            rate: RateMeter::default(),
            last_progress: None,
            last_checkpoint: now,
            outcome: None,
            error: None,
        }
    }

    pub(crate) fn run(mut self) -> Result<Summary, ScanError> {
        loop {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(msg) => self.on_message(msg),
                Err(RecvTimeoutError::Timeout) => {}
                // Reader and workers are all gone.
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let now = self.clock.now();
            if self
                .last_progress
                .is_none_or(|t| now.saturating_sub(t) >= self.cfg.progress_interval)
            {
                self.emit_progress();
            }
            if self.buffer.len() >= MAX_BUFFERED
                || now.saturating_sub(self.last_checkpoint) >= self.cfg.checkpoint_interval
            {
                self.checkpoint();
            }
        }
        self.finish()
    }

    fn on_message(&mut self, msg: CoordMsg) {
        match msg {
            CoordMsg::State(state) => {
                self.state = state;
                if state == RunState::Running {
                    // Do not let the pause drag the speed down.
                    self.rate.reset();
                } else {
                    // Pausing is a natural moment to save; the app may be closed next.
                    self.checkpoint();
                }
                self.emit_progress();
            }
            CoordMsg::Phase(phase) => {
                self.phase = phase;
                self.emit_progress();
            }
            CoordMsg::Read(range) => {
                self.scanned_bytes += range.end - range.start;
                self.position = range.end;
                self.map.mark_read(range);
            }
            CoordMsg::Bad { range, attempts } => {
                self.bad.insert(range.clone(), attempts);
                self.map.refresh_errors(range, &self.bad);
            }
            CoordMsg::RetryPosition(pos) => self.position = pos,
            CoordMsg::Recovered(range) => {
                self.bad.remove(range.clone());
                self.map.refresh_errors(range, &self.bad);
            }
            CoordMsg::RetryPassDone(n) => self.retry_passes = n,
            CoordMsg::JobFinished {
                origin,
                block_holes,
                spawned,
                found,
            } => {
                self.add_findings(found);
                if let Some(origin) = origin {
                    self.job_finished(origin, block_holes, spawned);
                }
            }
            CoordMsg::AnalyzerPanicked { analyzer, offset } => {
                let _ = self
                    .events
                    .try_send(Event::AnalyzerPanicked { analyzer, offset });
            }
            CoordMsg::ReaderDone(outcome) => self.outcome = Some(outcome),
        }
    }

    fn job_finished(&mut self, origin: Range<u64>, holes: Option<Vec<Range<u64>>>, spawned: usize) {
        let key = (origin.start, origin.end);
        let entry = self.open.entry(key).or_insert(OpenBlock {
            open_jobs: 0,
            holes: Vec::new(),
        });
        if let Some(holes) = holes {
            entry.open_jobs += 1;
            entry.holes = holes;
        }
        entry.open_jobs += i64::try_from(spawned).unwrap_or(i64::MAX) - 1;
        if entry.open_jobs <= 0
            && let Some(block) = self.open.remove(&key)
        {
            self.done.add(origin);
            for h in block.holes {
                self.done.remove(h);
            }
        }
    }

    fn add_findings(&mut self, found: Vec<Finding>) {
        for f in &found {
            count(&mut self.found, f.kind.media());
            if let Some(offset) = f.offset() {
                self.map.mark_found(offset);
            }
        }
        self.buffer.extend(found);
    }

    fn progress(&self) -> Progress {
        let running = self.state == RunState::Running;
        let left = self.planned_bytes.saturating_sub(self.scanned_bytes);
        let main_pass = matches!(self.phase, Phase::Metadata | Phase::Scanning);
        Progress {
            state: self.state,
            phase: self.phase,
            planned_bytes: self.planned_bytes,
            scanned_bytes: self.scanned_bytes,
            position: self.position,
            bytes_per_sec: self.rate.rate().filter(|_| running && main_pass),
            remaining: if running && main_pass {
                self.rate.remaining(left)
            } else {
                None
            },
            elapsed: self.clock.now().saturating_sub(self.started),
            found: self.found,
            bad_bytes: self.bad.total_len(),
            retry: match self.phase {
                Phase::Retrying { pass } => Some(RetryProgress {
                    pass,
                    passes: self.cfg.retry_passes,
                    bytes_left: self.bad.total_len(),
                }),
                _ => None,
            },
            map: self.map.snapshot(),
        }
    }

    fn emit_progress(&mut self) {
        let now = self.clock.now();
        if self.state == RunState::Running && self.phase == Phase::Scanning {
            self.rate.sample(now, self.scanned_bytes);
        }
        self.last_progress = Some(now);
        // Progress is a snapshot; if nobody is listening, dropping it is fine.
        let _ = self
            .events
            .try_send(Event::Progress(Box::new(self.progress())));
    }

    fn scan_progress(&self) -> ScanProgress {
        ScanProgress {
            done: self.done.to_vec(),
            bad: self
                .bad
                .iter()
                .map(|(range, attempts)| BadRange { range, attempts })
                .collect(),
            phase: match (self.outcome, self.phase) {
                (Some(Outcome::Completed), _) | (_, Phase::Done) => ScanPhase::Complete,
                (_, Phase::Retrying { .. }) => ScanPhase::Retrying,
                _ => ScanPhase::Scanning,
            },
            retry_passes: self.retry_passes,
        }
    }

    fn checkpoint(&mut self) {
        self.last_checkpoint = self.clock.now();
        if self.error.is_some() {
            return;
        }
        let progress = self.scan_progress();
        match self.store.commit(&self.buffer, &progress) {
            Ok(()) => self.buffer.clear(),
            Err(e) => {
                // Results that cannot be saved would be lost: stop the scan.
                self.error = Some(e);
                self.cancel.store(true, Ordering::Relaxed);
                let _ = self.control.send(Command::Cancel);
            }
        }
    }

    fn finish(mut self) -> Result<Summary, ScanError> {
        let outcome = match (self.outcome, &self.error) {
            (Some(Outcome::Completed), None) => Outcome::Completed,
            _ => Outcome::Cancelled,
        };
        if outcome == Outcome::Completed {
            let mut found = Vec::new();
            let mut ignored = Vec::new();
            for a in self.analyzers.iter() {
                let mut cx = Context {
                    source: &self.cfg.source,
                    found: &mut found,
                    requests: &mut ignored,
                    source_len: self.cfg.source_len,
                    sector_size: self.cfg.sector_size,
                };
                let ok =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.finish(&mut cx)));
                if ok.is_err() {
                    let _ = self.events.try_send(Event::AnalyzerPanicked {
                        analyzer: a.name().to_owned(),
                        offset: None,
                    });
                }
            }
            self.add_findings(found);
            self.phase = Phase::Done;
        }
        self.checkpoint();
        if let Some(e) = self.error.take() {
            return Err(ScanError::Store(e));
        }
        self.emit_progress();
        let summary = Summary {
            outcome,
            planned_bytes: self.planned_bytes,
            scanned_bytes: self.scanned_bytes,
            bad_bytes: self.bad.total_len(),
            retry_passes: self.retry_passes,
            found: self.found,
            elapsed: self.clock.now().saturating_sub(self.started),
        };
        let _ = self.events.try_send(Event::Finished(summary.clone()));
        Ok(summary)
    }
}

fn count(found: &mut FoundCounts, media: Media) {
    match media {
        Media::Photo => found.photos += 1,
        Media::Video => found.videos += 1,
    }
}
