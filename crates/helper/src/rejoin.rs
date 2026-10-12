//! After a scan: looking for the missing fragments of broken JPEGs.
//!
//! The scan reports a fragmented JPEG as damaged and moves on, since the search can take
//! seconds per file. Once the whole disk has been read, the search runs here, skipping the
//! data of every file known to be whole (complete files, and files the file system still
//! uses), which on a full disk is most of it. A JPEG put back together gets its fragments
//! as extents and becomes recoverable.

use std::ops::Range;
use std::path::Path;

use blocksource::BlockSource;
use carver::{fragments, reader::Reader};
use index::{FileKind, Index, Origin, Query, Status};

/// Longest JPEG to put back together.
const MAX_JPEG: u64 = 64 << 20;

/// Rejoins the broken carved JPEGs in the index at `path`, reading `source`; `stop`
/// ends the work early. Returns how many were put back together.
pub(crate) fn rejoin_jpegs(
    path: &Path,
    source: &dyn BlockSource,
    stop: &dyn Fn() -> bool,
) -> Result<usize, index::Error> {
    let mut index = Index::open(path)?;
    let records = index.files(&Query::default())?;
    let broken: Vec<(i64, u64)> = records
        .iter()
        .filter(|r| {
            r.origin == Origin::Carving && r.kind == FileKind::Jpeg && r.status == Status::Damaged
        })
        .filter_map(|r| Some((r.id, r.offset?)))
        .collect();
    if broken.is_empty() {
        return Ok(0);
    }
    // Data that belongs to other files: complete files, and files still in use.
    let mut taken: Vec<(u64, u64)> = Vec::new();
    for r in &records {
        let whole =
            r.status == Status::Recoverable || (r.origin == Origin::FileSystem && !r.deleted);
        if whole {
            taken.extend(index.extents(r.id)?.into_iter().map(|e| (e.start, e.end)));
        }
    }
    let taken = merge(taken);

    let mut reader = Reader::new(source);
    let opts = fragments::Options::default();
    let mut joined = 0;
    for (id, offset) in broken {
        if stop() {
            break;
        }
        let Some(found) =
            fragments::reassemble_avoiding(&mut reader, offset, MAX_JPEG, &opts, &taken)
                .filter(|f| f.complete && f.fragments.len() > 1)
        else {
            continue;
        };
        let extents: Vec<Range<u64>> = found
            .fragments
            .iter()
            .map(|&(at, len)| at..at + len)
            .collect();
        index.set_extents(id, &extents, Status::Recoverable)?;
        joined += 1;
    }
    Ok(joined)
}

/// Sorted, non-overlapping ranges covering `ranges`.
fn merge(mut ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    ranges.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (a, e) in ranges {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(e),
            _ => out.push((a, e)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlaps() {
        assert_eq!(
            merge(vec![(10, 20), (0, 5), (15, 30), (40, 50)]),
            vec![(0, 5), (10, 30), (40, 50)]
        );
    }
}
