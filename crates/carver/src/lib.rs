//! Carving: finding and extracting files from raw data without the file system.
//!
//! - [`scan`]: walks a whole source and finds JPEG and MP4/QuickTime files
//! - [`jpeg`], [`mp4`]: structure-aware carvers for one file at a known offset
//! - [`fragments`]: reassembles fragmented JPEGs by resuming the decoder elsewhere
//! - [`naive`]: the minimal baseline (SOI to the first EOI), kept for comparison
//! - [`reader`]: a windowed reader over a [`blocksource::BlockSource`]

pub mod fragments;
pub mod jpeg;
pub mod mp4;
pub mod naive;
pub mod reader;
pub mod scan;

/// A candidate file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Where the file starts.
    pub offset: u64,
    /// Total length (the sum of the fragments).
    pub len: u64,
    pub kind: Kind,
    pub status: Status,
    /// `(source offset, length)` of each piece in file order; one piece if contiguous.
    pub fragments: Vec<(u64, u64)>,
    /// Picture dimensions, if known (from the JPEG frame header).
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// For videos: whether the index (`moov`) was found. A video without one usually has
    /// intact data that a rebuilt index can make playable again.
    pub has_index: Option<bool>,
}

impl Found {
    /// A contiguous file.
    pub fn contiguous(offset: u64, len: u64, kind: Kind, status: Status) -> Self {
        Self {
            offset,
            len,
            kind,
            status,
            fragments: vec![(offset, len)],
            width: None,
            height: None,
            has_index: None,
        }
    }

    /// Reads the file's bytes, joining its fragments.
    pub fn read<S: blocksource::BlockSource + ?Sized>(&self, src: &S) -> Vec<u8> {
        let mut out = Vec::new();
        for &(at, len) in &self.fragments {
            let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0)];
            let n = src.read_at(at, &mut buf).unwrap_or(0);
            buf.truncate(n);
            out.extend(buf);
        }
        out
    }
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
