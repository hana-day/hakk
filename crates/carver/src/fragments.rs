//! Reassembling fragmented JPEGs by resuming the decoder elsewhere on the disk.
//!
//! When a baseline JPEG stops decoding, its first fragment ended shortly before the failure
//! point. Candidate boundaries are tried on a cluster grid just before that point, and for
//! each, candidate continuations elsewhere on the disk: decoding resumes from the decoder
//! state saved at the last MCU before the boundary, reading the candidate instead.
//!
//! Decoding alone is not enough: another photo's data encoded with the same Huffman tables
//! also decodes cleanly (Huffman codes resynchronize). So each candidate is also judged by
//! how smoothly the picture continues across the seam: the DC coefficients of luminance
//! blocks form a 1/8-scale image, and the blocks just after the seam are compared with the
//! ones right above them, relative to how much neighboring rows differ elsewhere in the
//! same photo. The candidate with the most natural seam wins; if every seam is a jump, the
//! file is left broken rather than glued to the wrong data. Restart markers, where present,
//! are checked by the decoder as well.
//!
//! Candidates are pre-filtered cheaply: entropy-coded JPEG data only ever has `FF` followed
//! by `00` or a restart marker, so clusters containing other `FF xx` pairs (other file
//! types, filler, headers) are skipped without decoding.
//!
//! Reference: Garfinkel, "Carving contiguous and fragmented files with fast object
//! validation" (DFRWS 2007) for bifragment gap carving by validation; Memon and Pal,
//! "Automated reassembly of file fragmented images using greedy algorithms" (IEEE TIP 2006)
//! for judging fragment adjacency by image continuity; ITU-T T.81 for the decoder.

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
    /// Bytes decoded past a boundary to judge a candidate.
    pub window: u64,
    /// Fewest bytes of clean decoding for a candidate to count at all.
    pub min_progress: u64,
    /// A seam this many times rougher than the photo's own row-to-row change is rejected.
    pub max_seam: f64,
    /// A seam this smooth (with a clean decoding window) is taken without looking further.
    pub good_seam: f64,
    pub max_fragments: usize,
    /// Trial decodes allowed per file.
    pub budget: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            grid: 4096,
            back: 64 << 10,
            reach: 16 << 20,
            window: 32 << 10,
            min_progress: 2048,
            max_seam: 3.0,
            good_seam: 1.5,
            max_fragments: 8,
            budget: 20_000,
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

/// Decoding progress so far: decoder states and luminance DC values, in step.
#[derive(Default)]
struct Trace {
    snaps: Vec<Snapshot>,
    /// `dcs.len()` when each snapshot was taken.
    marks: Vec<usize>,
    dcs: Vec<(u64, i32)>,
}

impl Trace {
    /// Drops everything decoded from snapshot `i` on.
    fn truncate(&mut self, i: usize) {
        let mark = self.marks.get(i).copied().unwrap_or(self.dcs.len());
        self.snaps.truncate(i);
        self.marks.truncate(i);
        self.dcs.truncate(mark);
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
    if !matches!(j.status, jpeg::Status::Corrupt { .. }) {
        return None;
    }
    let limit = start.saturating_add(max_len);
    let mut segs = vec![Seg {
        v: start,
        src: start,
    }];
    let mut trace = Trace::default();
    let mut budget = opts.budget;
    let mut filter = Filter::default();

    let mut outcome = run(
        r,
        &plan,
        &segs,
        Snapshot::start(plan.data),
        limit,
        &mut trace,
    );
    loop {
        let fail_at = match outcome {
            Outcome::Done(end) => {
                let mut sp = Spliced {
                    r: &mut *r,
                    segs: segs.clone(),
                };
                let complete = is_eoi(&mut sp, end);
                return Some(finish(&segs, end + 2, complete));
            }
            Outcome::Fail(Stop::Corrupt(at)) => at,
            Outcome::Fail(Stop::Truncated) | Outcome::Capped => {
                let end = trace.snaps.last().map_or(start, |s| s.pos);
                return Some(finish(&segs, end, false));
            }
        };
        if segs.len() >= opts.max_fragments || budget == 0 {
            return Some(finish(&segs, fail_at, false));
        }
        let search = Search {
            plan: &plan,
            start,
            limit,
            opts,
        };
        let Some((b, s)) = search.best(r, &segs, &trace, fail_at, &mut budget, &mut filter) else {
            return Some(finish(&segs, fail_at, false));
        };
        // Splice in the continuation and decode on from the last MCU before the boundary.
        segs.push(Seg { v: b, src: s });
        let i = trace.snaps.partition_point(|x| x.pos <= b).checked_sub(1)?;
        let from = *trace.snaps.get(i)?;
        trace.truncate(i);
        outcome = run(r, &plan, &segs, from, limit, &mut trace);
    }
}

/// Decodes from `from` over `segs`, appending to `trace`.
fn run<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    plan: &Plan,
    segs: &[Seg],
    from: Snapshot,
    limit: u64,
    trace: &mut Trace,
) -> Outcome {
    let mut sp = Spliced {
        r,
        segs: segs.to_vec(),
    };
    let mut snaps = Vec::new();
    let mut dcs = Vec::new();
    let outcome = jpeg::decode(
        plan,
        &mut sp,
        from,
        limit,
        None,
        Some(&mut snaps),
        Some(&mut dcs),
    );
    // Each MCU contributes the same number of luminance blocks.
    let per = if snaps.is_empty() {
        0
    } else {
        dcs.len().div_ceil(snaps.len())
    };
    let base = trace.dcs.len();
    for (k, snap) in snaps.into_iter().enumerate() {
        trace.marks.push((base + k * per).min(base + dcs.len()));
        trace.snaps.push(snap);
    }
    trace.dcs.extend(dcs);
    outcome
}

struct Search<'p> {
    plan: &'p Plan,
    start: u64,
    limit: u64,
    opts: &'p Options,
}

impl Search<'_> {
    /// Finds the boundary `b` (virtual) and continuation `s` (source) with the most natural
    /// seam among those that decode cleanly for a while.
    fn best<S: BlockSource + ?Sized>(
        &self,
        r: &mut Reader<S>,
        segs: &[Seg],
        trace: &Trace,
        fail_at: u64,
        budget: &mut u32,
        filter: &mut Filter,
    ) -> Option<(u64, u64)> {
        let opts = self.opts;
        let last = *segs.last()?;
        let grid = opts.grid.max(512);
        // The grid is anchored at the file start (clusters are aligned with it).
        let phase = self.start % grid;
        let snap_to_grid = |x: u64| (x.saturating_sub(phase) / grid) * grid + phase;

        // Boundaries: grid points in the current piece, from just before the failure back.
        let src_fail = last.src + (fail_at - last.v);
        let lo_src = src_fail.saturating_sub(opts.back).max(last.src + 1);
        let boundaries: Vec<u64> = (0..)
            .map(|k| snap_to_grid(src_fail).saturating_sub(k * grid))
            .take_while(|&x| x >= lo_src && x > 0)
            .map(|x| last.v + (x - last.src))
            .collect();

        let used = used_ranges(segs, fail_at);
        let len = r.len();
        let cols = self.plan.luma_cols.max(1);
        // (seam roughness, progress, b, s)
        let mut best: Option<(f64, u64, u64, u64)> = None;
        for &b in &boundaries {
            let i = trace.snaps.partition_point(|x| x.pos <= b);
            let Some(k) = i.checked_sub(1) else { continue };
            let Some(&from) = trace.snaps.get(k) else {
                continue;
            };
            let mark = trace.marks.get(k).copied().unwrap_or(0);
            let before = trace.dcs.get(..mark).unwrap_or_default();
            let base: HashMap<u64, i32> = before.iter().copied().collect();
            let typical = typical_step(before, &base, cols);
            let b_src = last.src + (b - last.v);

            for step in 1..=(opts.reach / grid) {
                let around = [
                    src_fail.checked_add(step * grid),
                    src_fail.checked_sub(step * grid),
                ];
                for s in around.into_iter().flatten().map(snap_to_grid) {
                    if s >= len || s == b_src || used.iter().any(|(a, e)| (*a..*e).contains(&s)) {
                        continue;
                    }
                    if !filter.clean(r, s, grid) {
                        continue;
                    }
                    if *budget == 0 {
                        return best.map(|x| (x.2, x.3));
                    }
                    *budget -= 1;

                    let mut trial = segs.to_vec();
                    trial.push(Seg { v: b, src: s });
                    let mut sp = Spliced {
                        r: &mut *r,
                        segs: trial,
                    };
                    let mut dcs = Vec::new();
                    let cap = Some(b + opts.window);
                    let outcome = jpeg::decode(
                        self.plan,
                        &mut sp,
                        from,
                        self.limit,
                        cap,
                        None,
                        Some(&mut dcs),
                    );
                    let (progress, settled) = match outcome {
                        Outcome::Done(end) if is_eoi(&mut sp, end) => (u64::MAX, true),
                        Outcome::Done(_) | Outcome::Fail(Stop::Truncated) => (0, false),
                        Outcome::Capped => (opts.window, true),
                        Outcome::Fail(Stop::Corrupt(at)) => (at.saturating_sub(b), false),
                    };
                    if progress < opts.min_progress {
                        continue;
                    }
                    let Some(roughness) = seam(&dcs, &base, cols, typical) else {
                        continue;
                    };
                    if roughness > opts.max_seam {
                        continue;
                    }
                    if settled && roughness <= opts.good_seam {
                        return Some((b, s));
                    }
                    let better = best
                        .is_none_or(|x| roughness < x.0 || (roughness == x.0 && progress > x.1));
                    if better {
                        best = Some((roughness, progress, b, s));
                    }
                }
            }
        }
        best.map(|x| (x.2, x.3))
    }
}

/// How much neighboring block rows typically differ before the seam (median of the last
/// few hundred vertical DC steps), floored so flat pictures still allow some change.
fn typical_step(dcs: &[(u64, i32)], base: &HashMap<u64, i32>, cols: u64) -> f64 {
    let mut steps: Vec<i32> = dcs
        .iter()
        .rev()
        .take(512)
        .filter_map(|&(i, v)| Some((v - *base.get(&i.checked_sub(cols)?)?).abs()))
        .collect();
    steps.sort_unstable();
    let median = steps.get(steps.len() / 2).copied().unwrap_or(0);
    f64::from(median.max(16))
}

/// Median vertical DC step from the blocks decoded after the seam to the blocks right
/// above them (decoded before it), relative to the photo's typical step. The median keeps
/// real edges that happen to cross the seam from counting against the right candidate.
/// `None` without enough pairs to judge.
fn seam(trial: &[(u64, i32)], base: &HashMap<u64, i32>, cols: u64, typical: f64) -> Option<f64> {
    let mut steps: Vec<i32> = trial
        .iter()
        .filter(|(i, _)| !base.contains_key(i))
        .filter_map(|&(i, v)| Some((v - *base.get(&i.checked_sub(cols)?)?).abs()))
        .collect();
    if steps.len() < 8 {
        return None;
    }
    steps.sort_unstable();
    let median = steps.get(steps.len() / 2).copied().unwrap_or(0);
    Some(f64::from(median) / typical)
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

fn is_eoi<T: Stream + ?Sized>(s: &mut T, v: u64) -> bool {
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
    for w in b.windows(2) {
        if let [0xFF, next] = w
            && !matches!(next, 0x00 | 0xD0..=0xD7 | 0xD9 | 0xFF)
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    #[test]
    fn entropy_filter() {
        assert!(entropy_like(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD3, 0x56]));
        assert!(!entropy_like(&[0x12, 0xFF, 0xC4, 0x00]));
        assert!(!entropy_like(&[0xB9; 4096]));
    }

    #[test]
    fn seam_compares_with_the_row_above() {
        let cols = 8;
        // Two rows of a smooth picture before the seam, then a row after it.
        let base: HashMap<u64, i32> = (0..16).map(|i| (i, 100 + i as i32)).collect();
        let smooth: Vec<(u64, i32)> = (16..24).map(|i| (i, 100 + i as i32)).collect();
        let jump: Vec<(u64, i32)> = (16..24).map(|i| (i, 900 + i as i32)).collect();
        let t = 16.0;
        assert!(seam(&smooth, &base, cols, t).unwrap() < 1.0);
        assert!(seam(&jump, &base, cols, t).unwrap() > 10.0);
    }
}
