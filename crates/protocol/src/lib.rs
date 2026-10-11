//! Types exchanged between the GUI (normal privileges) and the privileged helper.
//!
//! The helper reads the source disk and writes recovered files to a destination chosen by
//! the GUI. Everything that crosses the process boundary is defined here, so both sides and
//! the generated TypeScript bindings stay in sync.
//!
//! Conventions:
//! - Values are data, never display text. The GUI localizes enums and formats numbers.
//! - Byte offsets and sizes are `u64`. They are exported to TypeScript as `number`, which is
//!   exact up to 2^53 bytes (8 PiB).
//! - Timestamps without a time zone (EXIF capture time) are ISO 8601 local date-times,
//!   e.g. `2025-08-14T15:30:22`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Opaque identifier of a source drive (a physical disk, a volume or a disk image).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct DriveId(pub String);

/// Opaque identifier of a place recovered files can be written to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct DestinationId(pub String);

/// Identifier of a scan session. Events from an older session must be ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct ScanId(#[ts(type = "number")] pub u64);

/// Identifier of a found file, unique within one scan session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
pub struct FileId(#[ts(type = "number")] pub u64);

/// Identifier of a recovery job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct RecoveryId(#[ts(type = "number")] pub u64);

/// What kind of device a drive is, in terms the user recognizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum DriveKind {
    SdCard,
    UsbFlash,
    ExternalHdd,
    ExternalSsd,
    InternalHdd,
    InternalSsd,
    DiskImage,
}

/// Something the user should know before scanning a drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum DriveWarning {
    /// The drive is an SSD; TRIM has probably erased deleted data.
    TrimLikely,
    /// The drive is encrypted; only the unlocked volume can be scanned.
    Encrypted,
    /// Many read errors; copying the drive to an image first is safer.
    Failing,
}

/// A drive that can be scanned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Drive {
    pub id: DriveId,
    pub kind: DriveKind,
    /// Volume label or model name, e.g. `EOS_DIGITAL`.
    pub name: String,
    #[ts(type = "number")]
    pub capacity_bytes: u64,
    /// File system name as commonly written, e.g. `exFAT`. `None` if unrecognized.
    pub file_system: Option<String>,
    pub warnings: Vec<DriveWarning>,
}

/// Why a destination cannot be chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum DestinationBlock {
    /// On the same physical device as the drive being scanned. Writing there would
    /// overwrite the data being recovered.
    SameDeviceAsSource,
    ReadOnly,
}

/// A place recovered files can be written to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Destination {
    pub id: DestinationId,
    pub kind: DriveKind,
    pub name: String,
    /// Folder the files are written into.
    pub path: String,
    #[ts(type = "number")]
    pub free_bytes: u64,
    /// `Some` if this destination must not be chosen.
    pub blocked: Option<DestinationBlock>,
}

/// Phase of a scan. Scans run the phases in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ScanStage {
    /// Reading file system metadata. Finds deleted files with their names and folders.
    FileSystem,
    /// Reading the whole disk and carving files by their content.
    Carving,
    /// Reading areas that failed again, in several passes. On a failing disk this can take
    /// much longer than the main pass, so it is reported as its own stage.
    Retrying,
}

/// Progress of the retry stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RetryProgress {
    /// Current pass, starting at 1.
    pub pass: u32,
    pub passes: u32,
    /// Bytes still unreadable.
    #[ts(type = "number")]
    pub bytes_left: u64,
}

/// Run state of a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ScanState {
    Running,
    /// Paused by the user.
    Paused,
    /// Paused by the helper so a recovery can read the disk without seeking back and forth.
    /// The scan resumes from the same position when the recovery finishes.
    PausedForRecovery,
    Finished,
    Cancelled,
}

/// State of one cell of the disk map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum MapCell {
    /// Outside what this scan reads (e.g. allocated space in a free-space-only scan).
    Skipped,
    Unread,
    Read,
    /// Read, and at least one file starts in this cell.
    Found,
    /// A read error occurred in this cell.
    Error,
}

/// Coarse picture of the disk: the disk is divided into equal cells in offset order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DiskMap {
    pub cells: Vec<MapCell>,
    /// Cell being read now, if the scan is reading.
    pub current: Option<u32>,
}

/// Periodic scan progress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub scan_id: ScanId,
    pub state: ScanState,
    pub stage: ScanStage,
    #[ts(type = "number")]
    pub bytes_read: u64,
    #[ts(type = "number")]
    pub total_bytes: u64,
    #[ts(type = "number")]
    pub bytes_per_second: u64,
    /// `None` while the estimate is not yet meaningful.
    #[ts(type = "number | null")]
    pub remaining_seconds: Option<u64>,
    pub photos_found: u32,
    pub videos_found: u32,
    pub read_errors: u32,
    /// Set during the retry stage.
    pub retry: Option<RetryProgress>,
    pub disk_map: DiskMap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Photo,
    Video,
}

/// How a file was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FileOrigin {
    /// From file system metadata. The name and folders are the original ones.
    FileSystem {
        volume: VolumeRef,
        /// Folders below the volume root, e.g. `["DCIM", "100CANON"]`.
        folders: Vec<String>,
        /// `false` when part of the name was lost and guessed (the first character of a
        /// deleted FAT short name), so the name is shown as a guess.
        name_exact: bool,
    },
    /// Carved from raw data. The name was generated from embedded metadata (capture time and
    /// camera model) when available, or is a sequential placeholder.
    Carved { named_from_metadata: bool },
}

/// A volume (partition) on the scanned drive. The GUI shows the label, or a numbered
/// placeholder when the volume has none.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct VolumeRef {
    /// Zero-based position of the partition on the drive.
    pub index: u32,
    pub label: Option<String>,
}

/// How likely a file's data is still intact, judged from where it lies on the disk before
/// its content is verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum Confidence {
    High,
    Medium,
    /// Some of its space now belongs to other data; the content is probably overwritten.
    Low,
}

/// Where a file's date comes from. The two can disagree: a file copied onto the drive
/// carries the copy time in the file system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum DateSource {
    /// Capture time embedded in the file (EXIF, QuickTime metadata).
    Capture,
    /// Modification time from the file system entry.
    FileSystem,
}

/// Date of a file, as an ISO 8601 local date-time without a time zone
/// (e.g. `2025-08-14T15:30:22`), since neither EXIF nor FAT records one reliably.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileDate {
    pub local: String,
    pub source: DateSource,
}

/// Result of verifying a found file. Updated as verification progresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    Recoverable,
    PartiallyDamaged,
    /// Not verified yet, or verification could not decide.
    Unknown,
    /// A video whose index (`moov`) is missing. Data is present but not playable as is.
    IndexMissing,
    /// Repaired; the repaired copy plays. The original is kept as well.
    Repaired,
}

/// Why a file is hidden from the results by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum HiddenReason {
    /// Thumbnail caches and similar files the user did not take.
    Cache,
    Icon,
    /// Too small to be a photo the user cares about.
    TooSmall,
}

/// A way to repair a broken video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum RepairMethod {
    /// Rebuild the index (`moov`) from a reference video of the same camera.
    RebuildIndex,
    /// Drop the damaged parts (GOPs) and remux the rest without re-encoding.
    CutDamaged,
}

/// A file found by a scan. The data itself stays on the disk until it is recovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FoundFile {
    pub id: FileId,
    pub kind: FileKind,
    pub name: String,
    pub origin: FileOrigin,
    /// Where the data starts on the drive. `None` when the location is unknown, e.g. a
    /// deleted entry whose cluster chain is gone.
    #[ts(type = "number | null")]
    pub offset: Option<u64>,
    #[ts(type = "number")]
    pub size: u64,
    pub date: Option<FileDate>,
    /// Length of a video.
    pub duration_seconds: Option<u32>,
    pub confidence: Confidence,
    pub status: FileStatus,
    pub hidden: Option<HiddenReason>,
    /// Repair offered for this file, if any.
    pub repair: Option<RepairMethod>,
}

/// New verification result for a file that was reported earlier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileUpdate {
    pub id: FileId,
    pub status: FileStatus,
    pub repair: Option<RepairMethod>,
}

/// A carved file turned out to be the same as a file found through the file system.
/// The GUI drops `from` and keeps `into`, which has the original name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileMerge {
    pub from: FileId,
    pub into: FileId,
}

/// Final numbers of a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub scan_id: ScanId,
    pub cancelled: bool,
    pub photos: u32,
    pub videos: u32,
    pub recoverable: u32,
    pub partially_damaged: u32,
    #[ts(type = "number")]
    pub elapsed_seconds: u64,
    pub read_errors: u32,
}

/// Request to copy found files to a destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRequest {
    pub scan_id: ScanId,
    pub files: Vec<FileId>,
    pub destination: DestinationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryProgress {
    pub recovery_id: RecoveryId,
    pub files_done: u32,
    pub files_total: u32,
    #[ts(type = "number")]
    pub bytes_done: u64,
    #[ts(type = "number")]
    pub bytes_total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    pub recovery_id: RecoveryId,
    pub recovered: u32,
    /// Recovered, but some of the data is damaged.
    pub partially_damaged: u32,
    pub failed: u32,
    /// Folder the files were written into, grouped in subfolders by capture date.
    pub folder: String,
}

/// Request to write a repaired copy of a video to a destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepairRequest {
    pub scan_id: ScanId,
    pub file: FileId,
    pub method: RepairMethod,
    pub destination: DestinationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepairReport {
    pub file: FileId,
    pub succeeded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FilesFound {
    pub scan_id: ScanId,
    pub files: Vec<FoundFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FilesUpdated {
    pub scan_id: ScanId,
    pub updates: Vec<FileUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct FilesMerged {
    pub scan_id: ScanId,
    pub merges: Vec<FileMerge>,
}

/// Everything the helper reports on its own initiative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HelperEvent {
    ScanProgress(ScanProgress),
    FilesFound(FilesFound),
    FilesUpdated(FilesUpdated),
    FilesMerged(FilesMerged),
    ScanFinished(ScanSummary),
    RecoveryProgress(RecoveryProgress),
    RecoveryFinished(RecoveryReport),
    RepairFinished(RepairReport),
}

/// Errors returned by helper requests. The GUI shows a localized message per variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(
    tag = "code",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HelperError {
    #[error("the helper is not available: {detail}")]
    HelperUnavailable { detail: String },
    #[error("drive not found")]
    DriveNotFound,
    #[error("destination not found")]
    DestinationNotFound,
    #[error("destination is unavailable: {reason:?}")]
    DestinationBlocked { reason: DestinationBlock },
    #[error("no scan with this id")]
    ScanNotFound,
    #[error("the scan has already ended")]
    ScanEnded,
    #[error("file not found")]
    FileNotFound,
    #[error("no files were requested")]
    NothingRequested,
    #[error("a recovery is already running")]
    RecoveryInProgress,
    #[error("internal error: {detail}")]
    Internal { detail: String },
}

/// All types the GUI needs bindings for. Exporting these exports their dependencies too.
pub fn export_typescript(dir: &std::path::Path) -> Result<(), ts_rs::ExportError> {
    let cfg = ts_rs::Config::new().with_out_dir(dir);
    Drive::export_all(&cfg)?;
    Destination::export_all(&cfg)?;
    HelperEvent::export_all(&cfg)?;
    HelperError::export_all(&cfg)?;
    RecoveryRequest::export_all(&cfg)?;
    RepairRequest::export_all(&cfg)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_internally_tagged_camel_case() {
        let ev = HelperEvent::FilesMerged(FilesMerged {
            scan_id: ScanId(1),
            merges: vec![FileMerge {
                from: FileId(2),
                into: FileId(3),
            }],
        });
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(
            json,
            r#"{"type":"filesMerged","scanId":1,"merges":[{"from":2,"into":3}]}"#
        );
    }

    #[test]
    fn origin_fields_are_camel_case() {
        let origin = FileOrigin::FileSystem {
            volume: VolumeRef {
                index: 0,
                label: None,
            },
            folders: vec!["DCIM".into()],
            name_exact: false,
        };
        let json = serde_json::to_string(&origin).unwrap();
        assert_eq!(
            json,
            r#"{"type":"fileSystem","volume":{"index":0,"label":null},"folders":["DCIM"],"nameExact":false}"#
        );
    }

    #[test]
    fn errors_carry_a_code() {
        let err = HelperError::DestinationBlocked {
            reason: DestinationBlock::SameDeviceAsSource,
        };
        let json = serde_json::to_string(&err).unwrap();
        assert_eq!(
            json,
            r#"{"code":"destinationBlocked","reason":"sameDeviceAsSource"}"#
        );
    }
}
