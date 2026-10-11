//! File system analysis.
//!
//! Finds the volumes on a disk ([`find_partitions`]) and lists their files, including
//! deleted ones, with where their data lies ([`scan_volume`], [`scan_disk`]).
//!
//! Supported: exFAT and FAT12/16/32.
//!
//! Every read goes through [`BlockSource`], so this crate cannot write to the disk. The
//! parsers never panic or loop forever on malformed input: every structure read from disk
//! is bounds-checked, cluster chains are bounded by the cluster count, and each directory
//! cluster is parsed at most once (so directory cycles end).
//!
//! # Deleted files
//!
//! FAT and exFAT drop a deleted file's allocation (FAT12/16/32 clear its FAT chain; exFAT
//! clears its bits in the allocation bitmap), so where its data lies has to be inferred.
//! [`Entry::layout`] says how the extents were obtained and [`Entry::confidence`] how far
//! they can be trusted.

mod disk;
mod exfat;
mod fat;
mod heap;
mod le;
mod partition;
mod time;

use std::fmt;

use blocksource::{BlockSource, ReadError};

pub use partition::{Origin, Partition, find_partitions};
pub use time::Timestamp;

/// A byte range on the source (offsets are absolute, not relative to the volume).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Extent {
    pub offset: u64,
    pub len: u64,
}

impl Extent {
    pub fn end(&self) -> u64 {
        self.offset.saturating_add(self.len)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FsKind {
    Exfat,
    Fat12,
    Fat16,
    Fat32,
}

impl fmt::Display for FsKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FsKind::Exfat => "exFAT",
            FsKind::Fat12 => "FAT12",
            FsKind::Fat16 => "FAT16",
            FsKind::Fat32 => "FAT32",
        })
    }
}

/// How an entry's extents were obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layout {
    /// Followed the FAT chain of a live entry.
    Chain,
    /// The file system recorded the allocation as one contiguous run (exFAT `NoFatChain`),
    /// so the placement is known even after deletion.
    Contiguous,
    /// A deleted exFAT entry whose FAT chain was still intact: exFAT only clears the
    /// allocation bitmap on deletion, so the old chain may survive.
    StaleChain,
    /// The chain is gone; the data is assumed to be contiguous from the first cluster.
    Assumed,
}

/// How far an entry's extents can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Confidence {
    /// Some clusters are now allocated to other data (likely overwritten), or the
    /// placement could not be determined completely.
    Low,
    /// The placement is inferred (assumed contiguous, or a stale chain) and none of its
    /// clusters is currently allocated.
    Medium,
    /// Known placement: a live entry, a contiguous (`NoFatChain`) entry or a deleted entry
    /// fitting in one cluster, with no cluster reallocated since.
    High,
}

/// A file or directory, live or deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path within the volume, `/`-separated, without a leading `/`.
    pub path: String,
    pub name: String,
    /// `false` if part of the name was lost and guessed (FAT replaces the first character
    /// of a deleted short name) or could not be read.
    pub name_exact: bool,
    pub is_dir: bool,
    /// Deleted, or inside a deleted directory.
    pub deleted: bool,
    /// Size in bytes (0 for FAT directories, which do not record one).
    pub size: u64,
    /// Bytes holding written data; the rest up to `size` reads as zeros (exFAT
    /// `ValidDataLength`). Equal to `size` on FAT.
    pub valid_size: u64,
    pub first_cluster: u32,
    /// Where the data lies, in file order. Covers `size` bytes unless the placement could
    /// not be determined completely (then `confidence` is [`Confidence::Low`]).
    pub extents: Vec<Extent>,
    pub layout: Layout,
    pub confidence: Confidence,
    pub created: Option<Timestamp>,
    pub modified: Option<Timestamp>,
    pub accessed: Option<Timestamp>,
}

/// A scanned volume.
#[derive(Debug, Clone)]
pub struct Volume {
    pub kind: FsKind,
    /// Where the volume starts on the source.
    pub offset: u64,
    pub len: u64,
    pub cluster_size: u64,
    pub label: Option<String>,
    pub entries: Vec<Entry>,
    /// Unallocated clusters, merged into runs (for scanning free space only).
    pub free: Vec<Extent>,
    /// Problems found while scanning (unreadable sectors, inconsistent structures).
    /// Scanning continues past them.
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub enum Error {
    /// A structure needed to scan the volume could not be read.
    Read(ReadError),
    /// No supported file system was found.
    Unrecognized,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read(e) => e.fmt(f),
            Error::Unrecognized => f.write_str("no supported file system found"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Read(e) => Some(e),
            Error::Unrecognized => None,
        }
    }
}

impl From<ReadError> for Error {
    fn from(e: ReadError) -> Self {
        Error::Read(e)
    }
}

/// Scans the file system in `part`.
pub fn scan_volume(src: &dyn BlockSource, part: &Partition) -> Result<Volume, Error> {
    let disk = disk::Disk::new(src, part.offset, part.len);
    let mut vol = if let Some(boot) = exfat::probe(&disk)? {
        exfat::scan(&disk, &boot)
    } else if let Some(bpb) = fat::probe(&disk)? {
        fat::scan(&disk, &bpb)
    } else {
        return Err(Error::Unrecognized);
    };
    demote_overlapping(&mut vol.entries);
    Ok(vol)
}

/// Deleted files whose inferred extents overlap another file's cannot all be intact: the
/// clusters were reused by a file that was itself deleted later. Marks them
/// [`Confidence::Low`]. (Overlap with live files is already caught by the allocation
/// state.)
fn demote_overlapping(entries: &mut [Entry]) {
    let mut spans: Vec<(Extent, usize)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.is_dir)
        .flat_map(|(i, e)| e.extents.iter().map(move |x| (*x, i)))
        .filter(|(x, _)| x.len > 0)
        .collect();
    spans.sort_unstable_by_key(|(x, _)| (x.offset, x.end()));
    let mut hit = vec![false; entries.len()];
    // Sweep: compare each extent with the furthest-reaching one before it.
    let mut reach: Option<(u64, usize)> = None;
    for &(x, i) in &spans {
        if let Some((end, j)) = reach
            && x.offset < end
            && j != i
        {
            for k in [i, j] {
                if let Some(h) = hit.get_mut(k) {
                    *h = true;
                }
            }
        }
        if reach.is_none_or(|(end, _)| x.end() > end) {
            reach = Some((x.end(), i));
        }
    }
    for (e, h) in entries.iter_mut().zip(hit) {
        if h && e.deleted {
            e.confidence = Confidence::Low;
        }
    }
}

/// A partition and the result of scanning its file system.
pub type ScannedPartition = (Partition, Result<Volume, Error>);

/// Finds the partitions on `src` and scans each one.
pub fn scan_disk(src: &dyn BlockSource) -> Result<Vec<ScannedPartition>, Error> {
    Ok(find_partitions(src)?
        .into_iter()
        .map(|p| {
            let v = scan_volume(src, &p);
            (p, v)
        })
        .collect())
}
