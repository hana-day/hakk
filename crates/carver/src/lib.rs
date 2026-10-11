//! Carving: finding and extracting files from raw data without the file system.
//!
//! - [`scan`]: walks a whole source and finds JPEG and MP4/QuickTime files
//! - [`jpeg`], [`mp4`]: structure-aware carvers for one file at a known offset
//! - [`naive`]: the minimal baseline (SOI to the first EOI), kept for comparison
//! - [`reader`]: a windowed reader over a [`blocksource::BlockSource`]

pub mod jpeg;
pub mod mp4;
pub mod naive;
pub mod reader;
pub mod scan;

/// A candidate file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub offset: u64,
    pub len: u64,
    pub kind: Kind,
    pub status: Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
    Mp4,
    Mov,
}

impl Kind {
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Jpeg => "jpg",
            Kind::Mp4 => "mp4",
            Kind::Mov => "mov",
        }
    }
}

/// How much of a found file checked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Structurally complete and verified.
    Complete,
    /// Broken at this source offset, often where a fragment ends.
    Corrupt { at: u64 },
    /// The data ran out before the file ended.
    Truncated,
}
