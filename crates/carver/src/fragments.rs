//! Reassembling fragmented JPEGs by resuming the decoder elsewhere on the disk.
//!
//! When a baseline JPEG stops decoding, its first fragment ended shortly before the failure
//! point. Candidate boundaries are tried on a cluster grid just before that point, and for
//! each, candidate continuations elsewhere on the disk: decoding resumes from the decoder
//! state saved at the last MCU before the boundary, reading the candidate instead. The
//! right continuation keeps decoding (to the next fragment boundary or to EOI); wrong ones
//! break within a few hundred bytes. The best one is kept and the process repeats for the
//! next fragment.
//!
//! Candidates are pre-filtered cheaply: entropy-coded JPEG data only ever has `FF` followed
//! by `00` or a restart marker, so clusters containing other `FF xx` pairs (other file
//! types, filler, headers) are skipped without decoding.
//!
//! Reference: Garfinkel, "Carving contiguous and fragmented files with fast object
//! validation" (DFRWS 2007) for bifragment gap carving by validation; ITU-T T.81 for the
//! decoder itself (see [`crate::jpeg`]).

use std::collections::HashMap;

use blocksource::BlockSource;

use crate::{
    jpeg::{self, Outcome, Plan, Snapshot, Stop, Stream},
    reader::Reader,
};

/// Search parameters.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Fragments start and end on this grid, relative to the file start (a cluster size
    /// every file system uses is a multiple of; 512 also catches sector-level fragments).
    pub grid: u64,
    /// How far before the failure point to look for the end of a fragment.
    pub back: u64,
    /// How far from the end of a fragment (either way) to look for the next one.
    pub reach: u64,
    /// Bytes of clean decoding after a boundary that settle a candidate immediately.
    pub accept: u64,
    /// Fewest bytes of clean decoding for a candidate to count at all.
    pub min_progress: u64,
    pub max_fragments: usize,
    /// Trial decodes allowed per file.
    pub budget: u32,
    /// Only reassemble JPEGs with restart markers. Without them, another photo's data
    /// encoded with the same Huffman tables also decodes cleanly (Huffman codes
    /// resynchronize), so a continuation cannot be told apart by decoding alone.
    pub require_restart: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            grid: 4096,
            back: 64 << 10,
            reach: 64 << 20,
            accept: 32 << 10,
            min_progress: 2048,
            max_fragments: 8,
            budget: 200_000,
            require_restart: true,
        }
    }
}

/// A file put back together from pieces of the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reassembled {
    /// `(source offset, length)` of each piece, in file order.
    pub fragments: Vec<(u64, u64)>,
    /// Whether decoding reached EOI.
    pub complete: bool,
}

/// One piece of the virtual file: virtual offset `v` maps to source offset `src`.
#[derive(Debug, Clone, Copy)]
struct Seg {
    v: u64,
    src: u64,
}

/// The file as a list of pieces; the last piece runs on to the end of the source.
struct Spliced<'r, 'a, S: BlockSource + ?Sized> {
    r: &'r mut Reader<'a, S>,
    segs: Vec<Seg>,
}

impl<S: BlockSource + ?Sized> Spliced<'_, '_, S> {
    fn map(&self, v: u64) -> Option<u64> {
        let seg = self.segs.iter().rev().find(|s| s.v <= v)?;
        seg.src.checked_add(v - seg.v)
    }
}

impl<S: BlockSource + ?Sized> Stream for Spliced<'_, '_, S> {
    fn byte(&mut self, v: u64) -> Option<u8> {
        let src = self.map(v)?;
        self.r.byte(src)
    }
}

/// Tries to reassemble the JPEG at `start`. Returns `None` if it is not a single-scan
/// baseline JPEG that breaks (complete files need no help).
pub fn reassemble<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    start: u64,
    max_len: u64,
    opts: &Options,
) -> Option<Reassembled> {
    let (j, plan) = jpeg::carve_with_plan(r, start, max_len)?;
    let plan = plan?;
    if !matches!(j.status, jpeg::Status::Corrupt { .. })
        || (opts.require_restart && !plan.has_restart())
    {
        return None;
    }
    let limit = start.saturating_add(max_len);
    let mut segs = vec![Seg {
        v: start,
        src: start,
    }];
    let mut snaps = Vec::new();
    let mut budget = opts.budget;
    let mut filter = Filter::default();

    let mut outcome = {
        let mut s = Spliced {
            r: &mut *r,
            segs: segs.clone(),
        };
        jpeg::decode(
            &plan,
            &mut s,
            Snapshot::start(plan.data),
            limit,
            None,
            Some(&mut snaps),
        )
    };
    loop {
        let fail_at = match outcome {
            Outcome::Done(end) => {
                let complete = r_is_eoi(r, &segs, end);
                return Some(finish(&segs, end + 2, complete));
            }
            Outcome::Fail(Stop::Corrupt(at)) => at,
            Outcome::Fail(Stop::Truncated) | Outcome::Capped => {
                return Some(finish(&segs, last_end(&segs, &snaps), false));
            }
        };
        if segs.len() >= opts.max_fragments || budget == 0 {
            return Some(finish(&segs, fail_at, false));
        }
        let Some((b, s, _)) = best_continuation(
            r,
            &plan,
            start,
            limit,
            &segs,
            &snaps,
            fail_at,
            opts,
            &mut budget,
            &mut filter,
        ) else {
            return Some(finish(&segs, fail_at, false));
        };
        // Splice in the continuation and decode on from the last MCU before the boundary.
        segs.push(Seg { v: b, src: s });
        let from = last_snapshot_before(&snaps, b)?;
        snaps.retain(|x| x.pos <= from.pos);
        snaps.pop();
        let mut sp = Spliced {
            r: &mut *r,
            segs: segs.clone(),
        };
        outcome = jpeg::decode(&plan, &mut sp, from, limit, None, Some(&mut snaps));
    }
}

/// Finds the boundary `b` (virtual) and continuation `s` (source) that decode furthest.
#[allow(clippy::too_many_arguments)]
fn best_continuation<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    plan: &Plan,
    start: u64,
    limit: u64,
    segs: &[Seg],
    snaps: &[Snapshot],
    fail_at: u64,
    opts: &Options,
    budget: &mut u32,
    filter: &mut Filter,
) -> Option<(u64, u64, u64)> {
    let last = *segs.last()?;
    let grid = opts.grid.max(512);
    // The grid is anchored at the file start (clusters are aligned with it).
    let phase = start % grid;
    let on_grid = |x: u64| x >= phase && (x - phase).is_multiple_of(grid);

    // Boundaries: grid points in the current piece, from just before the failure backwards.
    let src_fail = last.src + (fail_at - last.v);
    let lo_src = src_fail.saturating_sub(opts.back).max(last.src + 1);
    let first_b = (src_fail.saturating_sub(phase) / grid) * grid + phase;
    let boundaries: Vec<u64> = (0..)
        .map(|k| first_b.saturating_sub(k * grid))
        .take_while(|&x| x >= lo_src && x > 0)
        .filter(|&x| on_grid(x))
        .map(|x| last.v + (x - last.src))
        .collect();

    // Continuations: grid points around the boundary, nearest first, outside used pieces.
    let used = used_ranges(segs, fail_at);
    let center = src_fail;
    let len = r.len();
    let mut best: Option<(u64, u64, u64)> = None;
    for &b in &boundaries {
        let Some(from) = last_snapshot_before(snaps, b) else {
            continue;
        };
        let b_src = last.src + (b - last.v);
        for k in 1..=(opts.reach / grid) {
            for s in [center.checked_add(k * grid), center.checked_sub(k * grid)]
                .into_iter()
                .flatten()
            {
                let s = (s.saturating_sub(phase) / grid) * grid + phase;
                if s >= len || s == b_src || used.iter().any(|(a, e)| (*a..*e).contains(&s)) {
                    continue;
                }
                if !filter.clean(r, s, grid) {
                    continue;
                }
                if *budget == 0 {
                    return best;
                }
                *budget -= 1;
                let mut trial: Vec<Seg> = segs.to_vec();
                trial.push(Seg { v: b, src: s });
                let mut sp = Spliced {
                    r: &mut *r,
                    segs: trial,
                };
                let progress =
                    match jpeg::decode(plan, &mut sp, from, limit, Some(b + opts.accept), None) {
                        Outcome::Done(end) => {
                            if r_is_eoi_spliced(&mut sp, end) {
                                return Some((b, s, u64::MAX));
                            }
                            0
                        }
                        Outcome::Capped => return Some((b, s, opts.accept)),
                        Outcome::Fail(Stop::Corrupt(at)) => at.saturating_sub(b),
                        Outcome::Fail(Stop::Truncated) => 0,
                    };
                if progress >= opts.min_progress && best.is_none_or(|x| progress > x.2) {
                    best = Some((b, s, progress));
                }
            }
        }
    }
    best
}

/// Source ranges already part of the file (they cannot be the continuation).
fn used_ranges(segs: &[Seg], end_v: u64) -> Vec<(u64, u64)> {
    segs.iter()
        .enumerate()
        .map(|(i, s)| {
            let end = segs.get(i + 1).map_or(end_v, |n| n.v);
            (s.src, s.src + end.saturating_sub(s.v))
        })
        .collect()
}

fn last_snapshot_before(snaps: &[Snapshot], b: u64) -> Option<Snapshot> {
    let i = snaps.partition_point(|s| s.pos <= b);
    snaps.get(i.checked_sub(1)?).copied()
}

fn last_end(segs: &[Seg], snaps: &[Snapshot]) -> u64 {
    snaps
        .last()
        .map_or_else(|| segs.last().map_or(0, |s| s.v), |s| s.pos)
}

fn r_is_eoi<S: BlockSource + ?Sized>(r: &mut Reader<S>, segs: &[Seg], v: u64) -> bool {
    let mut sp = Spliced {
        r,
        segs: segs.to_vec(),
    };
    r_is_eoi_spliced(&mut sp, v)
}

fn r_is_eoi_spliced<T: Stream + ?Sized>(s: &mut T, v: u64) -> bool {
    s.byte(v) == Some(0xFF) && s.byte(v + 1) == Some(0xD9)
}

/// Turns pieces and an end (virtual, exclusive) into source ranges.
fn finish(segs: &[Seg], end_v: u64, complete: bool) -> Reassembled {
    let fragments = segs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let end = segs.get(i + 1).map_or(end_v, |n| n.v);
            (s.src, end.saturating_sub(s.v))
        })
        .filter(|f| f.1 > 0)
        .collect();
    Reassembled {
        fragments,
        complete,
    }
}

/// Caches whether a grid cell could be entropy-coded JPEG data.
#[derive(Default)]
struct Filter {
    seen: HashMap<u64, bool>,
}

impl Filter {
    fn clean<S: BlockSource + ?Sized>(&mut self, r: &mut Reader<S>, at: u64, len: u64) -> bool {
        if let Some(&c) = self.seen.get(&at) {
            return c;
        }
        let c = r
            .slice(at, usize::try_from(len).unwrap_or(4096))
            .is_some_and(entropy_like);
        self.seen.insert(at, c);
        c
    }
}

/// Could these bytes be entropy-coded JPEG data? Only `FF 00`, `FF D0`-`FF D7` and the
/// final `FF D9` may appear; constant filler is rejected too.
fn entropy_like(b: &[u8]) -> bool {
    if b.first().is_some_and(|&x| b.iter().all(|&y| y == x)) {
        return false;
    }
    let mut ff = 0usize;
    for w in b.windows(2) {
        if let [0xFF, next] = w {
            ff += 1;
            if !matches!(next, 0x00 | 0xD0..=0xD7 | 0xD9 | 0xFF) {
                return false;
            }
        }
    }
    // Real entropy data has an FF roughly every 256 bytes (each one stuffed).
    ff > 0 || b.len() < 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_filter() {
        assert!(entropy_like(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD3, 0x56]));
        assert!(!entropy_like(&[0x12, 0xFF, 0xC4, 0x00]));
        assert!(!entropy_like(&[0xB9; 4096]));
    }
}
