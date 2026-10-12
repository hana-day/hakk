//! File system analysis as an analyzer.

use std::{ops::Range, sync::Mutex};

use fs::{Entry, FsKind, Timestamp};
use index::{Confidence, FileKind, NewFile, Origin, Preview, Status, Volume};

use crate::{Analyzer, Block, Context};

/// Reads every partition's file system metadata before the main pass and reports the
/// photos and videos it lists (live and deleted) with their names, folders and placement.
///
/// The work happens in [`Analyzer::start`], so it runs in the scan's metadata phase:
/// named files appear before carving results, and the metadata reads are not interleaved
/// with the main pass. Metadata near the start of the disk is reached by reading the main
/// pass forward, so on a typical card (one volume, metadata first) it costs no seek.
#[derive(Debug, Default)]
pub struct FileSystems {
    volumes: Mutex<Vec<VolumeInfo>>,
}

/// What was found on one partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Zero-based position of the partition on the drive.
    pub index: u32,
    /// Where the partition is on the source.
    pub range: Range<u64>,
    /// `None` if no supported file system was recognized (see `error`).
    pub kind: Option<FsKind>,
    pub label: Option<String>,
    /// Unallocated space (absolute ranges), for a later free-space-only scan.
    pub free: Vec<Range<u64>>,
    /// Problems the parser worked around.
    pub warnings: Vec<String>,
    /// Why the volume could not be analyzed.
    pub error: Option<String>,
}

impl FileSystems {
    pub fn new() -> Self {
        Self::default()
    }

    /// The partitions found so far (complete once the scan's metadata phase is over).
    pub fn volumes(&self) -> Vec<VolumeInfo> {
        self.volumes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn record(&self, info: VolumeInfo) {
        self.volumes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(info);
    }
}

impl Analyzer for FileSystems {
    fn name(&self) -> &str {
        "filesystems"
    }

    fn start(&self, cx: &mut Context<'_>) {
        let partitions = match fs::scan_disk(cx.source()) {
            Ok(p) => p,
            Err(e) => {
                // Not even a partition table or boot sector could be read: carving still
                // runs, so the scan goes on without names.
                self.record(VolumeInfo {
                    index: 0,
                    range: 0..cx.source_len(),
                    kind: None,
                    label: None,
                    free: Vec::new(),
                    warnings: Vec::new(),
                    error: Some(e.to_string()),
                });
                return;
            }
        };
        for (index, (part, volume)) in (0u32..).zip(partitions) {
            let range = part.offset..part.offset.saturating_add(part.len);
            let volume = match volume {
                Ok(v) => v,
                Err(e) => {
                    self.record(VolumeInfo {
                        index,
                        range,
                        kind: None,
                        label: None,
                        free: Vec::new(),
                        warnings: Vec::new(),
                        error: Some(e.to_string()),
                    });
                    continue;
                }
            };
            let vref = Volume {
                index,
                label: volume.label.clone(),
            };
            for entry in &volume.entries {
                if let Some(file) = to_new_file(entry, &vref) {
                    cx.report(file);
                }
            }
            self.record(VolumeInfo {
                index,
                range,
                kind: Some(volume.kind),
                label: volume.label,
                free: volume.free.iter().map(|x| x.offset..x.end()).collect(),
                warnings: volume.warnings,
                error: None,
            });
        }
    }

    fn block(&self, _block: &Block, _cx: &mut Context<'_>) {}
}

/// A photo or video entry as an index record; `None` for directories and other files.
fn to_new_file(e: &Entry, volume: &Volume) -> Option<NewFile> {
    if e.is_dir {
        return None;
    }
    let kind = kind_from_name(&e.name)?;
    let folder = e
        .path
        .rsplit_once('/')
        .map(|(dir, _)| dir.split('/').map(str::to_owned).collect())
        .unwrap_or_default();
    Some(NewFile {
        origin: Origin::FileSystem,
        kind,
        status: Status::Unknown,
        extents: e.extents.iter().map(|x| x.offset..x.end()).collect(),
        size: e.size,
        name: Some(e.name.clone()),
        volume: Some(volume.clone()),
        folder,
        name_exact: e.name_exact,
        // Unique per volume: a deleted entry may share its path with a live one.
        fs_ref: Some(format!(
            "v{}:{}:{}:{}",
            volume.index,
            if e.deleted { "deleted" } else { "live" },
            e.first_cluster,
            e.path
        )),
        deleted: e.deleted,
        confidence: Some(match e.confidence {
            fs::Confidence::High => Confidence::High,
            fs::Confidence::Medium => Confidence::Medium,
            fs::Confidence::Low => Confidence::Low,
        }),
        preview: Preview {
            created_at: e.created.map(local_datetime),
            modified_at: e.modified.map(local_datetime),
            ..Preview::default()
        },
    })
}

/// The format the index stores dates in (ISO 8601 local date-time, no zone).
fn local_datetime(t: Timestamp) -> String {
    let base = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    if t.millis == 0 {
        base
    } else {
        format!("{base}.{:03}", t.millis)
    }
}

/// Photo and video formats by file name extension.
fn kind_from_name(name: &str) -> Option<FileKind> {
    let (_, ext) = name.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "jpe" => FileKind::Jpeg,
        "png" => FileKind::Png,
        "gif" => FileKind::Gif,
        "bmp" => FileKind::Bmp,
        "tif" | "tiff" => FileKind::Tiff,
        "heic" | "heif" => FileKind::Heic,
        "webp" => FileKind::Webp,
        "cr2" => FileKind::Cr2,
        "cr3" => FileKind::Cr3,
        "nef" => FileKind::Nef,
        "arw" => FileKind::Arw,
        "dng" => FileKind::Dng,
        "orf" => FileKind::Orf,
        "rw2" => FileKind::Rw2,
        "pef" => FileKind::Pef,
        "raf" => FileKind::Raf,
        "mp4" | "m4v" => FileKind::Mp4,
        "mov" => FileKind::Mov,
        "avi" => FileKind::Avi,
        "mts" | "m2ts" => FileKind::Mts,
        "3gp" => FileKind::ThreeGp,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_from_name("IMG_0001.JPG"), Some(FileKind::Jpeg));
        assert_eq!(kind_from_name("clip.MoV"), Some(FileKind::Mov));
        assert_eq!(kind_from_name("notes.txt"), None);
        assert_eq!(kind_from_name("README"), None);
    }

    #[test]
    fn dates_match_the_index_format() {
        let t = Timestamp {
            year: 2025,
            month: 8,
            day: 14,
            hour: 15,
            minute: 30,
            second: 22,
            millis: 0,
            utc_offset_minutes: Some(540),
        };
        assert_eq!(local_datetime(t), "2025-08-14T15:30:22");
        assert!(index::is_local_datetime(&local_datetime(Timestamp {
            millis: 50,
            ..t
        })));
    }
}
