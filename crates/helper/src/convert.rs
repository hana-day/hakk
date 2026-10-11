//! Translation from the scan pipeline's and index's types to the GUI protocol.

use hakk_protocol::{
    Confidence, DateSource, DiskMap, FileDate, FileId, FileKind, FileOrigin, FileStatus, FoundFile,
    HiddenReason, MapCell, RetryProgress, ScanId, ScanProgress, ScanStage, ScanState, VolumeRef,
};
use index::{FileRecord, Media, Origin, Status};
use scan::{CellState, Phase, Progress, RunState};

/// Photos smaller than this are thumbnails, icons and the like.
const SMALL_PHOTO_BYTES: u64 = 20_000;
/// Photos whose longer side is shorter than this are hidden by default.
const SMALL_PHOTO_PIXELS: u32 = 200;

// The read rate is a non-negative float far below u64::MAX; the cast only drops the fraction.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn progress(scan_id: ScanId, p: &Progress) -> ScanProgress {
    let state = match p.state {
        RunState::Running => ScanState::Running,
        RunState::Paused => ScanState::Paused,
        RunState::PausedForRestore => ScanState::PausedForRecovery,
    };
    let stage = match p.phase {
        Phase::Metadata => ScanStage::FileSystem,
        Phase::Retrying { .. } => ScanStage::Retrying,
        Phase::Scanning | Phase::Finishing | Phase::Done => ScanStage::Carving,
    };
    let cells: Vec<MapCell> = p.map.cells.iter().map(|c| map_cell(*c)).collect();
    let current = (p.map.cell_bytes > 0)
        .then(|| p.position / p.map.cell_bytes)
        .and_then(|c| u32::try_from(c).ok())
        .filter(|c| (*c as usize) < cells.len());
    ScanProgress {
        scan_id,
        state,
        stage,
        bytes_read: p.scanned_bytes,
        total_bytes: p.planned_bytes,
        bytes_per_second: p.bytes_per_sec.map_or(0, |r| r.max(0.0) as u64),
        remaining_seconds: p.remaining.map(|d| d.as_secs()),
        photos_found: saturate(p.found.photos),
        videos_found: saturate(p.found.videos),
        read_errors: error_cells(&cells),
        retry: p.retry.map(|r| RetryProgress {
            pass: r.pass,
            passes: r.passes,
            bytes_left: r.bytes_left,
        }),
        disk_map: DiskMap { cells, current },
    }
}

/// Number of disk map cells with read errors, which the GUI reports as error areas.
pub(crate) fn error_cells(cells: &[MapCell]) -> u32 {
    saturate(cells.iter().filter(|c| **c == MapCell::Error).count() as u64)
}

fn map_cell(c: CellState) -> MapCell {
    match c {
        CellState::Skipped => MapCell::Skipped,
        CellState::Unread => MapCell::Unread,
        CellState::Read => MapCell::Read,
        CellState::Found => MapCell::Found,
        CellState::Error => MapCell::Error,
    }
}

fn saturate(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

pub(crate) fn status(s: Status) -> FileStatus {
    match s {
        Status::Unknown => FileStatus::Unknown,
        Status::Recoverable => FileStatus::Recoverable,
        Status::Damaged => FileStatus::PartiallyDamaged,
        Status::MissingIndex => FileStatus::IndexMissing,
        Status::Repaired => FileStatus::Repaired,
    }
}

/// A found file as the GUI sees it. `folders` is the record's folder path (below the
/// volume root), looked up by the caller.
pub(crate) fn found_file(r: &FileRecord, folders: Vec<String>) -> FoundFile {
    let kind = match r.kind.media() {
        Media::Photo => FileKind::Photo,
        Media::Video => FileKind::Video,
    };
    let (name, origin) = match r.origin {
        Origin::FileSystem => (
            r.name.clone().unwrap_or_else(|| placeholder_name(r)),
            FileOrigin::FileSystem {
                volume: r.volume.as_ref().map_or(
                    VolumeRef {
                        index: 0,
                        label: None,
                    },
                    |v| VolumeRef {
                        index: v.index,
                        label: v.label.clone(),
                    },
                ),
                folders,
                name_exact: r.name_exact,
            },
        ),
        Origin::Carving => {
            let from_metadata = metadata_name(r);
            let named = from_metadata.is_some();
            (
                from_metadata.unwrap_or_else(|| placeholder_name(r)),
                FileOrigin::Carved {
                    named_from_metadata: named,
                },
            )
        }
    };
    let date = match (&r.preview.taken_at, &r.preview.modified_at) {
        (Some(t), _) => Some(FileDate {
            local: t.clone(),
            source: DateSource::Capture,
        }),
        (None, Some(m)) => Some(FileDate {
            local: m.clone(),
            source: DateSource::FileSystem,
        }),
        (None, None) => None,
    };
    FoundFile {
        id: FileId(u64::try_from(r.id).unwrap_or(0)),
        kind,
        name,
        origin,
        offset: r.offset,
        size: r.size,
        date,
        duration_seconds: r
            .preview
            .duration_ms
            .map(|ms| saturate(ms.saturating_add(500) / 1000)),
        confidence: r.confidence.map(|c| match c {
            index::Confidence::Low => Confidence::Low,
            index::Confidence::Medium => Confidence::Medium,
            index::Confidence::High => Confidence::High,
        }),
        status: status(r.status),
        hidden: hidden(r, kind),
        // Video repair needs the `video` crate, which does not exist yet.
        repair: None,
    }
}

fn hidden(r: &FileRecord, kind: FileKind) -> Option<HiddenReason> {
    if kind != FileKind::Photo {
        return None;
    }
    let small_dims = match (r.preview.width, r.preview.height) {
        (Some(w), Some(h)) => w.max(h) < SMALL_PHOTO_PIXELS,
        _ => false,
    };
    (r.size < SMALL_PHOTO_BYTES || small_dims).then_some(HiddenReason::TooSmall)
}

/// `2025-08-14_153022_EOS-R6.jpg` from the capture time and camera model, when known.
fn metadata_name(r: &FileRecord) -> Option<String> {
    let taken = r.preview.taken_at.as_deref()?;
    let date = taken.get(..10)?;
    let time: String = taken.get(11..19)?.chars().filter(|c| *c != ':').collect();
    let camera = r
        .preview
        .camera
        .as_deref()
        .map(sanitize)
        .filter(|c| !c.is_empty());
    let ext = r.kind.code();
    Some(match camera {
        Some(camera) => format!("{date}_{time}_{camera}.{ext}"),
        None => format!("{date}_{time}.{ext}"),
    })
}

/// `photo_000184.jpg`: carved files without usable metadata are numbered by id.
fn placeholder_name(r: &FileRecord) -> String {
    let stem = match r.kind.media() {
        Media::Photo => "photo",
        Media::Video => "video",
    };
    format!("{stem}_{:06}.{}", r.id, r.kind.code())
}

/// Keeps a camera model usable in a file name: letters, digits, `-` and `_`.
fn sanitize(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use index::{Preview, Volume};

    fn record(origin: Origin) -> FileRecord {
        FileRecord {
            id: 184,
            origin,
            kind: index::FileKind::Jpeg,
            status: Status::Damaged,
            offset: Some(4096),
            size: 3_000_000,
            name: None,
            volume: None,
            folder_id: None,
            name_exact: true,
            fs_ref: None,
            deleted: true,
            confidence: None,
            merged_into: None,
            rev: 0,
            preview: Preview::default(),
        }
    }

    #[test]
    fn carved_names_come_from_metadata_or_the_id() {
        let mut r = record(Origin::Carving);
        assert_eq!(found_file(&r, vec![]).name, "photo_000184.jpg");
        r.preview.taken_at = Some("2025-08-14T15:30:22".into());
        r.preview.camera = Some("Canon EOS R6".into());
        let f = found_file(&r, vec![]);
        assert_eq!(f.name, "2025-08-14_153022_Canon-EOS-R6.jpg");
        assert_eq!(
            f.origin,
            FileOrigin::Carved {
                named_from_metadata: true
            }
        );
        assert_eq!(f.status, FileStatus::PartiallyDamaged);
        assert_eq!(f.date.map(|d| d.source), Some(DateSource::Capture));
    }

    #[test]
    fn file_system_records_keep_names_folders_and_dates() {
        let mut r = record(Origin::FileSystem);
        r.name = Some("IMG_0001.JPG".into());
        r.name_exact = false;
        r.volume = Some(Volume {
            index: 1,
            label: Some("EOS_DIGITAL".into()),
        });
        r.preview.modified_at = Some("2025-08-14T15:30:22".into());
        r.confidence = Some(index::Confidence::Low);
        let f = found_file(&r, vec!["DCIM".into()]);
        assert_eq!(f.name, "IMG_0001.JPG");
        assert_eq!(
            f.origin,
            FileOrigin::FileSystem {
                volume: VolumeRef {
                    index: 1,
                    label: Some("EOS_DIGITAL".into())
                },
                folders: vec!["DCIM".into()],
                name_exact: false,
            }
        );
        assert_eq!(f.date.map(|d| d.source), Some(DateSource::FileSystem));
        assert_eq!(f.confidence, Some(Confidence::Low));
    }

    #[test]
    fn tiny_photos_are_hidden() {
        let mut r = record(Origin::Carving);
        r.size = 4_000;
        assert_eq!(found_file(&r, vec![]).hidden, Some(HiddenReason::TooSmall));
    }
}
