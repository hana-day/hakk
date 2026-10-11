//! The data model: what a scan finds and how it is stored.

use std::{fmt, ops::Range, str::FromStr};

/// Where a file record came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// File system metadata (has a name and a folder).
    FileSystem,
    /// Carving (found by its byte signature; no name).
    Carving,
}

impl Origin {
    pub(crate) fn code(self) -> i64 {
        match self {
            Origin::FileSystem => 1,
            Origin::Carving => 2,
        }
    }

    pub(crate) fn from_code(code: i64) -> Option<Self> {
        match code {
            1 => Some(Origin::FileSystem),
            2 => Some(Origin::Carving),
            _ => None,
        }
    }
}

/// Broad category, used for counts and the "photos" / "videos" virtual folders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Media {
    Photo,
    Video,
}

/// File format.
///
/// Stored as a stable text code ([`FileKind::code`]); never renumber or rename codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileKind {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Tiff,
    Heic,
    Webp,
    Cr2,
    Cr3,
    Nef,
    Arw,
    Dng,
    Mp4,
    Mov,
    Avi,
    Mts,
    ThreeGp,
}

impl FileKind {
    const ALL: [FileKind; 17] = [
        FileKind::Jpeg,
        FileKind::Png,
        FileKind::Gif,
        FileKind::Bmp,
        FileKind::Tiff,
        FileKind::Heic,
        FileKind::Webp,
        FileKind::Cr2,
        FileKind::Cr3,
        FileKind::Nef,
        FileKind::Arw,
        FileKind::Dng,
        FileKind::Mp4,
        FileKind::Mov,
        FileKind::Avi,
        FileKind::Mts,
        FileKind::ThreeGp,
    ];

    pub fn all() -> impl Iterator<Item = FileKind> {
        Self::ALL.into_iter()
    }

    /// The stable code stored in the database. Also usable as a file extension.
    pub fn code(self) -> &'static str {
        match self {
            FileKind::Jpeg => "jpg",
            FileKind::Png => "png",
            FileKind::Gif => "gif",
            FileKind::Bmp => "bmp",
            FileKind::Tiff => "tif",
            FileKind::Heic => "heic",
            FileKind::Webp => "webp",
            FileKind::Cr2 => "cr2",
            FileKind::Cr3 => "cr3",
            FileKind::Nef => "nef",
            FileKind::Arw => "arw",
            FileKind::Dng => "dng",
            FileKind::Mp4 => "mp4",
            FileKind::Mov => "mov",
            FileKind::Avi => "avi",
            FileKind::Mts => "mts",
            FileKind::ThreeGp => "3gp",
        }
    }

    pub fn media(self) -> Media {
        match self {
            FileKind::Mp4 | FileKind::Mov | FileKind::Avi | FileKind::Mts | FileKind::ThreeGp => {
                Media::Video
            }
            _ => Media::Photo,
        }
    }
}

impl fmt::Display for FileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// The text was not a known [`FileKind`] code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKind(pub String);

impl fmt::Display for UnknownKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown file kind: {:?}", self.0)
    }
}

impl std::error::Error for UnknownKind {}

impl FromStr for FileKind {
    type Err = UnknownKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        FileKind::ALL
            .into_iter()
            .find(|k| k.code() == s)
            .ok_or_else(|| UnknownKind(s.to_owned()))
    }
}

/// How recoverable a file looks. Updated as verification progresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// Not verified yet.
    Unknown,
    /// Verified to be intact.
    Recoverable,
    /// Some of the data is missing or damaged.
    Damaged,
    /// The data looks present but the index the player needs (e.g. MP4 `moov`) is missing.
    MissingIndex,
    /// Repaired; the repaired copy plays. The original stays on the disk untouched.
    Repaired,
}

impl Status {
    pub(crate) fn code(self) -> i64 {
        match self {
            Status::Unknown => 0,
            Status::Recoverable => 1,
            Status::Damaged => 2,
            Status::MissingIndex => 3,
            Status::Repaired => 4,
        }
    }

    pub(crate) fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Status::Unknown),
            1 => Some(Status::Recoverable),
            2 => Some(Status::Damaged),
            3 => Some(Status::MissingIndex),
            4 => Some(Status::Repaired),
            _ => None,
        }
    }
}

/// How likely a file's data is still intact, judged from its placement before the
/// content is verified (file system analysis).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Confidence {
    /// Some of its space now belongs to other data, or the placement is incomplete.
    Low,
    /// The placement is inferred (e.g. assumed contiguous) and nothing reuses it.
    Medium,
    /// The placement is known and nothing reuses it.
    High,
}

impl Confidence {
    pub(crate) fn code(self) -> i64 {
        match self {
            Confidence::Low => 0,
            Confidence::Medium => 1,
            Confidence::High => 2,
        }
    }

    pub(crate) fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Confidence::Low),
            1 => Some(Confidence::Medium),
            2 => Some(Confidence::High),
            _ => None,
        }
    }
}

/// A partition (volume) on the scanned drive.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Volume {
    /// Zero-based position of the partition on the drive.
    pub index: u32,
    pub label: Option<String>,
}

/// Whether `s` is an ISO 8601 local date-time without a time zone, as stored for dates:
/// `YYYY-MM-DDTHH:MM:SS`, optionally followed by `.` and 1-9 digits of fractional seconds.
pub fn is_local_datetime(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| {
        b.get(r)
            .is_some_and(|d| !d.is_empty() && d.iter().all(u8::is_ascii_digit))
    };
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    let base = digits(0..4)
        && at(4, b'-')
        && digits(5..7)
        && at(7, b'-')
        && digits(8..10)
        && at(10, b'T')
        && digits(11..13)
        && at(13, b':')
        && digits(14..16)
        && at(16, b':')
        && digits(17..19);
    match b.len() {
        19 => base,
        n if (21..=29).contains(&n) => base && at(19, b'.') && digits(20..n),
        _ => false,
    }
}

/// Information for listing and previewing a file without reading it again.
/// Every field is optional; analyzers fill in what they know.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    /// Capture time embedded in the file (EXIF, QuickTime metadata), as an ISO 8601 local
    /// date-time without a time zone (see [`is_local_datetime`]). Kept as recorded: EXIF
    /// and FAT do not reliably say which zone they are in.
    pub taken_at: Option<String>,
    /// Camera model (e.g. EXIF `Model`).
    pub camera: Option<String>,
    /// An embedded thumbnail (e.g. the EXIF thumbnail), as a byte range on the source.
    pub thumbnail: Option<Range<u64>>,
    /// File system timestamps, in the same format as `taken_at`.
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
}

/// A file found by the scan, before it is stored.
///
/// Only the location is recorded; the data itself is never copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFile {
    pub origin: Origin,
    pub kind: FileKind,
    pub status: Status,
    /// Where the data is on the source, in file order. Carving yields a single extent;
    /// file system analysis may yield several (fragmented files), or none when the
    /// location is lost (e.g. a deleted entry whose first cluster is unknown).
    pub extents: Vec<Range<u64>>,
    /// Logical file size. May be less than the extents' total (the last cluster is
    /// usually only partly used).
    pub size: u64,
    /// File name (file system only).
    pub name: Option<String>,
    /// The partition the file system entry belongs to (file system only).
    pub volume: Option<Volume>,
    /// Folder path components below the volume root (file system only). Empty means
    /// the root.
    pub folder: Vec<String>,
    /// `false` when part of the name was lost and guessed (the first character of a
    /// deleted FAT short name).
    pub name_exact: bool,
    /// File system identity, unique per scanned file system (e.g. `"exfat:entry:0x1a2b00"`,
    /// `"ntfs:mft:1234"`). Re-reporting the same identity does not create a second record.
    pub fs_ref: Option<String>,
    /// Whether the file system marks the file as deleted (file system only).
    pub deleted: bool,
    /// Placement confidence from file system analysis; `None` if not judged.
    pub confidence: Option<Confidence>,
    pub preview: Preview,
}

impl NewFile {
    /// A carving result: one extent, unknown status.
    pub fn carved(kind: FileKind, offset: u64, len: u64) -> Self {
        Self {
            origin: Origin::Carving,
            kind,
            status: Status::Unknown,
            extents: std::iter::once(offset..offset.saturating_add(len)).collect(),
            size: len,
            name: None,
            volume: None,
            folder: Vec::new(),
            name_exact: true,
            fs_ref: None,
            deleted: true,
            confidence: None,
            preview: Preview::default(),
        }
    }

    /// Start of the first extent.
    pub fn offset(&self) -> Option<u64> {
        self.extents.first().map(|e| e.start)
    }
}

/// A stored file record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    /// Increases in the order files were found, so "files found since id N" is a cheap query.
    pub id: i64,
    pub origin: Origin,
    pub kind: FileKind,
    pub status: Status,
    /// Start of the first extent; `None` when the location is unknown.
    pub offset: Option<u64>,
    pub size: u64,
    pub name: Option<String>,
    pub volume: Option<Volume>,
    /// Folder id (see [`crate::Index::folder_path`]); `None` for carving results and
    /// files in a volume's root.
    pub folder_id: Option<i64>,
    pub name_exact: bool,
    pub fs_ref: Option<String>,
    pub deleted: bool,
    pub confidence: Option<Confidence>,
    /// Set when this record was found to be the same file as another one (e.g. a carving
    /// result matching a named file system record). Merged records are hidden from listings.
    pub merged_into: Option<i64>,
    /// Revision of the last change to this record, from a counter shared by the whole
    /// index. To follow changes, remember the largest `rev` seen and query
    /// [`crate::Query::changed_after`] with it.
    pub rev: i64,
    pub preview: Preview,
}

/// A saved scan: what it covers and how far it got. Lets a scan resume after the app
/// is closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanState {
    pub source: SourceInfo,
    /// Byte ranges the scan is meant to read (the whole disk, or only free space).
    /// Sorted and non-overlapping.
    pub planned: Vec<Range<u64>>,
    pub progress: ScanProgress,
}

/// How far a scan got. Saved at every checkpoint, together with the files found so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanProgress {
    /// Ranges fully read and analyzed. Sorted and non-overlapping.
    pub done: Vec<Range<u64>>,
    /// Ranges that could not be read (still bad after the retries so far).
    /// Sorted and non-overlapping.
    pub bad: Vec<BadRange>,
    pub phase: ScanPhase,
    /// How many retry passes over the bad ranges have completed.
    pub retry_passes: u32,
}

/// An unreadable range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadRange {
    pub range: Range<u64>,
    /// How many times reading it has failed.
    pub attempts: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScanPhase {
    /// Reading the planned ranges in order.
    #[default]
    Scanning,
    /// Retrying the bad ranges.
    Retrying,
    /// Finished. Bad ranges may remain.
    Complete,
}

impl ScanPhase {
    pub(crate) fn code(self) -> i64 {
        match self {
            ScanPhase::Scanning => 0,
            ScanPhase::Retrying => 1,
            ScanPhase::Complete => 2,
        }
    }

    pub(crate) fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(ScanPhase::Scanning),
            1 => Some(ScanPhase::Retrying),
            2 => Some(ScanPhase::Complete),
            _ => None,
        }
    }
}

/// Identity of the scanned source, checked when resuming so an index is never applied to
/// a different disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub len: u64,
    pub sector_size: u32,
    /// Human-readable description (model, serial, volume label...). Informational only.
    pub label: String,
}

/// Number of (unmerged) files per category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub photos: u64,
    pub videos: u64,
    pub recoverable: u64,
    pub damaged: u64,
    pub missing_index: u64,
    pub repaired: u64,
    pub unknown: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.photos + self.videos
    }
}
