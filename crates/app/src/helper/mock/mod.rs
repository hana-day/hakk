//! In-process stand-in for the privileged helper.
//!
//! Simulates a scan of fake drives (see [`dataset`]): files appear as the read position
//! advances, are verified a little later, and some carved files are merged into file system
//! results. A recovery pauses the scan and resumes it afterwards, as the real helper will.
//! Simulated time runs faster than wall-clock time so a whole scan takes about
//! [`MockConfig::scan_seconds`].

// Simulation arithmetic mixes floats and byte counts that are far below the truncation limits.
#![allow(clippy::cast_possible_truncation)]

mod dataset;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;
use std::time::Duration;

use hakk_protocol::{
    Destination, DestinationId, DiskMap, Drive, DriveId, FileId, FileKind, FileMerge, FileStatus,
    FileUpdate, FilesFound, FilesMerged, FilesUpdated, FoundFile, HelperError, HelperEvent,
    MapCell, RecoveryId, RecoveryProgress, RecoveryReport, RecoveryRequest, RepairReport,
    RepairRequest, ScanId, ScanProgress, ScanStage, ScanState, ScanSummary,
};

use self::dataset::{MockDrive, PlannedFile, Reveal, Rng};
use super::{EventSink, Helper};

/// Number of cells in the disk map (48 columns by 12 rows in the GUI).
const MAP_CELLS: u32 = 576;
/// Share of the scan time spent in the file system stage.
const FILE_SYSTEM_STAGE_SHARE: f64 = 0.08;
/// Bytes read during the file system stage (metadata near the start of the disk).
const FILE_SYSTEM_STAGE_BYTES_SHARE: f64 = 0.006;
/// Cells that fail to read, as in the mock screens.
const ERROR_CELLS: [u32; 2] = [120, 121];
/// Wall-clock speed of a simulated recovery.
const RECOVERY_BYTES_PER_SECOND: f64 = 40_000_000.0;
const RECOVERY_SECONDS: (f64, f64) = (3.0, 20.0);
const REPAIR_SECONDS: f64 = 3.0;
const TICK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct MockConfig {
    /// Wall-clock length of a full scan.
    pub scan_seconds: f64,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self { scan_seconds: 40.0 }
    }
}

pub struct MockHelper {
    sim: Arc<Mutex<Sim>>,
}

impl MockHelper {
    /// Starts the simulation clock on a background thread. The thread stops when the helper
    /// is dropped.
    pub fn new(sink: EventSink, config: MockConfig) -> Self {
        let sim = Arc::new(Mutex::new(Sim::new(sink, config)));
        let weak: Weak<Mutex<Sim>> = Arc::downgrade(&sim);
        thread::Builder::new()
            .name("mock-helper".into())
            .spawn(move || {
                loop {
                    thread::sleep(TICK);
                    let Some(sim) = weak.upgrade() else { break };
                    lock(&sim).tick(TICK.as_secs_f64());
                }
            })
            .ok();
        Self { sim }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Sim) -> T) -> T {
        f(&mut lock(&self.sim))
    }
}

/// A panic while holding the lock leaves the simulation usable, so ignore poisoning.
fn lock(sim: &Mutex<Sim>) -> std::sync::MutexGuard<'_, Sim> {
    sim.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Helper for MockHelper {
    fn list_drives(&self) -> Result<Vec<Drive>, HelperError> {
        Ok(self.with(|s| s.drives.iter().map(|d| d.drive.clone()).collect()))
    }

    fn list_destinations(&self, source: &DriveId) -> Result<Vec<Destination>, HelperError> {
        self.with(|s| Ok(dataset::destinations(s.drive(source)?.device)))
    }

    fn start_scan(&self, drive: &DriveId) -> Result<ScanId, HelperError> {
        self.with(|s| s.start_scan(drive))
    }

    fn pause_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.with(|s| s.set_user_paused(scan, true))
    }

    fn resume_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.with(|s| s.set_user_paused(scan, false))
    }

    fn cancel_scan(&self, scan: ScanId) -> Result<(), HelperError> {
        self.with(|s| s.cancel_scan(scan))
    }

    fn recover(&self, request: RecoveryRequest) -> Result<RecoveryId, HelperError> {
        self.with(|s| s.recover(request))
    }

    fn repair(&self, request: RepairRequest) -> Result<(), HelperError> {
        self.with(|s| s.repair(request))
    }
}

struct Sim {
    sink: EventSink,
    config: MockConfig,
    drives: Vec<MockDrive>,
    /// Wall-clock seconds since the simulation started.
    clock: f64,
    next_scan: u64,
    next_recovery: u64,
    scan: Option<Scan>,
    recovery: Option<Recovery>,
    /// Pending repairs as (due time, file).
    repairs: Vec<(f64, FileId)>,
}

struct Scan {
    id: ScanId,
    drive: usize,
    plan: Vec<PlannedFile>,
    /// Next plan entry to reveal.
    next: usize,
    /// Files reported to the GUI and not merged away, keyed by id (= index into `plan`).
    found: BTreeMap<FileId, FoundFile>,
    /// Pending verification results and merges, in due-time order.
    verifications: VecDeque<(f64, FileId)>,
    merges: VecDeque<(f64, FileMerge)>,
    user_paused: bool,
    ended: Option<ScanState>,
    stage: ScanStage,
    /// Wall-clock seconds spent reading (excludes pauses).
    active: f64,
    bytes_read: u64,
    bytes_per_second: u64,
    found_cells: Vec<bool>,
    error_cells: Vec<u32>,
    rng: Rng,
}

struct Recovery {
    id: RecoveryId,
    folder: String,
    /// (size, status) of each file, in copy order.
    files: Vec<(u64, FileStatus)>,
    bytes_total: u64,
    bytes_done: f64,
    bytes_per_second: f64,
}

impl Sim {
    fn new(sink: EventSink, config: MockConfig) -> Self {
        Self {
            sink,
            config,
            drives: dataset::drives(),
            clock: 0.0,
            next_scan: 1,
            next_recovery: 1,
            scan: None,
            recovery: None,
            repairs: Vec::new(),
        }
    }

    fn drive(&self, id: &DriveId) -> Result<&MockDrive, HelperError> {
        self.drives
            .iter()
            .find(|d| d.drive.id == *id)
            .ok_or(HelperError::DriveNotFound)
    }

    fn emit(&self, event: HelperEvent) {
        (self.sink)(event);
    }

    fn start_scan(&mut self, drive: &DriveId) -> Result<ScanId, HelperError> {
        let index = self
            .drives
            .iter()
            .position(|d| d.drive.id == *drive)
            .ok_or(HelperError::DriveNotFound)?;
        if let Some(scan) = self.scan.as_mut()
            && scan.ended.is_none()
        {
            scan.ended = Some(ScanState::Cancelled);
        }
        let mock = self.drives.get(index).ok_or(HelperError::DriveNotFound)?;
        let id = ScanId(self.next_scan);
        self.next_scan += 1;
        self.recovery = None;
        self.repairs.clear();
        self.scan = Some(Scan {
            id,
            drive: index,
            plan: dataset::plan(mock),
            next: 0,
            found: BTreeMap::new(),
            verifications: VecDeque::new(),
            merges: VecDeque::new(),
            user_paused: false,
            ended: None,
            stage: ScanStage::FileSystem,
            active: 0.0,
            bytes_read: 0,
            bytes_per_second: mock.bytes_per_second,
            found_cells: vec![false; MAP_CELLS as usize],
            error_cells: Vec::new(),
            rng: Rng(id.0),
        });
        self.emit_progress();
        Ok(id)
    }

    fn scan_mut(&mut self, id: ScanId) -> Result<&mut Scan, HelperError> {
        match self.scan.as_mut() {
            Some(scan) if scan.id == id => Ok(scan),
            _ => Err(HelperError::ScanNotFound),
        }
    }

    fn set_user_paused(&mut self, id: ScanId, paused: bool) -> Result<(), HelperError> {
        let scan = self.scan_mut(id)?;
        if scan.ended.is_some() {
            return Err(HelperError::ScanEnded);
        }
        scan.user_paused = paused;
        self.emit_progress();
        Ok(())
    }

    fn cancel_scan(&mut self, id: ScanId) -> Result<(), HelperError> {
        let scan = self.scan_mut(id)?;
        if scan.ended.is_some() {
            return Err(HelperError::ScanEnded);
        }
        scan.ended = Some(ScanState::Cancelled);
        self.finish_scan();
        Ok(())
    }

    /// Checks that `destination` exists and is not on the scanned device.
    fn check_destination(
        &self,
        scan: ScanId,
        destination: &DestinationId,
    ) -> Result<Destination, HelperError> {
        let drive = match &self.scan {
            Some(s) if s.id == scan => self.drives.get(s.drive),
            _ => None,
        }
        .ok_or(HelperError::ScanNotFound)?;
        let dest = dataset::destinations(drive.device)
            .into_iter()
            .find(|d| d.id == *destination)
            .ok_or(HelperError::DestinationNotFound)?;
        match dest.blocked {
            Some(reason) => Err(HelperError::DestinationBlocked { reason }),
            None => Ok(dest),
        }
    }

    fn recover(&mut self, request: RecoveryRequest) -> Result<RecoveryId, HelperError> {
        let dest = self.check_destination(request.scan_id, &request.destination)?;
        if self.recovery.is_some() {
            return Err(HelperError::RecoveryInProgress);
        }
        if request.files.is_empty() {
            return Err(HelperError::NothingRequested);
        }
        let scan = self.scan_mut(request.scan_id)?;
        let files = request
            .files
            .iter()
            .map(|id| {
                scan.found
                    .get(id)
                    .map(|f| (f.size, f.status))
                    .ok_or(HelperError::FileNotFound)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let bytes_total: u64 = files.iter().map(|(size, _)| size).sum();
        let seconds = (bytes_total as f64 / RECOVERY_BYTES_PER_SECOND)
            .clamp(RECOVERY_SECONDS.0, RECOVERY_SECONDS.1);
        let id = RecoveryId(self.next_recovery);
        self.next_recovery += 1;
        self.recovery = Some(Recovery {
            id,
            folder: dest.path,
            files,
            bytes_total,
            bytes_done: 0.0,
            bytes_per_second: bytes_total as f64 / seconds,
        });
        self.emit_recovery_progress();
        self.emit_progress();
        Ok(id)
    }

    fn repair(&mut self, request: RepairRequest) -> Result<(), HelperError> {
        self.check_destination(request.scan_id, &request.destination)?;
        let scan = self.scan_mut(request.scan_id)?;
        let file = scan
            .found
            .get(&request.file)
            .ok_or(HelperError::FileNotFound)?;
        if file.repair != Some(request.method) {
            return Err(HelperError::Internal {
                detail: "this repair is not offered for the file".into(),
            });
        }
        self.repairs
            .push((self.clock + REPAIR_SECONDS, request.file));
        Ok(())
    }

    fn tick(&mut self, dt: f64) {
        self.clock += dt;
        self.tick_recovery(dt);
        self.tick_repairs();
        self.tick_scan(dt);
    }

    fn tick_recovery(&mut self, dt: f64) {
        let Some(rec) = self.recovery.as_mut() else {
            return;
        };
        rec.bytes_done = (rec.bytes_done + rec.bytes_per_second * dt).min(rec.bytes_total as f64);
        self.emit_recovery_progress();
        let Some(rec) = self.recovery.as_ref() else {
            return;
        };
        if rec.bytes_done < rec.bytes_total as f64 {
            return;
        }
        let damaged =
            |s: &FileStatus| matches!(s, FileStatus::PartiallyDamaged | FileStatus::IndexMissing);
        let report = RecoveryReport {
            recovery_id: rec.id,
            recovered: rec.files.len() as u32,
            partially_damaged: rec.files.iter().filter(|(_, s)| damaged(s)).count() as u32,
            failed: 0,
            folder: rec.folder.clone(),
        };
        self.recovery = None;
        self.emit(HelperEvent::RecoveryFinished(report));
        self.emit_progress();
    }

    fn tick_repairs(&mut self) {
        let clock = self.clock;
        let (due, pending): (Vec<_>, Vec<_>) =
            self.repairs.drain(..).partition(|(at, _)| *at <= clock);
        self.repairs = pending;
        let Some(scan) = self.scan.as_mut() else {
            return;
        };
        let scan_id = scan.id;
        let mut updates = Vec::new();
        for (_, id) in due {
            if let Some(file) = scan.found.get_mut(&id) {
                file.status = FileStatus::Repaired;
                file.repair = None;
                updates.push(FileUpdate {
                    id,
                    status: file.status,
                    repair: None,
                });
            }
        }
        if updates.is_empty() {
            return;
        }
        let files: Vec<FileId> = updates.iter().map(|u| u.id).collect();
        self.emit(HelperEvent::FilesUpdated(FilesUpdated { scan_id, updates }));
        for file in files {
            self.emit(HelperEvent::RepairFinished(RepairReport {
                file,
                succeeded: true,
            }));
        }
    }

    fn tick_scan(&mut self, dt: f64) {
        let recovering = self.recovery.is_some();
        let scan_seconds = self.config.scan_seconds;
        let clock = self.clock;
        let Some(drive) = self.scan.as_ref().and_then(|s| self.drives.get(s.drive)) else {
            return;
        };
        let total = drive.drive.capacity_bytes;
        let nominal_rate = drive.bytes_per_second;
        let Some(scan) = self.scan.as_mut() else {
            return;
        };
        let reading = scan.ended.is_none() && !scan.user_paused && !recovering;
        let mut found = Vec::new();
        if reading {
            scan.active += dt;
            let fs_seconds = scan_seconds * FILE_SYSTEM_STAGE_SHARE;
            let fs_bytes = (total as f64 * FILE_SYSTEM_STAGE_BYTES_SHARE) as u64;
            let jitter = 0.95 + scan.rng.unit() * 0.1;
            scan.bytes_per_second = (nominal_rate as f64 * jitter) as u64;
            if scan.active < fs_seconds {
                let stage_progress = scan.active / fs_seconds;
                scan.bytes_read = (fs_bytes as f64 * stage_progress) as u64;
                while let Some(file) = scan.plan.get(scan.next) {
                    match file.reveal {
                        Reveal::FileSystem(at) if at <= stage_progress => {
                            found.extend(scan.reveal(clock));
                        }
                        _ => break,
                    }
                }
            } else {
                // Reveal whatever the file system stage has left before moving on.
                while let Some(Reveal::FileSystem(_)) = scan.plan.get(scan.next).map(|f| f.reveal) {
                    found.extend(scan.reveal(clock));
                }
                scan.stage = ScanStage::Carving;
                let carving = (scan.active - fs_seconds) / (scan_seconds - fs_seconds);
                scan.bytes_read = fs_bytes + ((total - fs_bytes) as f64 * carving.min(1.0)) as u64;
                while let Some(file) = scan.plan.get(scan.next) {
                    if file.offset > scan.bytes_read {
                        break;
                    }
                    found.extend(scan.reveal(clock));
                }
                for cell in ERROR_CELLS {
                    let start = cell_start(cell, total);
                    if scan.bytes_read > start && !scan.error_cells.contains(&cell) {
                        scan.error_cells.push(cell);
                    }
                }
            }
        }

        let mut updates = Vec::new();
        while scan
            .verifications
            .front()
            .is_some_and(|(at, _)| *at <= clock)
        {
            if let Some((_, id)) = scan.verifications.pop_front() {
                updates.extend(scan.verify(id));
            }
        }
        let mut merges = Vec::new();
        while scan.merges.front().is_some_and(|(at, _)| *at <= clock) {
            if let Some((_, merge)) = scan.merges.pop_front() {
                scan.found.remove(&merge.from);
                merges.push(merge);
            }
        }
        let finished = reading && scan.bytes_read >= total;
        if finished {
            // Settle every pending result so the summary has final numbers.
            while let Some((_, id)) = scan.verifications.pop_front() {
                updates.extend(scan.verify(id));
            }
            while let Some((_, merge)) = scan.merges.pop_front() {
                scan.found.remove(&merge.from);
                merges.push(merge);
            }
            scan.ended = Some(ScanState::Finished);
        }
        // Only carving finds files at the read position; file system results come from metadata.
        let carved = found
            .iter()
            .filter(|f| matches!(f.origin, hakk_protocol::FileOrigin::Carved { .. }));
        for cell in carved.map(|f| cell_of(f.offset, total)) {
            if let Some(slot) = scan.found_cells.get_mut(cell as usize) {
                *slot = true;
            }
        }

        let scan_id = scan.id;
        let ended = scan.ended.is_some();
        if !found.is_empty() {
            self.emit(HelperEvent::FilesFound(FilesFound {
                scan_id,
                files: found,
            }));
        }
        if !updates.is_empty() {
            self.emit(HelperEvent::FilesUpdated(FilesUpdated { scan_id, updates }));
        }
        if !merges.is_empty() {
            self.emit(HelperEvent::FilesMerged(FilesMerged { scan_id, merges }));
        }
        if finished {
            self.finish_scan();
        } else if !ended {
            self.emit_progress();
        }
    }

    fn finish_scan(&mut self) {
        self.emit_progress();
        let Some(scan) = self.scan.as_ref() else {
            return;
        };
        let rate = self
            .drives
            .get(scan.drive)
            .map_or(1, |d| d.bytes_per_second);
        let visible = || scan.found.values().filter(|f| f.hidden.is_none());
        let count =
            |pred: &dyn Fn(&FoundFile) -> bool| visible().filter(|f| pred(f)).count() as u32;
        let summary = ScanSummary {
            scan_id: scan.id,
            cancelled: scan.ended == Some(ScanState::Cancelled),
            photos: count(&|f| f.kind == FileKind::Photo),
            videos: count(&|f| f.kind == FileKind::Video),
            recoverable: count(&|f| {
                matches!(f.status, FileStatus::Recoverable | FileStatus::Repaired)
            }),
            partially_damaged: count(&|f| f.status == FileStatus::PartiallyDamaged),
            elapsed_seconds: scan.bytes_read / rate.max(1),
            read_errors: scan.error_cells.len() as u32,
        };
        self.emit(HelperEvent::ScanFinished(summary));
    }

    fn emit_progress(&self) {
        let Some(scan) = self.scan.as_ref() else {
            return;
        };
        let Some(drive) = self.drives.get(scan.drive) else {
            return;
        };
        let total = drive.drive.capacity_bytes;
        let state = match scan.ended {
            Some(state) => state,
            None if self.recovery.is_some() => ScanState::PausedForRecovery,
            None if scan.user_paused => ScanState::Paused,
            None => ScanState::Running,
        };
        let visible = |kind| {
            scan.found
                .values()
                .filter(|f| f.kind == kind && f.hidden.is_none())
                .count() as u32
        };
        let read_cells =
            (scan.bytes_read as f64 / total as f64 * f64::from(MAP_CELLS)).ceil() as u32;
        let cells = (0..MAP_CELLS)
            .map(|i| {
                if scan.error_cells.contains(&i) {
                    MapCell::Error
                } else if i >= read_cells {
                    MapCell::Unread
                } else if scan.found_cells.get(i as usize).copied().unwrap_or(false) {
                    MapCell::Found
                } else {
                    MapCell::Read
                }
            })
            .collect();
        let remaining_seconds = (scan.stage == ScanStage::Carving && scan.bytes_per_second > 0)
            .then(|| (total - scan.bytes_read.min(total)) / scan.bytes_per_second);
        self.emit(HelperEvent::ScanProgress(ScanProgress {
            scan_id: scan.id,
            state,
            stage: scan.stage,
            bytes_read: scan.bytes_read,
            total_bytes: total,
            bytes_per_second: scan.bytes_per_second,
            remaining_seconds,
            photos_found: visible(FileKind::Photo),
            videos_found: visible(FileKind::Video),
            read_errors: scan.error_cells.len() as u32,
            disk_map: DiskMap {
                cells,
                current: scan
                    .ended
                    .is_none()
                    .then(|| cell_of(scan.bytes_read, total)),
            },
        }));
    }

    fn emit_recovery_progress(&self) {
        let Some(rec) = self.recovery.as_ref() else {
            return;
        };
        let mut files_done = 0;
        let mut copied = 0;
        for (size, _) in &rec.files {
            copied += size;
            if copied as f64 > rec.bytes_done {
                break;
            }
            files_done += 1;
        }
        self.emit(HelperEvent::RecoveryProgress(RecoveryProgress {
            recovery_id: rec.id,
            files_done,
            files_total: rec.files.len() as u32,
            bytes_done: rec.bytes_done as u64,
            bytes_total: rec.bytes_total,
        }));
    }
}

impl Scan {
    /// Reports the next planned file as found, still unverified.
    fn reveal(&mut self, clock: f64) -> Option<FoundFile> {
        let index = self.next;
        let plan = self.plan.get(index)?;
        self.next += 1;
        let id = FileId(index as u64);
        let file = FoundFile {
            id,
            kind: plan.kind,
            name: plan.name.clone(),
            origin: plan.origin.clone(),
            offset: plan.offset,
            size: plan.size,
            captured_at: plan.captured_at.clone(),
            duration_seconds: plan.duration_seconds,
            status: FileStatus::Unknown,
            hidden: plan.hidden,
            repair: None,
        };
        match plan.duplicate_of {
            Some(of) => self.merges.push_back((
                clock + plan.verify_delay,
                FileMerge {
                    from: id,
                    into: FileId(of as u64),
                },
            )),
            None => insert_sorted(&mut self.verifications, (clock + plan.verify_delay, id)),
        }
        self.found.insert(id, file.clone());
        Some(file)
    }

    fn verify(&mut self, id: FileId) -> Option<FileUpdate> {
        let plan = self.plan.get(id.0 as usize)?;
        let file = self.found.get_mut(&id)?;
        // A repair that already finished wins over a late verification.
        if file.status == FileStatus::Repaired {
            return None;
        }
        file.status = plan.status;
        file.repair = plan.repair;
        Some(FileUpdate {
            id,
            status: plan.status,
            repair: plan.repair,
        })
    }
}

fn insert_sorted(queue: &mut VecDeque<(f64, FileId)>, item: (f64, FileId)) {
    let at = queue.partition_point(|(due, _)| *due <= item.0);
    queue.insert(at, item);
}

fn cell_of(offset: u64, total: u64) -> u32 {
    ((u128::from(offset) * u128::from(MAP_CELLS) / u128::from(total.max(1))) as u32)
        .min(MAP_CELLS - 1)
}

fn cell_start(cell: u32, total: u64) -> u64 {
    (u128::from(total) * u128::from(cell) / u128::from(MAP_CELLS)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A simulation whose events are collected for inspection.
    fn sim() -> (Sim, Arc<Mutex<Vec<HelperEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let sink: EventSink = Box::new(move |e| sink_events.lock().unwrap().push(e));
        let sim = Sim::new(sink, MockConfig { scan_seconds: 10.0 });
        (sim, events)
    }

    fn run(sim: &mut Sim, seconds: f64) {
        for _ in 0..(seconds / 0.1).round() as u32 {
            sim.tick(0.1);
        }
    }

    fn sd() -> DriveId {
        DriveId("sd".into())
    }

    #[test]
    fn full_scan_reports_every_file_and_a_summary() {
        let (mut sim, events) = sim();
        let id = sim.start_scan(&sd()).unwrap();
        run(&mut sim, 11.0);
        let events = events.lock().unwrap();
        let summary = events
            .iter()
            .find_map(|e| match e {
                HelperEvent::ScanFinished(s) => Some(s.clone()),
                _ => None,
            })
            .expect("scan finished");
        assert_eq!(summary.scan_id, id);
        assert!(!summary.cancelled);
        assert_eq!(summary.photos, 3412);
        assert_eq!(summary.videos, 148);
        assert_eq!(summary.read_errors, 2);
        let merged: usize = events
            .iter()
            .map(|e| match e {
                HelperEvent::FilesMerged(m) => m.merges.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(merged, 8);
        let last = events.iter().rev().find_map(|e| match e {
            HelperEvent::ScanProgress(p) => Some(p.clone()),
            _ => None,
        });
        let last = last.expect("progress");
        assert_eq!(last.state, ScanState::Finished);
        assert_eq!(last.bytes_read, last.total_bytes);
        assert!(last.disk_map.cells.iter().all(|c| *c != MapCell::Unread));
    }

    #[test]
    fn recovery_pauses_the_scan_and_resumes_it() {
        let (mut sim, events) = sim();
        let scan_id = sim.start_scan(&sd()).unwrap();
        run(&mut sim, 2.0);
        let files: Vec<FileId> = sim
            .scan
            .as_ref()
            .unwrap()
            .found
            .keys()
            .take(3)
            .copied()
            .collect();
        sim.recover(RecoveryRequest {
            scan_id,
            files,
            destination: DestinationId("backup".into()),
        })
        .unwrap();
        let read_before = sim.scan.as_ref().unwrap().bytes_read;
        run(&mut sim, 1.0);
        assert_eq!(sim.scan.as_ref().unwrap().bytes_read, read_before);
        run(&mut sim, RECOVERY_SECONDS.0 + 0.5);
        assert!(sim.scan.as_ref().unwrap().bytes_read > read_before);
        let events = events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            HelperEvent::ScanProgress(p) if p.state == ScanState::PausedForRecovery
        )));
        let report = events.iter().find_map(|e| match e {
            HelperEvent::RecoveryFinished(r) => Some(r.clone()),
            _ => None,
        });
        assert_eq!(report.map(|r| r.recovered), Some(3));
    }

    #[test]
    fn recovery_to_the_scanned_device_is_refused() {
        let (mut sim, _) = sim();
        let scan_id = sim.start_scan(&sd()).unwrap();
        run(&mut sim, 1.0);
        let file = *sim.scan.as_ref().unwrap().found.keys().next().unwrap();
        let err = sim
            .recover(RecoveryRequest {
                scan_id,
                files: vec![file],
                destination: DestinationId("sd".into()),
            })
            .unwrap_err();
        assert_eq!(
            err,
            HelperError::DestinationBlocked {
                reason: hakk_protocol::DestinationBlock::SameDeviceAsSource
            }
        );
    }

    #[test]
    fn repair_marks_the_file_repaired() {
        let (mut sim, events) = sim();
        let scan_id = sim.start_scan(&sd()).unwrap();
        run(&mut sim, 11.0);
        let (id, method) = sim
            .scan
            .as_ref()
            .unwrap()
            .found
            .values()
            .find_map(|f| f.repair.map(|m| (f.id, m)))
            .expect("a repairable video");
        sim.repair(RepairRequest {
            scan_id,
            file: id,
            method,
            destination: DestinationId("backup".into()),
        })
        .unwrap();
        run(&mut sim, REPAIR_SECONDS + 0.2);
        assert_eq!(
            sim.scan.as_ref().unwrap().found[&id].status,
            FileStatus::Repaired
        );
        assert!(events.lock().unwrap().iter().any(|e| matches!(
            e,
            HelperEvent::RepairFinished(r) if r.file == id && r.succeeded
        )));
    }

    #[test]
    fn pause_and_cancel() {
        let (mut sim, events) = sim();
        let id = sim.start_scan(&sd()).unwrap();
        run(&mut sim, 2.0);
        sim.set_user_paused(id, true).unwrap();
        let read = sim.scan.as_ref().unwrap().bytes_read;
        run(&mut sim, 1.0);
        assert_eq!(sim.scan.as_ref().unwrap().bytes_read, read);
        sim.cancel_scan(id).unwrap();
        assert_eq!(sim.set_user_paused(id, false), Err(HelperError::ScanEnded));
        assert!(events.lock().unwrap().iter().any(|e| matches!(
            e,
            HelperEvent::ScanFinished(s) if s.cancelled
        )));
    }
}
