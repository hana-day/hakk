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
//! Drives are disk images opened by the user and, on macOS, the physical disks attached
//! (see [`drives`]). Reading a physical disk needs administrator privileges.

mod convert;
mod drives;
mod places;
mod recovery;
#[cfg(unix)]
pub mod remote;

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

use blocksource::{Aligned, BlockSource, Device, ImageFile};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use hakk_protocol::{
    Destination, Drive, DriveId, DriveKind, EventSink, FileKind, FileMerge, FileStatus, FileUpdate,
    FilesFound, FilesMerged, FilesUpdated, FoundFile, Helper, HelperError, HelperEvent, RecoveryId,
    RecoveryRequest, RepairRequest, SavedScan, ScanId, ScanSummary,
};
use index::{Index, Query};
use scan::{Analyzer, Event, Outcome, Scan, ScanHandle};

/// Builds the analyzers for each new scan (they keep per-scan state).
pub type AnalyzerFactory = Arc<dyn Fn() -> Vec<Box<dyn Analyzer>> + Send + Sync>;

/// Lists destinations; the argument is the whole disk being scanned (e.g. `disk4`) when it
/// is a physical disk, whose volumes are then blocked.
type PlaceLister = Box<dyn Fn(Option<&str>) -> Vec<Destination> + Send + Sync>;

/// A drive opened for reading: the source, its label, and the whole physical disk if it
/// is one.
type Opened = (Arc<dyn BlockSource>, String, Option<String>);

/// Where scans are saved by default: the app's data folder of the current user.
pub fn default_data_dir() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    });
    match home {
        Some(home) if cfg!(target_os = "macos") => {
            PathBuf::from(home).join("Library/Application Support/hakk")
        }
        Some(base) if cfg!(windows) => PathBuf::from(base).join("hakk"),
        Some(home) => PathBuf::from(home).join(".local/share/hakk"),
        None => std::env::temp_dir().join("hakk"),
    }
}

/// FNV-1a (64-bit): a short, stable name for a drive's saved scan.
fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// What the scan saved at `path` got to, if there is one.
fn saved_scan(path: &Path) -> Option<SavedScan> {
    if !path.exists() {
        return None;
    }
    let index = Index::open(path).ok()?;
    let state = index.scan_state().ok()??;
    let span = |r: &[std::ops::Range<u64>]| r.iter().map(|r| r.end - r.start).sum::<u64>();
    let planned = span(&state.planned).max(1);
    let done = span(&state.progress.done).min(planned);
    let counts = index.counts().ok()?;
    Some(SavedScan {
        percent_done: u8::try_from(done * 100 / planned).unwrap_or(100),
        files_found: counts.photos + counts.videos,
        complete: state.progress.phase == index::ScanPhase::Complete,
    })
}

/// How often the index is read for new and changed files.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub struct RealHelper {
    shared: Arc<Shared>,
}

struct Shared {
    sink: EventSink,
    analyzers: AnalyzerFactory,
    places: PlaceLister,
    /// Lists the physical drives attached now.
    disks: fn() -> Vec<drives::PhysicalDrive>,
    /// (user id, group id) that recovered files are given to.
    owner: Mutex<Option<(u32, u32)>>,
    /// Where scans are saved (`scans/<drive>.sqlite`), so they can be continued later.
    data_dir: Mutex<PathBuf>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    images: Vec<Image>,
    /// Physical drives as last listed.
    disks: Vec<drives::PhysicalDrive>,
    next_scan: u64,
    next_recovery: u64,
    scan: Option<ActiveScan>,
    recovering: bool,
}

struct Image {
    drive: Drive,
    path: PathBuf,
    /// Names the image's saved scan: its path and size.
    identity: String,
}

struct ActiveScan {
    id: ScanId,
    source: Arc<dyn BlockSource>,
    /// The whole physical disk scanned, if it is one (its volumes cannot be destinations).
    disk: Option<String>,
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

    /// `analyzers` builds the analyzers for each scan. Scans are saved in
    /// [`default_data_dir`].
    pub fn with_analyzers(sink: EventSink, analyzers: AnalyzerFactory) -> Self {
        Self::build(
            sink,
            analyzers,
            default_data_dir(),
            Box::new(places::destinations),
            drives::list,
        )
    }

    /// Saves scans in `dir` instead (the GUI's user's folder, when the helper runs as
    /// root).
    pub fn set_data_dir(&mut self, dir: PathBuf) {
        *self
            .shared
            .data_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = dir;
    }

    /// Gives recovered files to this user (user id, group id) instead of the user the
    /// helper runs as (root, when elevated).
    pub fn set_owner(&mut self, owner: Option<(u32, u32)>) {
        *self
            .shared
            .owner
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = owner;
    }

    /// A helper for tests: no physical drives, the given work folder and destinations.
    #[cfg(test)]
    fn with_options(
        sink: EventSink,
        analyzers: AnalyzerFactory,
        data_dir: PathBuf,
        places: PlaceLister,
    ) -> Self {
        Self::build(sink, analyzers, data_dir, places, Vec::new)
    }

    fn build(
        sink: EventSink,
        analyzers: AnalyzerFactory,
        data_dir: PathBuf,
        places: PlaceLister,
        disks: fn() -> Vec<drives::PhysicalDrive>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                sink,
                analyzers,
                places,
                disks,
                owner: Mutex::new(None),
                data_dir: Mutex::new(data_dir),
                state: Mutex::new(State::default()),
            }),
        }
    }
}

impl Shared {
    /// Where the scan of the drive with this identity is saved.
    fn index_path(&self, identity: &str) -> PathBuf {
        self.data_dir
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .join("scans")
            .join(format!("{:016x}.sqlite", fnv1a(identity.as_bytes())))
    }

    /// The drive with its saved scan, if any.
    fn with_saved(&self, mut drive: Drive, identity: &str) -> Drive {
        drive.saved_scan = saved_scan(&self.index_path(identity));
        drive
    }

    fn state_owner(&self) -> Option<(u32, u32)> {
        *self.owner.lock().unwrap_or_else(PoisonError::into_inner)
    }

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

    /// What names the drive's saved scan.
    fn identity(&self, id: &DriveId) -> Result<String, HelperError> {
        if drives::is_physical(id) {
            Ok(self.disk(id)?.identity.clone())
        } else {
            Ok(self.image(id)?.identity.clone())
        }
    }

    fn disk(&self, id: &DriveId) -> Result<&drives::PhysicalDrive, HelperError> {
        self.disks
            .iter()
            .find(|d| d.drive.id == *id)
            .ok_or(HelperError::DriveNotFound)
    }

    /// Opens a drive for reading: `(source, label, whole disk if physical)`.
    fn open(&self, id: &DriveId) -> Result<Opened, HelperError> {
        if drives::is_physical(id) {
            let disk = self.disk(id)?;
            let device = Device::open(&disk.device).map_err(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    HelperError::AdminRequired
                } else {
                    HelperError::CannotOpen {
                        detail: format!("{}: {e}", disk.device.display()),
                    }
                }
            })?;
            // Raw devices take only sector-aligned reads.
            return Ok((
                Arc::new(Aligned::new(device)),
                disk.drive.name.clone(),
                Some(disk.disk.clone()),
            ));
        }
        let image = self.image(id)?;
        let source = ImageFile::open(&image.path).map_err(|e| HelperError::CannotOpen {
            detail: e.to_string(),
        })?;
        Ok((Arc::new(source), image.drive.name.clone(), None))
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
        let disks = (self.shared.disks)();
        let mut state = self.shared.state();
        state.disks = disks;
        let listed: Vec<(Drive, String)> = state
            .disks
            .iter()
            .map(|d| (d.drive.clone(), d.identity.clone()))
            .chain(
                state
                    .images
                    .iter()
                    .map(|i| (i.drive.clone(), i.identity.clone())),
            )
            .collect();
        drop(state);
        Ok(listed
            .into_iter()
            .map(|(d, identity)| self.shared.with_saved(d, &identity))
            .collect())
    }

    fn open_image(&self, path: &str) -> Result<Drive, HelperError> {
        let path = PathBuf::from(path);
        let image = ImageFile::open(&path).map_err(|e| HelperError::CannotOpen {
            detail: e.to_string(),
        })?;
        let identity = format!(
            "image|{}|{}",
            std::fs::canonicalize(&path)
                .unwrap_or_else(|_| path.clone())
                .display(),
            image.len()
        );
        let mut state = self.shared.state();
        if let Some(existing) = state.images.iter().find(|i| i.path == path) {
            let drive = existing.drive.clone();
            drop(state);
            return Ok(self.shared.with_saved(drive, &identity));
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
            saved_scan: None,
        };
        state.images.push(Image {
            drive: drive.clone(),
            path,
            identity: identity.clone(),
        });
        drop(state);
        Ok(self.shared.with_saved(drive, &identity))
    }

    fn list_destinations(&self, source: &DriveId) -> Result<Vec<Destination>, HelperError> {
        let state = self.shared.state();
        // Images are regular files: writing next to them cannot overwrite their content.
        let disk = if drives::is_physical(source) {
            Some(state.disk(source)?.disk.clone())
        } else {
            state.image(source)?;
            None
        };
        drop(state);
        Ok((self.shared.places)(disk.as_deref()))
    }

    fn start_scan(&self, drive: &DriveId, fresh: bool) -> Result<ScanId, HelperError> {
        let mut state = self.shared.state();
        let (source, label, disk) = state.open(drive)?;
        let index_path = self.shared.index_path(&state.identity(drive)?);
        if let Some(previous) = state.scan.take() {
            previous.handle.cancel();
        }
        state.next_scan += 1;
        let id = ScanId(state.next_scan);

        let scans = index_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        std::fs::create_dir_all(&scans).map_err(internal)?;
        if fresh {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", index_path.display()));
            }
        }
        let index = Index::open(&index_path).map_err(internal)?;
        // Continue the saved scan if it was of this same source.
        let saved = index
            .scan_state()
            .map_err(internal)?
            .filter(|s| s.source.len == source.len());
        let mut scan = Scan::new(source.clone())
            .label(label)
            .analyzers((self.shared.analyzers)());
        if let Some(saved) = saved {
            let known = index
                .files(&index::Query::default())
                .map_err(internal)?
                .into_iter()
                .map(|f| (f.offset, f.kind.media()));
            scan = scan.known_files(known).resume_from(saved);
        }
        let handle = Arc::new(scan.start(index).map_err(internal)?);
        // When the helper runs as root, the saved scan stays the user's.
        if let Some((uid, gid)) = self.shared.state_owner() {
            recovery::hand_over(&scans, uid, gid);
        }
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
            disk,
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
        let scan = state.scan(request.scan_id)?;
        // Checked again here: the GUI's list may be stale, and a volume on the scanned disk
        // must never receive files.
        let destination = (self.shared.places)(scan.disk.as_deref())
            .into_iter()
            .find(|d| d.id == request.destination)
            .ok_or(HelperError::DestinationNotFound)?;
        if let Some(reason) = destination.blocked {
            return Err(HelperError::DestinationBlocked { reason });
        }
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
            owner: self.shared.state_owner(),
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
                    self.poll();
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
                self.poll();
                next_poll = Instant::now() + POLL_INTERVAL;
            }
        }
    }

    fn poll(&mut self) {
        let scan_id = self.scan_id;
        match self.poller.poll() {
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
///
/// Every record carries a revision the index bumps on insert and on any change the GUI
/// shows (status, merge, name, ...), so each poll reads only what changed.
struct Poller {
    path: PathBuf,
    index: Option<Index>,
    /// Highest revision seen so far.
    last_rev: i64,
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
            last_rev: 0,
            known: HashMap::new(),
            folders: HashMap::new(),
        }
    }

    fn poll(&mut self) -> Result<Changes, index::Error> {
        if self.index.is_none() {
            self.index = Some(Index::open(&self.path)?);
        }
        let Some(index) = self.index.as_mut() else {
            return Ok(Changes::default());
        };
        // Fold carving results into the named records they duplicate, so the merge shows
        // up while scanning. It writes while the scan also writes; if it loses the race
        // for the lock it is simply retried on the next poll. Merges bump the revision, so
        // they are picked up below.
        let _ = index.merge_duplicates();

        let records = index.files(&Query {
            changed_after: Some(self.last_rev),
            include_merged: true,
            ..Query::default()
        })?;
        let mut changes = Changes::default();
        for record in records {
            self.last_rev = self.last_rev.max(record.rev);
            match self.known.get_mut(&record.id) {
                None => {
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
                Some(known) if known.merged => {}
                Some(known) => {
                    if let Some(into) = record.merged_into {
                        known.merged = true;
                        changes.merges.push(FileMerge {
                            from: known.file.id,
                            into: hakk_protocol::FileId(u64::try_from(into).unwrap_or(0)),
                        });
                        continue;
                    }
                    let status = convert::status(record.status);
                    if status != known.file.status {
                        known.file.status = status;
                        changes.updates.push(FileUpdate {
                            id: known.file.id,
                            status,
                            repair: known.file.repair,
                        });
                    }
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
