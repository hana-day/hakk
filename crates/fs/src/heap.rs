//! The cluster heap shared by FAT and exFAT: cluster chains, allocation state, and
//! inferring where a (possibly deleted) file's data lies.
//!
//! Reference: Microsoft "FAT32 File System Specification" (fatgen103), "FAT Data
//! Structure" (cluster N starts at FirstDataSector + (N - 2) * BPB_SecPerClus; the FAT
//! entry of a cluster holds the next cluster of the file, or an end-of-chain mark).
//! Microsoft "exFAT file system specification" 4.1 First and Second FAT Sub-regions,
//! 5.1 Cluster Heap Sub-region, 6.3.4.2 / 6.4.2.2 NoFatChain Field (when set, the
//! allocation is one contiguous run and the FAT entries are not to be interpreted), and
//! 7.1.5 Allocation Bitmap (bit N describes cluster N + 2; the bitmap, not the FAT,
//! records which clusters are in use).

use std::collections::HashSet;

use crate::{Confidence, Extent, Layout, disk::Disk};

/// Normalized FAT entry values. Any other value is the next cluster of the chain.
pub(crate) const FREE: u32 = 0;
pub(crate) const END: u32 = u32::MAX;
pub(crate) const BAD: u32 = u32::MAX - 1;
/// Reserved values, or a "next cluster" outside the heap.
pub(crate) const INVALID: u32 = u32::MAX - 2;

/// A run of consecutive clusters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) start: u32,
    pub(crate) len: u32,
}

pub(crate) struct Heap {
    /// Absolute offset of the volume on the source.
    vol_start: u64,
    /// Offset of cluster 2 within the volume.
    base: u64,
    pub(crate) cluster_size: u64,
    /// Number of clusters; valid cluster numbers are `2..count + 2`.
    pub(crate) count: u32,
    /// Normalized FAT, indexed by cluster number.
    next: Vec<u32>,
    /// Allocation state, bit `N` for cluster `N + 2`.
    alloc: Vec<u64>,
}

/// How a chain walk stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Reached an end-of-chain mark.
    End,
    /// Collected the requested number of clusters.
    Limit,
    /// Hit a free, bad or invalid entry, or a cluster outside the heap.
    Broken,
}

pub(crate) struct Placement {
    pub(crate) runs: Vec<Run>,
    pub(crate) extents: Vec<Extent>,
    pub(crate) layout: Layout,
    pub(crate) confidence: Confidence,
}

impl Heap {
    pub(crate) fn new(
        disk: &Disk,
        base: u64,
        cluster_size: u64,
        count: u32,
        next: Vec<u32>,
    ) -> Self {
        let words = (count as usize).div_ceil(64);
        Self {
            vol_start: disk.start(),
            base,
            cluster_size: cluster_size.max(1),
            count,
            next,
            alloc: vec![0; words],
        }
    }

    /// FAT12/16/32: a cluster is in use when its FAT entry is not zero.
    pub(crate) fn allocation_from_fat(&mut self) {
        for c in 2..self.end() {
            if self.next_of(c) != FREE {
                self.set_allocated(c);
            }
        }
    }

    /// exFAT: the allocation bitmap.
    pub(crate) fn allocation_from_bitmap(&mut self, bitmap: &[u8]) {
        for (i, &byte) in bitmap.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    let c = (i as u64) * 8 + bit + 2;
                    if let Ok(c) = u32::try_from(c) {
                        self.set_allocated(c);
                    }
                }
            }
        }
    }

    fn set_allocated(&mut self, c: u32) {
        if !self.in_heap(c) {
            return;
        }
        let i = (c - 2) as usize;
        if let Some(w) = self.alloc.get_mut(i / 64) {
            *w |= 1 << (i % 64);
        }
    }

    /// One past the last valid cluster number.
    fn end(&self) -> u32 {
        self.count.saturating_add(2)
    }

    pub(crate) fn in_heap(&self, c: u32) -> bool {
        c >= 2 && c < self.end()
    }

    /// Clusters outside the heap count as allocated (they are not free space).
    pub(crate) fn is_allocated(&self, c: u32) -> bool {
        if !self.in_heap(c) {
            return true;
        }
        let i = (c - 2) as usize;
        self.alloc
            .get(i / 64)
            .is_none_or(|w| w & (1 << (i % 64)) != 0)
    }

    fn next_of(&self, c: u32) -> u32 {
        match self.next.get(c as usize) {
            Some(&n) if n == FREE || n == END || n == BAD || n == INVALID => n,
            Some(&n) if self.in_heap(n) => n,
            _ => INVALID,
        }
    }

    /// Offset of cluster `c` within the volume.
    pub(crate) fn rel_offset(&self, c: u32) -> u64 {
        self.base
            .saturating_add(u64::from(c.saturating_sub(2)).saturating_mul(self.cluster_size))
    }

    pub(crate) fn clusters_for(&self, size: u64) -> u64 {
        size.div_ceil(self.cluster_size)
    }

    /// Follows the chain from `first`, collecting at most `max` clusters.
    pub(crate) fn follow(&self, first: u32, max: u64) -> (Vec<Run>, u64, Stop) {
        let mut runs: Vec<Run> = Vec::new();
        let mut total = 0u64;
        let mut c = first;
        // `max` is capped by the cluster count, so a cycle cannot loop forever.
        let max = max.min(u64::from(self.count));
        loop {
            if total >= max {
                return (runs, total, Stop::Limit);
            }
            if !self.in_heap(c) {
                return (runs, total, Stop::Broken);
            }
            push_cluster(&mut runs, c);
            total += 1;
            match self.next_of(c) {
                END => {
                    let stop = if total >= max { Stop::Limit } else { Stop::End };
                    return (runs, total, stop);
                }
                FREE | BAD | INVALID => {
                    let stop = if total >= max {
                        Stop::Limit
                    } else {
                        Stop::Broken
                    };
                    return (runs, total, stop);
                }
                n => c = n,
            }
        }
    }

    /// `n` clusters from `first`, clipped to the end of the heap.
    pub(crate) fn contiguous(&self, first: u32, n: u64) -> Vec<Run> {
        if !self.in_heap(first) || n == 0 {
            return Vec::new();
        }
        let avail = u64::from(self.end() - first);
        let len = u32::try_from(n.min(avail)).unwrap_or(u32::MAX);
        vec![Run { start: first, len }]
    }

    pub(crate) fn run_clusters(runs: &[Run]) -> impl Iterator<Item = u32> + '_ {
        runs.iter()
            .flat_map(|r| (0..r.len).map(move |i| r.start.saturating_add(i)))
    }

    fn total(runs: &[Run]) -> u64 {
        runs.iter().map(|r| u64::from(r.len)).sum()
    }

    /// Absolute extents of `runs`, trimmed to `size` bytes.
    pub(crate) fn extents(&self, runs: &[Run], size: u64) -> Vec<Extent> {
        let mut left = size;
        let mut out = Vec::new();
        for r in runs {
            if left == 0 {
                break;
            }
            let len = (u64::from(r.len) * self.cluster_size).min(left);
            left -= len;
            out.push(Extent {
                offset: self.vol_start.saturating_add(self.rel_offset(r.start)),
                len,
            });
        }
        out
    }

    /// Decides where an entry's data lies.
    ///
    /// - `no_fat_chain`: the entry says its clusters are contiguous (exFAT only).
    /// - `stale_chain`: a deleted entry's FAT chain may have survived (exFAT keeps FAT
    ///   entries on deletion; FAT12/16/32 zero them, so there is nothing to follow).
    pub(crate) fn place(
        &self,
        first: u32,
        size: u64,
        deleted: bool,
        no_fat_chain: bool,
        stale_chain: bool,
    ) -> Placement {
        let n = self.clusters_for(size);
        let empty = |layout, confidence| Placement {
            runs: Vec::new(),
            extents: Vec::new(),
            layout,
            confidence,
        };
        if n == 0 {
            return empty(Layout::Chain, Confidence::High);
        }
        if !self.in_heap(first) {
            return empty(Layout::Assumed, Confidence::Low);
        }
        let (runs, layout) = if no_fat_chain {
            (self.contiguous(first, n), Layout::Contiguous)
        } else if !deleted {
            (self.follow(first, n).0, Layout::Chain)
        } else {
            let stale = (stale_chain && n > 1)
                .then(|| self.follow(first, n))
                .filter(|(runs, total, _)| {
                    *total == n
                        && runs
                            .last()
                            .is_some_and(|r| self.next_of(r.start + (r.len - 1)) == END)
                });
            match stale {
                Some((runs, _, _)) => (runs, Layout::StaleChain),
                None => (self.contiguous(first, n), Layout::Assumed),
            }
        };
        let complete = Self::total(&runs) == n;
        let reallocated = deleted && Self::run_clusters(&runs).any(|c| self.is_allocated(c));
        let confidence = if !complete || reallocated {
            Confidence::Low
        } else if !deleted || layout == Layout::Contiguous || n == 1 {
            Confidence::High
        } else {
            Confidence::Medium
        };
        Placement {
            extents: self.extents(&runs, size),
            runs,
            layout,
            confidence,
        }
    }

    /// Unallocated clusters as absolute extents, merged into runs.
    pub(crate) fn free_extents(&self) -> Vec<Extent> {
        let mut runs: Vec<Run> = Vec::new();
        for c in 2..self.end() {
            if !self.is_allocated(c) {
                push_cluster(&mut runs, c);
            }
        }
        self.extents(&runs, u64::MAX)
    }

    /// Reads the clusters of `runs` (at most `cap` bytes); unreadable parts read as zeros.
    pub(crate) fn read_runs(
        &self,
        disk: &Disk,
        runs: &[Run],
        cap: u64,
        warnings: &mut Vec<String>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        for r in runs {
            let left = cap.saturating_sub(out.len() as u64);
            let len = (u64::from(r.len) * self.cluster_size).min(left);
            let Ok(len) = usize::try_from(len) else {
                break;
            };
            if len == 0 {
                break;
            }
            out.extend(disk.read_lossy(self.rel_offset(r.start), len, warnings));
        }
        out
    }
}

/// Keeps the runs up to the first cluster already visited, marking them visited. Each
/// directory cluster is parsed at most once, which also ends directory cycles.
pub(crate) fn unvisited_prefix(runs: &[Run], visited: &mut HashSet<u32>) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for c in Heap::run_clusters(runs) {
        if !visited.insert(c) {
            break;
        }
        push_cluster(&mut out, c);
    }
    out
}

/// `count` limited to the clusters that lie (at least partly) within the source.
pub(crate) fn available_clusters(
    disk: &Disk,
    heap_offset: u64,
    cluster_size: u64,
    count: u32,
) -> u32 {
    let avail = disk
        .len()
        .saturating_sub(heap_offset)
        .div_ceil(cluster_size.max(1));
    u32::try_from(avail).map_or(count, |a| a.min(count))
}

fn push_cluster(runs: &mut Vec<Run>, c: u32) {
    if let Some(last) = runs.last_mut()
        && last.start.checked_add(last.len) == Some(c)
    {
        last.len += 1;
        return;
    }
    runs.push(Run { start: c, len: 1 });
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    fn heap(next: Vec<u32>) -> Heap {
        let src = MemSource::new(vec![0u8; 1 << 16]);
        let disk = Disk::new(&src, 0, 1 << 16);
        let count = u32::try_from(next.len() - 2).unwrap();
        let mut h = Heap::new(&disk, 1000, 100, count, next);
        h.allocation_from_fat();
        h
    }

    #[test]
    fn follows_and_merges() {
        // 2 -> 3 -> 5 -> end
        let h = heap(vec![0, 0, 3, 5, 0, END, 0]);
        let (runs, total, stop) = h.follow(2, 10);
        assert_eq!(runs, [Run { start: 2, len: 2 }, Run { start: 5, len: 1 }]);
        assert_eq!((total, stop), (3, Stop::End));
        let e = h.extents(&runs, 250);
        assert_eq!(
            e,
            [
                Extent {
                    offset: 1000,
                    len: 200
                },
                Extent {
                    offset: 1300,
                    len: 50
                }
            ]
        );
    }

    #[test]
    fn cycles_end() {
        let h = heap(vec![0, 0, 3, 2]);
        let (_, total, stop) = h.follow(2, u64::MAX);
        assert_eq!((total, stop), (2, Stop::Limit));
    }

    #[test]
    fn deleted_placement() {
        // Cluster 4 is in use by someone else.
        let h = heap(vec![0, 0, 0, 0, END, 0, 0, 0]);
        let p = h.place(5, 300, true, false, false);
        assert_eq!(
            (p.layout, p.confidence),
            (Layout::Assumed, Confidence::Medium)
        );
        let p = h.place(3, 300, true, false, false);
        assert_eq!(p.confidence, Confidence::Low);
        let p = h.place(5, 300, true, true, false);
        assert_eq!(
            (p.layout, p.confidence),
            (Layout::Contiguous, Confidence::High)
        );
        // Runs past the end of the heap.
        let p = h.place(6, 500, true, false, false);
        assert_eq!(p.confidence, Confidence::Low);
        assert_eq!(
            h.free_extents(),
            [
                Extent {
                    offset: 1000,
                    len: 200
                },
                Extent {
                    offset: 1300,
                    len: 300
                }
            ]
        );
    }

    #[test]
    fn stale_chain() {
        // A deleted exFAT file: bitmap says free, FAT still says 2 -> 5 -> end.
        let src = MemSource::new(vec![0u8; 4096]);
        let disk = Disk::new(&src, 0, 4096);
        let h = Heap::new(&disk, 0, 100, 5, vec![0, 0, 5, 0, 0, END, 0]);
        let p = h.place(2, 150, true, false, true);
        assert_eq!(
            (p.layout, p.confidence),
            (Layout::StaleChain, Confidence::Medium)
        );
        assert_eq!(p.runs, [Run { start: 2, len: 1 }, Run { start: 5, len: 1 }]);
        // Too long for the chain: fall back to assuming contiguity.
        let p = h.place(2, 350, true, false, true);
        assert_eq!(p.layout, Layout::Assumed);
    }
}
