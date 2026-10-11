//! Fake drives, destinations and the files a scan of each drive "finds".
//!
//! Everything is generated from a fixed seed, so the mock behaves the same on every run.

// Fake data is built from small numbers; truncating casts cannot lose anything here.
#![allow(clippy::cast_possible_truncation)]

use hakk_protocol::{
    Confidence, DateSource, Destination, DestinationBlock, DestinationId, Drive, DriveId,
    DriveKind, DriveWarning, FileDate, FileKind, FileOrigin, FileStatus, HiddenReason,
    RepairMethod, VolumeRef,
};

const GB: u64 = 1_000_000_000;
/// Number of disk regions carved files are found in.
const CARVED_REGIONS: u64 = 7;
const MB: u64 = 1_000_000;

/// A drive plus what the mock needs to simulate scanning it.
pub struct MockDrive {
    pub drive: Drive,
    /// Physical device the drive lives on; destinations on the same device are blocked.
    pub device: &'static str,
    /// Read speed the simulated scan reports.
    pub bytes_per_second: u64,
    spec: DatasetSpec,
}

struct MockDestination {
    id: &'static str,
    kind: DriveKind,
    name: &'static str,
    path: &'static str,
    free_bytes: u64,
    device: &'static str,
}

/// What a drive contains.
struct DatasetSpec {
    /// Folders that still have file system entries for deleted files.
    folders: &'static [FolderSpec],
    camera: &'static str,
    carved_photos: u32,
    carved_videos: u32,
    /// Carved files the GUI hides by default (thumbnails, icons, tiny images).
    carved_junk: u32,
    /// Carved copies of file system files, merged into them shortly after being found.
    duplicates: u32,
    /// Days since 1970-01-01 of the first capture date of carved files.
    carved_from_day: i64,
    carved_days: i64,
}

struct FolderSpec {
    path: &'static [&'static str],
    photos: u32,
    videos: u32,
    photo_name: fn(u32) -> String,
    video_name: fn(u32) -> String,
    /// Days since 1970-01-01 of the first capture date in this folder.
    from_day: i64,
    days: i64,
}

// 2025-06-01, 2025-08-01, 2024-03-01 and 2025-01-01 as days since 1970-01-01.
const JUN_1_2025: i64 = 20_240;
const AUG_1_2025: i64 = 20_301;
const MAR_1_2024: i64 = 19_783;
const JAN_1_2025: i64 = 20_089;

fn canon_photo(n: u32) -> String {
    format!("IMG_{:04}.JPG", n % 10_000)
}
fn canon_video(n: u32) -> String {
    format!("MVI_{:04}.MP4", n % 10_000)
}
fn sony_photo(n: u32) -> String {
    format!("DSC{:05}.JPG", n % 100_000)
}
fn sony_video(n: u32) -> String {
    format!("C{:04}.MP4", n % 10_000)
}
fn phone_photo(n: u32) -> String {
    format!("IMG_{:04}.HEIC", n % 10_000)
}
fn phone_video(n: u32) -> String {
    format!("IMG_{:04}.MOV", n % 10_000)
}

pub fn drives() -> Vec<MockDrive> {
    vec![
        MockDrive {
            drive: Drive {
                id: DriveId("sd".into()),
                kind: DriveKind::SdCard,
                name: "EOS_DIGITAL".into(),
                capacity_bytes: 64 * GB,
                file_system: Some("exFAT".into()),
                warnings: vec![],
            },
            device: "disk4",
            bytes_per_second: 92 * MB,
            spec: DatasetSpec {
                folders: &[
                    FolderSpec {
                        path: &["EOS_DIGITAL", "DCIM", "100CANON"],
                        photos: 1950,
                        videos: 49,
                        photo_name: canon_photo,
                        video_name: canon_video,
                        from_day: JUN_1_2025,
                        days: 60,
                    },
                    FolderSpec {
                        path: &["EOS_DIGITAL", "DCIM", "101CANON"],
                        photos: 771,
                        videos: 28,
                        photo_name: canon_photo,
                        video_name: canon_video,
                        from_day: AUG_1_2025,
                        days: 20,
                    },
                    FolderSpec {
                        path: &["EOS_DIGITAL", "MISC"],
                        photos: 7,
                        videos: 0,
                        photo_name: canon_photo,
                        video_name: canon_video,
                        from_day: JUN_1_2025,
                        days: 80,
                    },
                ],
                camera: "EOS-R6",
                carved_photos: 684,
                carved_videos: 71,
                carved_junk: 60,
                duplicates: 8,
                carved_from_day: JAN_1_2025,
                carved_days: 230,
            },
        },
        MockDrive {
            drive: Drive {
                id: DriveId("hdd".into()),
                kind: DriveKind::ExternalHdd,
                name: "WD Elements".into(),
                capacity_bytes: 2_000 * GB,
                file_system: Some("NTFS".into()),
                warnings: vec![],
            },
            device: "disk5",
            bytes_per_second: 160 * MB,
            spec: DatasetSpec {
                folders: &[
                    FolderSpec {
                        path: &["WD Elements", "Pictures", "2024"],
                        photos: 3100,
                        videos: 0,
                        photo_name: sony_photo,
                        video_name: sony_video,
                        from_day: MAR_1_2024,
                        days: 300,
                    },
                    FolderSpec {
                        path: &["WD Elements", "Pictures", "2025"],
                        photos: 2400,
                        videos: 0,
                        photo_name: sony_photo,
                        video_name: sony_video,
                        from_day: JAN_1_2025,
                        days: 220,
                    },
                    FolderSpec {
                        path: &["WD Elements", "Videos"],
                        photos: 0,
                        videos: 210,
                        photo_name: sony_photo,
                        video_name: sony_video,
                        from_day: MAR_1_2024,
                        days: 520,
                    },
                ],
                camera: "ILCE-7M4",
                carved_photos: 1500,
                carved_videos: 160,
                carved_junk: 300,
                duplicates: 20,
                carved_from_day: MAR_1_2024,
                carved_days: 520,
            },
        },
        MockDrive {
            drive: Drive {
                id: DriveId("internal".into()),
                kind: DriveKind::InternalSsd,
                name: "Macintosh HD".into(),
                capacity_bytes: 1_000 * GB,
                file_system: Some("APFS".into()),
                warnings: vec![DriveWarning::TrimLikely],
            },
            device: "disk1",
            bytes_per_second: 1_800 * MB,
            spec: DatasetSpec {
                folders: &[FolderSpec {
                    path: &["Macintosh HD", "Users", "Pictures"],
                    photos: 40,
                    videos: 2,
                    photo_name: phone_photo,
                    video_name: phone_video,
                    from_day: JUN_1_2025,
                    days: 90,
                }],
                camera: "iPhone-15",
                carved_photos: 23,
                carved_videos: 2,
                carved_junk: 80,
                duplicates: 0,
                carved_from_day: JAN_1_2025,
                carved_days: 230,
            },
        },
    ]
}

const DESTINATIONS: &[MockDestination] = &[
    MockDestination {
        id: "backup",
        kind: DriveKind::ExternalSsd,
        name: "BACKUP",
        path: "/Volumes/BACKUP/hakk",
        free_bytes: 412 * GB,
        device: "disk6",
    },
    MockDestination {
        id: "internal",
        kind: DriveKind::InternalSsd,
        name: "Macintosh HD",
        path: "/Users/me/Pictures/hakk",
        free_bytes: 180 * GB,
        device: "disk1",
    },
    MockDestination {
        id: "hdd",
        kind: DriveKind::ExternalHdd,
        name: "WD Elements",
        path: "/Volumes/WD Elements/hakk",
        free_bytes: 820 * GB,
        device: "disk5",
    },
    MockDestination {
        id: "sd",
        kind: DriveKind::SdCard,
        name: "EOS_DIGITAL",
        path: "/Volumes/EOS_DIGITAL/hakk",
        free_bytes: 21 * GB,
        device: "disk4",
    },
];

/// Destinations as seen while `source_device` is being scanned.
pub fn destinations(source_device: &str) -> Vec<Destination> {
    DESTINATIONS
        .iter()
        .map(|d| Destination {
            id: DestinationId(d.id.into()),
            kind: d.kind,
            name: d.name.into(),
            path: d.path.into(),
            free_bytes: d.free_bytes,
            blocked: (d.device == source_device).then_some(DestinationBlock::SameDeviceAsSource),
        })
        .collect()
}

/// When a planned file shows up during the scan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Reveal {
    /// During the file system stage, at this fraction (0..1) of the stage.
    FileSystem(f64),
    /// During carving, when the read position passes the file's offset.
    Carving,
}

/// A file the simulated scan will find, with the verification result it will get.
#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub kind: FileKind,
    pub name: String,
    pub origin: FileOrigin,
    pub offset: u64,
    pub size: u64,
    pub date: Option<FileDate>,
    pub duration_seconds: Option<u32>,
    pub confidence: Option<Confidence>,
    pub hidden: Option<HiddenReason>,
    pub reveal: Reveal,
    /// Status after verification.
    pub status: FileStatus,
    pub repair: Option<RepairMethod>,
    /// Seconds between being found and being verified.
    pub verify_delay: f64,
    /// For carved duplicates: index (into the plan) of the file system file it duplicates.
    pub duplicate_of: Option<usize>,
}

/// Plans every file a scan of `drive` finds. File system files come first (in reveal order),
/// then carved files sorted by offset.
pub fn plan(drive: &MockDrive) -> Vec<PlannedFile> {
    let spec = &drive.spec;
    let total = drive.drive.capacity_bytes;
    let mut rng = Rng(0x5eed_0000 ^ total);
    let mut out = Vec::new();

    // File system files are laid out one after another in the first 70 % of the disk.
    let fs_count: u32 = spec.folders.iter().map(|f| f.photos + f.videos).sum();
    let mut cursor = total / 50;
    let mut fs_index = 0u32;
    for folder in spec.folders {
        let count = folder.photos + folder.videos;
        let mut seconds = 9 * 3600;
        let mut day = folder.from_day;
        for i in 0..count {
            // Spread videos evenly between photos.
            let is_video = (i + 1) * folder.videos / count > i * folder.videos / count;
            let number = 4000 + i;
            let (kind, name, size, duration) = if is_video {
                let duration = rng.range(8, 240) as u32;
                let size = u64::from(duration) * rng.range(900_000, 1_100_000);
                (
                    FileKind::Video,
                    (folder.video_name)(number),
                    size,
                    Some(duration),
                )
            } else {
                let size = rng.range(3 * MB, 7_500_000);
                (FileKind::Photo, (folder.photo_name)(number), size, None)
            };
            // Advance the capture clock; move to a later day in the evening.
            seconds += rng.range(5, 420) as i64;
            if seconds > 19 * 3600 {
                seconds = 9 * 3600 + rng.range(0, 3600) as i64;
                // About 170 shots fit in a day; skip days so the folder spans `days`.
                let max_step = (folder.days as u64 * 170 / u64::from(count)).max(1);
                day = (day + 1 + rng.range(0, max_step) as i64).min(folder.from_day + folder.days);
            }
            let (status, repair) = verdict(&mut rng, kind, false);
            let offset = cursor;
            cursor += size + rng.range(0, 8 * MB);
            if cursor > total * 7 / 10 {
                cursor = total / 50 + rng.range(0, total / 10);
            }
            out.push(PlannedFile {
                kind,
                name,
                origin: FileOrigin::FileSystem {
                    volume: VolumeRef {
                        index: 0,
                        label: folder.path.first().map(|s| (*s).to_owned()),
                    },
                    folders: folder
                        .path
                        .iter()
                        .skip(1)
                        .map(|s| (*s).to_owned())
                        .collect(),
                    // A few deleted short names lost their first letter.
                    name_exact: rng.unit() >= 0.02,
                },
                confidence: Some(confidence_for(status)),
                offset,
                size,
                // Overwritten files have no readable embedded metadata; only the file
                // system date is left.
                date: Some(FileDate {
                    local: datetime(day, seconds),
                    source: if status == FileStatus::PartiallyDamaged {
                        DateSource::FileSystem
                    } else {
                        DateSource::Capture
                    },
                }),
                duration_seconds: duration,
                hidden: None,
                reveal: Reveal::FileSystem(f64::from(fs_index) / f64::from(fs_count.max(1))),
                status,
                repair,
                verify_delay: rng.unit() * 4.0 + 0.5,
                duplicate_of: None,
            });
            fs_index += 1;
        }
    }
    let fs_files = out.len();

    // Deleted data sits in a few regions that were in use before, not evenly over the disk.
    let regions: Vec<(u64, u64)> = (0..CARVED_REGIONS)
        .map(|_| {
            let len = total / 40;
            (rng.range(total / 100, total - len - 64 * MB), len)
        })
        .collect();
    let mut carved = Vec::new();
    let mut photo_seq = 0u32;
    let mut video_seq = 0u32;
    for i in 0..spec.carved_photos + spec.carved_videos + spec.carved_junk {
        let kind = if i < spec.carved_photos || i >= spec.carved_photos + spec.carved_videos {
            FileKind::Photo
        } else {
            FileKind::Video
        };
        let junk = i >= spec.carved_photos + spec.carved_videos;
        let hidden = junk.then(|| match rng.range(0, 3) {
            0 => HiddenReason::Cache,
            1 => HiddenReason::Icon,
            _ => HiddenReason::TooSmall,
        });
        let named = !junk && rng.unit() < 0.75;
        let day = spec.carved_from_day + rng.range(0, spec.carved_days as u64) as i64;
        let seconds = rng.range(8 * 3600, 20 * 3600) as i64;
        let (size, duration) = match (kind, junk) {
            (_, true) => (rng.range(2_000, 40_000), None),
            (FileKind::Photo, false) => (rng.range(2 * MB, 7_500_000), None),
            (FileKind::Video, false) => {
                let duration = rng.range(5, 300) as u32;
                (
                    u64::from(duration) * rng.range(900_000, 1_100_000),
                    Some(duration),
                )
            }
        };
        let ext = match kind {
            FileKind::Photo => "jpg",
            FileKind::Video => "mp4",
        };
        let name = if named {
            format!("{}_{}.{ext}", compact_datetime(day, seconds), spec.camera)
        } else {
            match kind {
                FileKind::Photo => {
                    photo_seq += 1;
                    format!("photo_{photo_seq:06}.{ext}")
                }
                FileKind::Video => {
                    video_seq += 1;
                    format!("video_{video_seq:06}.{ext}")
                }
            }
        };
        let (status, repair) = verdict(&mut rng, kind, true);
        carved.push(PlannedFile {
            kind,
            name,
            origin: FileOrigin::Carved {
                named_from_metadata: named,
            },
            confidence: None,
            offset: {
                let region = regions.get(rng.range(0, CARVED_REGIONS) as usize);
                let (start, len) = region.copied().unwrap_or((total / 100, total / 40));
                start + rng.range(0, len)
            },
            size,
            date: named.then(|| FileDate {
                local: datetime(day, seconds),
                source: DateSource::Capture,
            }),
            duration_seconds: duration,
            hidden,
            reveal: Reveal::Carving,
            status,
            repair,
            verify_delay: rng.unit() * 3.0 + 0.8,
            duplicate_of: None,
        });
    }
    for _ in 0..spec.duplicates {
        if fs_files == 0 {
            break;
        }
        let of = rng.range(0, fs_files as u64) as usize;
        let Some(original) = out.get(of) else {
            continue;
        };
        carved.push(PlannedFile {
            name: match original.kind {
                FileKind::Photo => format!("photo_dup_{of:06}.jpg"),
                FileKind::Video => format!("video_dup_{of:06}.mp4"),
            },
            origin: FileOrigin::Carved {
                named_from_metadata: false,
            },
            reveal: Reveal::Carving,
            verify_delay: 1.5,
            duplicate_of: Some(of),
            date: None,
            ..original.clone()
        });
    }
    carved.sort_by_key(|f| f.offset);
    out.extend(carved);
    out
}

/// Placement confidence consistent with the verification result the file will get.
fn confidence_for(status: FileStatus) -> Confidence {
    match status {
        FileStatus::PartiallyDamaged => Confidence::Low,
        FileStatus::IndexMissing | FileStatus::Unknown => Confidence::Medium,
        FileStatus::Recoverable | FileStatus::Repaired => Confidence::High,
    }
}

/// Verification result for a file; carved files are damaged more often.
fn verdict(rng: &mut Rng, kind: FileKind, carved: bool) -> (FileStatus, Option<RepairMethod>) {
    let r = rng.unit();
    match (kind, carved) {
        (FileKind::Photo, false) if r < 0.04 => (FileStatus::PartiallyDamaged, None),
        (FileKind::Photo, false) => (FileStatus::Recoverable, None),
        (FileKind::Photo, true) if r < 0.10 => (FileStatus::PartiallyDamaged, None),
        (FileKind::Photo, true) if r < 0.15 => (FileStatus::Unknown, None),
        (FileKind::Photo, true) => (FileStatus::Recoverable, None),
        (FileKind::Video, false) if r < 0.08 => {
            (FileStatus::IndexMissing, Some(RepairMethod::RebuildIndex))
        }
        (FileKind::Video, false) if r < 0.15 => {
            (FileStatus::PartiallyDamaged, Some(RepairMethod::CutDamaged))
        }
        (FileKind::Video, false) => (FileStatus::Recoverable, None),
        (FileKind::Video, true) if r < 0.25 => {
            (FileStatus::IndexMissing, Some(RepairMethod::RebuildIndex))
        }
        (FileKind::Video, true) if r < 0.40 => {
            (FileStatus::PartiallyDamaged, Some(RepairMethod::CutDamaged))
        }
        (FileKind::Video, true) if r < 0.45 => (FileStatus::Unknown, None),
        (FileKind::Video, true) => (FileStatus::Recoverable, None),
    }
}

/// Converts days since 1970-01-01 to a civil date (proleptic Gregorian calendar).
///
/// Reference: Howard Hinnant, "chrono-Compatible Low-Level Date Algorithms",
/// `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m as u32, d as u32)
}

/// `2025-08-14T15:30:22`
fn datetime(days: i64, seconds: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}")
}

/// `2025-08-14_153022`, the prefix of names generated from capture metadata.
fn compact_datetime(days: i64, seconds: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    format!("{y:04}-{m:02}-{d:02}_{h:02}{min:02}{s:02}")
}

/// SplitMix64. Small, deterministic and good enough for fake data.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[lo, hi)`; `lo` if the range is empty.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            return lo;
        }
        lo + self.next_u64() % (hi - lo)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(JUN_1_2025), (2025, 6, 1));
        assert_eq!(civil_from_days(AUG_1_2025), (2025, 8, 1));
        assert_eq!(civil_from_days(MAR_1_2024), (2024, 3, 1));
        assert_eq!(civil_from_days(JAN_1_2025), (2025, 1, 1));
    }

    #[test]
    fn sd_card_plan_matches_the_mock_screens() {
        let drives = drives();
        let sd = &drives[0];
        let plan = plan(sd);
        let visible = |kind| {
            plan.iter()
                .filter(|f| f.kind == kind && f.hidden.is_none() && f.duplicate_of.is_none())
                .count()
        };
        assert_eq!(visible(FileKind::Photo), 3412);
        assert_eq!(visible(FileKind::Video), 148);
        assert!(
            plan.iter()
                .all(|f| f.offset + f.size <= sd.drive.capacity_bytes)
        );
    }

    #[test]
    fn source_device_is_blocked() {
        let dests = destinations("disk4");
        let sd = dests.iter().find(|d| d.id.0 == "sd").unwrap();
        assert_eq!(sd.blocked, Some(DestinationBlock::SameDeviceAsSource));
        assert!(
            dests
                .iter()
                .filter(|d| d.id.0 != "sd")
                .all(|d| d.blocked.is_none())
        );
    }
}
