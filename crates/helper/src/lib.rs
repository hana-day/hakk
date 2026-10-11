//! The helper: serves the GUI's requests ([`hakk_protocol::Helper`]) with the real scan
//! pipeline.
//!
//! - A scan runs [`scan::Scan`] over a [`BlockSource`] with the given analyzers and
//!   stores what they find in an [`index::Index`] (SQLite) in a work folder.
//! - A pump thread per scan turns the scan's progress into GUI events, and reads new and
//!   changed records from the index (a second connection; the index runs in WAL mode) to
//!   report found files, verification updates and merged duplicates.
//! - A recovery reads through the scan's foreground lease while the scan runs, so the scan
//!   pauses and resumes at the same position (see [`scan::ScanHandle::foreground`]).
//!
//! For now it runs inside the GUI process and scans disk images only. Running it as a
//! separate, elevated process and listing physical drives come next.

mod convert;
mod places;
mod recovery;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use blocksource::{BlockSource, ImageFile};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use hakk_protocol::{
    Destination, Drive, DriveId, DriveKind, EventSink, FileKind, FileMerge, FileStatus, FileUpdate,
    FilesFound, FilesMerged, FilesUpdated, FoundFile, Helper, HelperError, HelperEvent, RecoveryId,
    RecoveryRequest, RepairRequest, ScanId, ScanSummary,
};
use index::{Index, Query};
use scan::{Analyzer, Event, Outcome, Scan, ScanHandle};

/// Builds the analyzers for each new scan (they keep per-scan state).
pub type AnalyzerFactory = Arc<dyn Fn() -> Vec<Box<dyn Analyzer>> + Send + Sync>;

/// Lists destinations; the argument is the scanned device when it is a physical disk.
type PlaceLister = Box<dyn Fn(Option<&Path>) -> Vec<Destination> + Send + Sync>;

/// How often the index is read for new and merged files.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// How often every shown record is re-read for status changes; this grows with the number
/// of files, so it runs less often than the poll for new ones.
const STATUS_INTERVAL: Duration = Duration::from_secs(2);

pub struct RealHelper {
    shared: Arc<Shared>,
}

struct Shared {
    sink: EventSink,
    analyzers: AnalyzerFactory,
    places: PlaceLister,
    work_dir: PathBuf,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    images: Vec<Image>,
    next_scan: u64,
    next_recovery: u64,
    scan: Option<ActiveScan>,
    recovering: bool,
}

struct Image {
    drive: Drive,
    path: PathBuf,
}

struct ActiveScan {
    id: ScanId,
    source: Arc<dyn BlockSource>,
    handle: Arc<ScanHandle>,
    index_path: PathBuf,
    /// Set once the scan has finished or been cancelled.
    ended: Arc<AtomicBool>,
}

impl RealHelper {
    /// A helper that scans with the file system and carving analyzers.
    pub fn new(sink: EventSink) -> Self {
        Self::with_analyzers(sink, Arc::new(scan::analyzers::default_set))
    }

    /// `analyzers` builds the analyzers for each scan. Indexes go into a `hakk` folder in
    /// the system's temporary directory.
    pub fn with_analyzers(sink: EventSink, analyzers: AnalyzerFactory) -> Self {
        Self::with_options(
            sink,
            analyzers,
            std::env::temp_dir().join("hakk"),
            Box::new(places::destinations),
        )
    }

    fn with_options(
        sink: EventSink,
        analyzers: AnalyzerFactory,
        work_dir: PathBuf,
        places: PlaceLister,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                sink,
                analyzers,
                places,
                work_dir,
                state: Mutex::new(State::default()),
            }),
        }
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves the state usable; ignore poisoning.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, event: HelperEvent) {
        (self.sink)(event);
    }
}

impl State {
    fn image(&self, id: &DriveId) -> Result<&Image, HelperError> {
        self.images
            .iter()
            .find(|i| i.drive.id == *id)
            .ok_or(HelperError::DriveNotFound)
    }

    fn scan(&self, id: ScanId) -> Result<&ActiveScan, HelperError> {
        match &self.scan {
            Some(s) if s.id == id => Ok(s),
            _ => Err(HelperError::ScanNotFound),
        }
    }

    fn running_scan(&self, id: ScanId) -> Result<&ActiveScan, HelperError> {
        let scan = self.scan(id)?;
        if scan.ended.load(Ordering::Acquire) {
            return Err(HelperError::ScanEnded);
        }
        Ok(scan)
    }
}

fn internal(err: impl std::fmt::Display) -> HelperError {
    HelperError::Internal {
        detail: err.to_string(),
    }
}

impl Helper for RealHelper {
    fn list_drives(&self) -> Result<Vec<Drive>, HelperError> {
        Ok(self
            .shared
            .state()
            .images
            .iter()
            .map(|i| i.drive.clone())
            .collect())
    }

    fn open_image(&self, path: &str) -> Result<Drive, HelperError> {
        let path = PathBuf::from(path);
        let image = ImageFile::open(&path).map_err(|e| HelperError::CannotOpen {
            detail: e.to_string(),
        })?;
        let mut state = self.shared.state();
        if let Some(existing) = state.images.iter().find(|i| i.path == path) {
            return Ok(existing.drive.clone());
        }
        let drive = Drive {
            id: DriveId(format!("image-{}", state.images.len())),
            kind: DriveKind::DiskImage,
            name: path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            capacity_bytes: image.len(),
            file_system: None,
            warnings: Vec::new(),
        };
        state.images.push(Image {
            drive: drive.clone(),
            path,
        });
        Ok(drive)
    }

    fn list_destinations(&self, source: &DriveId) -> Result<Vec<Destination>, HelperError> {
        // Images are regular files: writing next to them cannot overwrite their content.
        self.shared.state().image(source)?;
        Ok((self.shared.places)(None))
    }

    fn start_scan(&self, drive: &DriveId) -> Result<ScanId, HelperError> {
        let mut state = self.shared.state();
        let image = state.image(drive)?;
        let source: Arc<dyn BlockSource> =
            Arc::new(
                ImageFile::open(&image.path).map_err(|e| HelperError::CannotOpen {
                    detail: e.to_string(),
                })?,
            );
        let label = image.drive.name.clone();
        if let Some(previous) = state.scan.take() {
            previous.handle.cancel();
        }
        state.next_scan += 1;
        let id = ScanId(state.next_scan);

        std::fs::create_dir_all(&self.shared.work_dir).map_err(internal)?;
        let index_path =
            self.shared
                .work_dir
                .join(format!("scan-{}-{}.sqlite", std::process::id(), id.0));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", index_path.display()));
        }
        let index = Index::open(&index_path).map_err(internal)?;
        let handle = Arc::new(
            Scan::new(source.clone())
                .label(label)
                .analyzers((self.shared.analyzers)())
                .start(index)
                .map_err(internal)?,
        );
        let ended = Arc::new(AtomicBool::new(false));

        let pump = Pump {
            shared: self.shared.clone(),
            scan_id: id,
            events: handle.events(),
            poller: Poller::new(index_path.clone()),
            ended: ended.clone(),
            read_errors: 0,
        };
        thread::Builder::new()
            .name(format!("helper-pump-{}", id.0))
            .spawn(move || pump.run())
            .map_err(internal)?;

        state.scan = Some(ActiveScan {
            id,
            source,
            handle,
            index_path,
            ended,
        });
        Ok(id)
    }

    fn pause_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.shared.state().running_scan(scan)?.handle.pause();
        Ok(())
    }

    fn resume_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.shared.state().running_scan(scan)?.handle.resume();
        Ok(())
    }

    fn cancel_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.shared.state().running_scan(scan)?.handle.cancel();
        Ok(())
    }

    fn recover(&self, request: RecoveryRequest) -> Result<RecoveryId, HelperError> {
        if request.files.is_empty() {
            return Err(HelperError::NothingRequested);
        }
        let mut state = self.shared.state();
        if state.recovering {
            return Err(HelperError::RecoveryInProgress);
        }
        let destination = (self.shared.places)(None)
            .into_iter()
            .find(|d| d.id == request.destination)
            .ok_or(HelperError::DestinationNotFound)?;
        if let Some(reason) = destination.blocked {
            return Err(HelperError::DestinationBlocked { reason });
        }
        let scan = state.scan(request.scan_id)?;
        // While the scan runs, read through a foreground lease so the scan pauses instead
        // of fighting over the disk head; afterwards read the source directly.
        let source: Box<dyn BlockSource> = if scan.ended.load(Ordering::Acquire) {
            Box::new(scan.source.clone())
        } else {
            Box::new(scan.handle.foreground())
        };
        let index_path = scan.index_path.clone();
        state.next_recovery += 1;
        let job = recovery::Job {
            id: RecoveryId(state.next_recovery),
            files: request.files,
            folder: PathBuf::from(destination.path),
            index_path,
        };
        let id = job.id;
        state.recovering = true;
        drop(state);

        let shared = self.shared.clone();
        let spawned = thread::Builder::new()
            .name(format!("helper-recovery-{}", id.0))
            .spawn(move || {
                let report = recovery::run(&job, source.as_ref(), &|e| shared.emit(e));
                // Release the lease before reporting, so the scan resumes first.
                drop(source);
                shared.state().recovering = false;
                shared.emit(HelperEvent::RecoveryFinished(report));
            });
        if let Err(err) = spawned {
            self.shared.state().recovering = false;
            return Err(internal(err));
        }
        Ok(id)
    }

    fn repair(&self, _request: RepairRequest) -> Result<(), HelperError> {
        // Video repair needs the `video` crate, which does not exist yet.
        Err(HelperError::NotAvailable)
    }
}

/// Forwards one scan's events to the GUI.
struct Pump {
    shared: Arc<Shared>,
    scan_id: ScanId,
    events: Receiver<Event>,
    poller: Poller,
    ended: Arc<AtomicBool>,
    read_errors: u32,
}

impl Pump {
    fn run(mut self) {
        let mut next_poll = Instant::now();
        loop {
            match self.events.recv_timeout(POLL_INTERVAL / 2) {
                Ok(Event::Progress(p)) => {
                    let progress = convert::progress(self.scan_id, &p);
                    self.read_errors = progress.read_errors;
                    self.shared.emit(HelperEvent::ScanProgress(progress));
                }
                Ok(Event::AnalyzerPanicked { analyzer, offset }) => {
                    eprintln!("analyzer {analyzer} panicked at {offset:?}; scanning continues");
                }
                Ok(Event::Finished(summary)) => {
                    self.poll(true);
                    self.ended.store(true, Ordering::Release);
                    let report = self.poller.summary(
                        self.scan_id,
                        summary.outcome == Outcome::Cancelled,
                        summary.elapsed.as_secs(),
                        self.read_errors,
                    );
                    self.shared.emit(HelperEvent::ScanFinished(report));
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // The scan stopped without a summary (e.g. it was replaced).
                    self.ended.store(true, Ordering::Release);
                    return;
                }
            }
            if Instant::now() >= next_poll {
                self.poll(false);
                next_poll = Instant::now() + POLL_INTERVAL;
            }
        }
    }

    fn poll(&mut self, full: bool) {
        let scan_id = self.scan_id;
        match self.poller.poll(full) {
            Ok(changes) => {
                if !changes.found.is_empty() {
                    self.shared.emit(HelperEvent::FilesFound(FilesFound {
                        scan_id,
                        files: changes.found,
                    }));
                }
                if !changes.updates.is_empty() {
                    self.shared.emit(HelperEvent::FilesUpdated(FilesUpdated {
                        scan_id,
                        updates: changes.updates,
                    }));
                }
                if !changes.merges.is_empty() {
                    self.shared.emit(HelperEvent::FilesMerged(FilesMerged {
                        scan_id,
                        merges: changes.merges,
                    }));
                }
            }
            Err(err) => eprintln!("cannot read the scan index: {err}"),
        }
    }
}

/// What changed in the index since the last poll.
#[derive(Default)]
struct Changes {
    found: Vec<FoundFile>,
    updates: Vec<FileUpdate>,
    merges: Vec<FileMerge>,
}

/// Reads the index for files that are new, changed or merged since the last poll.
struct Poller {
    path: PathBuf,
    index: Option<Index>,
    /// Highest record id seen so far; newer records are new files.
    last_id: i64,
    next_status_check: Instant,
    /// Every record seen so far, as last reported.
    known: HashMap<i64, Known>,
    folders: HashMap<i64, Vec<String>>,
}

struct Known {
    file: FoundFile,
    merged: bool,
}

impl Poller {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            index: None,
            last_id: 0,
            next_status_check: Instant::now(),
            known: HashMap::new(),
            folders: HashMap::new(),
        }
    }

    /// `full` re-reads every record for status changes regardless of the interval.
    fn poll(&mut self, full: bool) -> Result<Changes, index::Error> {
        if self.index.is_none() {
            self.index = Some(Index::open(&self.path)?);
        }
        let Some(index) = self.index.as_mut() else {
            return Ok(Changes::default());
        };
        let mut changes = Changes::default();

        // New records, including ones already merged (remembered, not shown).
        let new = index.files(&Query {
            after_id: Some(self.last_id),
            include_merged: true,
            ..Query::default()
        })?;
        for record in new {
            self.last_id = self.last_id.max(record.id);
            let folders = match record.folder_id {
                Some(id) => match self.folders.get(&id) {
                    Some(path) => path.clone(),
                    None => {
                        let path = index.folder_path(id)?;
                        self.folders.insert(id, path.clone());
                        path
                    }
                },
                None => Vec::new(),
            };
            let file = convert::found_file(&record, folders);
            let merged = record.merged_into.is_some();
            if !merged {
                changes.found.push(file.clone());
            }
            self.known.insert(record.id, Known { file, merged });
        }

        // Fold carving results into the named records they duplicate, so the merge shows
        // up while scanning. It writes while the scan also writes; if it loses the race
        // for the lock it is simply retried on the next poll.
        if let Ok(pairs) = index.merge_duplicates() {
            for (from, into) in pairs {
                if let Some(known) = self.known.get_mut(&from)
                    && !known.merged
                {
                    known.merged = true;
                    changes.merges.push(FileMerge {
                        from: known.file.id,
                        into: hakk_protocol::FileId(u64::try_from(into).unwrap_or(0)),
                    });
                }
            }
        }

        // Verification results for records already shown.
        if full || Instant::now() >= self.next_status_check {
            self.next_status_check = Instant::now() + STATUS_INTERVAL;
            for record in index.files(&Query::default())? {
                let Some(known) = self.known.get_mut(&record.id) else {
                    continue;
                };
                let status = convert::status(record.status);
                if status != known.file.status && !known.merged {
                    known.file.status = status;
                    changes.updates.push(FileUpdate {
                        id: known.file.id,
                        status,
                        repair: known.file.repair,
                    });
                }
            }
        }
        Ok(changes)
    }

    /// Final numbers, over the files the GUI shows (not merged away, not hidden).
    fn summary(
        &self,
        scan_id: ScanId,
        cancelled: bool,
        elapsed_seconds: u64,
        read_errors: u32,
    ) -> ScanSummary {
        let visible = || {
            self.known
                .values()
                .filter(|k| !k.merged && k.file.hidden.is_none())
                .map(|k| &k.file)
        };
        let count = |pred: &dyn Fn(&FoundFile) -> bool| {
            u32::try_from(visible().filter(|f| pred(f)).count()).unwrap_or(u32::MAX)
        };
        ScanSummary {
            scan_id,
            cancelled,
            photos: count(&|f| f.kind == FileKind::Photo),
            videos: count(&|f| f.kind == FileKind::Video),
            recoverable: count(&|f| {
                matches!(f.status, FileStatus::Recoverable | FileStatus::Repaired)
            }),
            partially_damaged: count(&|f| f.status == FileStatus::PartiallyDamaged),
            elapsed_seconds,
            read_errors,
        }
    }
}

#[cfg(test)]
mod tests;
