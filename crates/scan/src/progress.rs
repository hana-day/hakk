//! Progress reporting: events, the disk map, speed and remaining time.

use std::{ops::Range, time::Duration};

use crate::ranges::{RangeMap, RangeSet};

/// Whether the scan is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    /// Paused by the user.
    Paused,
    /// Paused because a restore (or preview) is reading; resumes at the same position when
    /// it ends.
    PausedForRestore,
}

/// What the scan is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Reading metadata analyzers asked for before the main pass.
    Metadata,
    /// The main pass: reading the planned ranges front to back.
    Scanning,
    /// Reading what analyzers deferred, and waiting for them to finish.
    Finishing,
    /// Retrying bad ranges (`pass` starts at 1).
    Retrying {
        pass: u32,
    },
    Done,
}

/// State of one cell of the disk map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellState {
    /// Not part of this scan (e.g. allocated space in a free-space scan).
    Skipped,
    Unread,
    Read,
    /// Read, and a file starts here.
    Found,
    /// Contains a range that could not be read.
    Error,
}

/// The disk drawn as a row of cells, each covering [`DiskMap::cell_bytes`] bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskMap {
    pub cells: Vec<CellState>,
    pub cell_bytes: u64,
}

/// Number of files found so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FoundCounts {
    pub photos: u64,
    pub videos: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub state: RunState,
    pub phase: Phase,
    /// Bytes the scan covers.
    pub planned_bytes: u64,
    /// Bytes of the plan already read (including bytes found to be bad).
    pub scanned_bytes: u64,
    /// Where the main pass is reading.
    pub position: u64,
    /// Smoothed read speed of the main pass. `None` until measurable, and while paused.
    pub bytes_per_sec: Option<f64>,
    /// Estimated time until the main pass finishes.
    pub remaining: Option<Duration>,
    /// Time spent so far, by the scan's clock.
    pub elapsed: Duration,
    pub found: FoundCounts,
    /// Bytes currently known to be unreadable.
    pub bad_bytes: u64,
    /// Set while retrying bad ranges ([`Phase::Retrying`]).
    pub retry: Option<RetryProgress>,
    pub map: DiskMap,
}

/// Progress of the retry phase. Speed and remaining time are not meaningful here (each
/// failed read can take seconds), so they are `None` while retrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryProgress {
    /// The current pass, starting at 1.
    pub pass: u32,
    /// Passes configured ([`crate::Scan::retry_passes`]).
    pub passes: u32,
    /// Bytes still unreadable.
    pub bytes_left: u64,
}

impl Progress {
    /// Fraction of the main pass done, 0.0 to 1.0.
    pub fn fraction(&self) -> f64 {
        if self.planned_bytes == 0 {
            1.0
        } else {
            self.scanned_bytes as f64 / self.planned_bytes as f64
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Cancelled,
}

/// The result of a scan.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub outcome: Outcome,
    pub planned_bytes: u64,
    pub scanned_bytes: u64,
    /// Bytes still unreadable after the retries.
    pub bad_bytes: u64,
    pub retry_passes: u32,
    pub found: FoundCounts,
    pub elapsed: Duration,
}

/// Something that happened during a scan. Received from [`crate::ScanHandle::events`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Progress(Box<Progress>),
    /// An analyzer panicked. Its output for that piece of data is lost; the scan goes on.
    AnalyzerPanicked {
        analyzer: String,
        offset: Option<u64>,
    },
    Finished(Summary),
}

/// Keeps the per-cell figures behind [`DiskMap`].
#[derive(Debug)]
pub(crate) struct MapTracker {
    cell_bytes: u64,
    planned: Vec<u64>,
    read: Vec<u64>,
    found: Vec<bool>,
    error: Vec<bool>,
}

impl MapTracker {
    pub(crate) fn new(source_len: u64, cells: usize, planned: &RangeSet) -> Self {
        let cells = u64::try_from(cells.max(1))
            .unwrap_or(u64::MAX)
            .min(source_len.max(1));
        let cell_bytes = source_len.div_ceil(cells).max(1);
        let n = usize::try_from(source_len.div_ceil(cell_bytes).max(1)).unwrap_or(1);
        let mut t = Self {
            cell_bytes,
            planned: vec![0; n],
            read: vec![0; n],
            found: vec![false; n],
            error: vec![false; n],
        };
        for r in planned.ranges() {
            t.for_cells(r, |t, i, len| {
                if let Some(p) = t.planned.get_mut(i) {
                    *p += len;
                }
            });
        }
        t
    }

    /// Calls `f(self, cell index, overlap length)` for each cell `range` touches.
    fn for_cells(&mut self, range: Range<u64>, mut f: impl FnMut(&mut Self, usize, u64)) {
        if range.is_empty() {
            return;
        }
        let first = range.start / self.cell_bytes;
        let last = (range.end - 1) / self.cell_bytes;
        for cell in first..=last {
            let cell_start = cell * self.cell_bytes;
            let cell_end = cell_start.saturating_add(self.cell_bytes);
            let overlap = range.end.min(cell_end) - range.start.max(cell_start);
            if let Ok(i) = usize::try_from(cell) {
                f(self, i, overlap);
            }
        }
    }

    pub(crate) fn mark_read(&mut self, range: Range<u64>) {
        self.for_cells(range, |t, i, len| {
            if let Some(r) = t.read.get_mut(i) {
                *r += len;
            }
        });
    }

    pub(crate) fn mark_found(&mut self, offset: u64) {
        let i = usize::try_from(offset / self.cell_bytes).unwrap_or(usize::MAX);
        if let Some(f) = self.found.get_mut(i) {
            *f = true;
        }
    }

    /// Recomputes the error flag of the cells `range` touches from the bad set.
    pub(crate) fn refresh_errors(&mut self, range: Range<u64>, bad: &RangeMap<u32>) {
        let cell_bytes = self.cell_bytes;
        self.for_cells(range, |t, i, _| {
            let start = i as u64 * cell_bytes;
            let any = bad.overlap_len(start..start.saturating_add(cell_bytes)) > 0;
            if let Some(e) = t.error.get_mut(i) {
                *e = any;
            }
        });
    }

    pub(crate) fn snapshot(&self) -> DiskMap {
        let cells = (0..self.planned.len())
            .map(|i| {
                let planned = self.planned.get(i).copied().unwrap_or(0);
                let read = self.read.get(i).copied().unwrap_or(0);
                if self.error.get(i).copied().unwrap_or(false) {
                    CellState::Error
                } else if self.found.get(i).copied().unwrap_or(false) {
                    CellState::Found
                } else if planned == 0 {
                    CellState::Skipped
                } else if read >= planned {
                    CellState::Read
                } else {
                    CellState::Unread
                }
            })
            .collect();
        DiskMap {
            cells,
            cell_bytes: self.cell_bytes,
        }
    }
}

/// Exponentially smoothed read speed.
#[derive(Debug, Default)]
pub(crate) struct RateMeter {
    last: Option<(Duration, u64)>,
    rate: Option<f64>,
}

impl RateMeter {
    /// Weight of the newest sample.
    const ALPHA: f64 = 0.3;

    pub(crate) fn sample(&mut self, now: Duration, bytes: u64) {
        if let Some((t0, b0)) = self.last {
            let dt = now.saturating_sub(t0).as_secs_f64();
            if dt <= 0.0 {
                return;
            }
            let inst = bytes.saturating_sub(b0) as f64 / dt;
            self.rate = Some(match self.rate {
                Some(r) => r + Self::ALPHA * (inst - r),
                None => inst,
            });
        }
        self.last = Some((now, bytes));
    }

    /// Forget the history (after a pause, so the pause does not drag the speed down).
    pub(crate) fn reset(&mut self) {
        self.last = None;
        self.rate = None;
    }

    pub(crate) fn rate(&self) -> Option<f64> {
        self.rate.filter(|r| r.is_finite() && *r > 0.0)
    }

    pub(crate) fn remaining(&self, bytes_left: u64) -> Option<Duration> {
        let rate = self.rate()?;
        Duration::try_from_secs_f64(bytes_left as f64 / rate).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_cells() {
        let planned: RangeSet = [0..400, 600..1000].into_iter().collect();
        let mut t = MapTracker::new(1000, 10, &planned);
        t.mark_read(0..250);
        t.mark_found(120);
        let mut bad = RangeMap::new();
        bad.insert(310..320, 1);
        t.refresh_errors(310..320, &bad);
        use CellState::*;
        assert_eq!(
            t.snapshot().cells,
            [
                Read, Found, Unread, Error, Skipped, Skipped, Unread, Unread, Unread, Unread
            ]
        );
        bad.remove(310..320);
        t.refresh_errors(310..320, &bad);
        assert_eq!(t.snapshot().cells.get(3), Some(&Unread));
    }

    #[test]
    fn rate_and_remaining() {
        let mut m = RateMeter::default();
        m.sample(Duration::from_secs(0), 0);
        assert_eq!(m.rate(), None);
        m.sample(Duration::from_secs(1), 100);
        assert_eq!(m.rate(), Some(100.0));
        assert_eq!(m.remaining(500), Some(Duration::from_secs(5)));
        m.reset();
        assert_eq!(m.remaining(500), None);
    }
}
