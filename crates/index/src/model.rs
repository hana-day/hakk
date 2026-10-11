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
}

impl Status {
    pub(crate) fn code(self) -> i64 {
        match self {
            Status::Unknown => 0,
            Status::Recoverable => 1,
            Status::Damaged => 2,
            Status::MissingIndex => 3,
        }
    }

    pub(crate) fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => Some(Status::Unknown),
            1 => Some(Status::Recoverable),
            2 => Some(Status::Damaged),
            3 => Some(Status::MissingIndex),
            _ => None,
        }
    }
}

/// Information for listing and previewing a file without reading it again.
/// Every field is optional; analyzers fill in what they know.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preview {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    /// Capture time (EXIF / `mvhd`), seconds since the Unix epoch.
    pub taken_at: Option<i64>,
    /// Camera model (e.g. EXIF `Model`).
    pub camera: Option<String>,
    /// An embedded thumbnail (e.g. the EXIF thumbnail), as a byte range on the source.
    pub thumbnail: Option<Range<u64>>,
    /// File system timestamps, seconds since the Unix epoch.
    pub created_at: Option<i64>,
    pub modified_at: Option<i64>,
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
    /// file system analysis may yield several (fragmented files). Must not be empty.
    pub extents: Vec<Range<u64>>,
    /// Logical file size. May be less than the extents' total (the last cluster is
    /// usually only partly used).
    pub size: u64,
    /// File name (file system only).
    pub name: Option<String>,
    /// Folder path components from the root (file system only). Empty means the root.
    pub folder: Vec<String>,
    /// File system identity, unique per scanned file system (e.g. `"exfat:entry:0x1a2b00"`,
    /// `"ntfs:mft:1234"`). Re-reporting the same identity does not create a second record.
    pub fs_ref: Option<String>,
    /// Whether the file system marks the file as deleted (file system only).
    pub deleted: bool,
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
            folder: Vec::new(),
            fs_ref: None,
            deleted: true,
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
    /// Start of the first extent.
    pub offset: u64,
    pub size: u64,
    pub name: Option<String>,
    /// Folder id (see [`crate::Index::folder_path`]); `None` for carving results.
    pub folder_id: Option<i64>,
    pub fs_ref: Option<String>,
    pub deleted: bool,
    /// Set when this record was found to be the same file as another one (e.g. a carving
    /// result matching a named file system record). Merged records are hidden from listings.
    pub merged_into: Option<i64>,
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
    pub unknown: u64,
}

impl Counts {
    pub fn total(&self) -> u64 {
        self.photos + self.videos
    }
}
