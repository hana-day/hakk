//! Simulation of physical conditions.
//!
//! Wrappers that reproduce bad sectors, latency, HDD seek cost and raw-device alignment
//! constraints so they can be tested without real hardware. Everything is deterministic
//! given a seed.

use std::{
    io,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::{BlockSource, ReadError, ReadErrorKind, readable_len};

/// Rejects reads whose offset or length is not sector-aligned, like a raw device.
///
/// Used to test [`Aligned`](crate::Aligned) and to check that upper layers respect
/// raw-device constraints.
#[derive(Debug)]
pub struct StrictAlign<S> {
    inner: S,
}

impl<S: BlockSource> StrictAlign<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

impl<S: BlockSource> BlockSource for StrictAlign<S> {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        let ss = u64::from(self.sector_size());
        let len = buf.len() as u64;
        if !offset.is_multiple_of(ss) || !len.is_multiple_of(ss) {
            let range = offset..offset.saturating_add(len);
            return Err(ReadError::new(range, ReadErrorKind::Misaligned));
        }
        self.inner.read_at(offset, buf)
    }
}

/// The kind of fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// Fails every time.
    Bad,
    /// Fails the first `failures` times, then reads fine (recoverable by retrying).
    Weak { failures: u32 },
    /// Does not respond; fails after spending [`Latency::timeout`].
    Timeout,
}

/// A faulty range (in sectors).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub sectors: Range<u64>,
    pub kind: FaultKind,
}

/// A model of how long reads take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latency {
    /// Fixed cost per read.
    pub per_read: Duration,
    /// Transfer rate in bytes per second. 0 means transfers take no time.
    pub bytes_per_sec: u64,
    /// Cost of one seek.
    pub seek: Duration,
    /// A read further than this many bytes from the end of the previous read counts as a seek.
    pub seek_threshold: u64,
    /// Extra cost of a failed read ([`FaultKind::Bad`], [`FaultKind::Weak`]).
    pub error: Duration,
    /// Cost of a [`FaultKind::Timeout`].
    pub timeout: Duration,
}

impl Latency {
    /// Nothing takes any time.
    pub const NONE: Self = Self {
        per_read: Duration::ZERO,
        bytes_per_sec: 0,
        seek: Duration::ZERO,
        seek_threshold: u64::MAX,
        error: Duration::ZERO,
        timeout: Duration::ZERO,
    };

    /// Rough figures for a 7200 rpm HDD.
    pub const HDD: Self = Self {
        per_read: Duration::from_micros(50),
        bytes_per_sec: 150_000_000,
        seek: Duration::from_millis(8),
        seek_threshold: 1 << 20,
        error: Duration::from_millis(500),
        timeout: Duration::from_secs(5),
    };

    fn transfer(&self, bytes: u64) -> Duration {
        if self.bytes_per_sec == 0 {
            return Duration::ZERO;
        }
        let nanos = u128::from(bytes) * 1_000_000_000 / u128::from(self.bytes_per_sec);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

/// Simulated elapsed time. Clones share the same clock.
#[derive(Debug, Clone, Default)]
pub struct VirtualClock {
    nanos: Arc<AtomicU64>,
}

impl VirtualClock {
    pub fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::Relaxed))
    }

    fn advance(&self, d: Duration) {
        let nanos = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        self.nanos.fetch_add(nanos, Ordering::Relaxed);
    }
}

/// Fault injection wrapper.
///
/// Simulates faults such as bad sectors, and the time reads take. By default time is only
/// accumulated on a [`VirtualClock`] without actually waiting (see [`Faulty::real_sleep`]).
#[derive(Debug)]
pub struct Faulty<S> {
    inner: S,
    /// Sorted by start sector; never overlapping.
    faults: Vec<Fault>,
    /// Parallel to `faults`: how many times each [`FaultKind::Weak`] has failed so far.
    weak_failures: Mutex<Vec<u32>>,
    latency: Latency,
    real_sleep: bool,
    clock: VirtualClock,
    /// End of the previous read (for seek detection).
    head: AtomicU64,
}

impl<S: BlockSource> Faulty<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            faults: Vec::new(),
            weak_failures: Mutex::new(Vec::new()),
            latency: Latency::NONE,
            real_sleep: false,
            clock: VirtualClock::default(),
            head: AtomicU64::new(0),
        }
    }

    /// Adds a fault.
    ///
    /// # Panics
    /// If the range is empty or overlaps an existing fault.
    pub fn fault(mut self, sectors: Range<u64>, kind: FaultKind) -> Self {
        assert!(!sectors.is_empty(), "empty fault range: {sectors:?}");
        let i = self
            .faults
            .partition_point(|f| f.sectors.start < sectors.start);
        let overlaps_prev = i
            .checked_sub(1)
            .and_then(|p| self.faults.get(p))
            .is_some_and(|p| p.sectors.end > sectors.start);
        let overlaps_next = self
            .faults
            .get(i)
            .is_some_and(|n| n.sectors.start < sectors.end);
        assert!(
            !overlaps_prev && !overlaps_next,
            "overlapping fault ranges: {sectors:?}"
        );
        self.faults.insert(i, Fault { sectors, kind });
        self.weak_failures
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .insert(i, 0);
        self
    }

    pub fn bad(self, sectors: Range<u64>) -> Self {
        self.fault(sectors, FaultKind::Bad)
    }

    /// Deterministically scatters `runs` runs of bad sectors from `seed`.
    /// Each run is 1 to `max_run` sectors long. Runs overlapping existing faults are dropped.
    pub fn random_bad(mut self, seed: u64, runs: usize, max_run: u64) -> Self {
        let sectors = self.inner.len() / u64::from(self.inner.sector_size());
        if sectors == 0 || max_run == 0 {
            return self;
        }
        let mut rng = SplitMix64(seed);
        for _ in 0..runs {
            let start = rng.next() % sectors;
            let len = 1 + rng.next() % max_run;
            let end = start.saturating_add(len).min(sectors);
            let overlaps = self
                .faults
                .iter()
                .any(|f| f.sectors.start < end && start < f.sectors.end);
            if !overlaps {
                self = self.bad(start..end);
            }
        }
        self
    }

    pub fn latency(mut self, latency: Latency) -> Self {
        self.latency = latency;
        self
    }

    /// Actually sleep the thread for the simulated time.
    pub fn real_sleep(mut self, yes: bool) -> Self {
        self.real_sleep = yes;
        self
    }

    /// The injected faults (ground truth for evaluation).
    pub fn faults(&self) -> &[Fault] {
        &self.faults
    }

    pub fn clock(&self) -> &VirtualClock {
        &self.clock
    }

    fn spend(&self, d: Duration) {
        self.clock.advance(d);
        if self.real_sleep && !d.is_zero() {
            std::thread::sleep(d);
        }
    }

    /// Returns the first fault hit by sectors `s0..s1`.
    /// [`FaultKind::Weak`] failures are counted here; exhausted ones are let through.
    fn hit(&self, s0: u64, s1: u64) -> Option<&Fault> {
        let first = self.faults.partition_point(|f| f.sectors.end <= s0);
        let mut weak = self.weak_failures.lock().unwrap_or_else(|e| e.into_inner());
        for (i, f) in self.faults.iter().enumerate().skip(first) {
            if f.sectors.start >= s1 {
                break;
            }
            match f.kind {
                FaultKind::Weak { failures } => {
                    if let Some(count) = weak.get_mut(i).filter(|c| **c < failures) {
                        *count += 1;
                        return Some(f);
                    }
                }
                FaultKind::Bad | FaultKind::Timeout => return Some(f),
            }
        }
        None
    }
}

impl<S: BlockSource> BlockSource for Faulty<S> {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        let want = readable_len(self.len(), offset, buf.len());
        let Some(dst) = buf.get_mut(..want).filter(|d| !d.is_empty()) else {
            return Ok(0);
        };
        let end = offset + want as u64;

        let last = self.head.swap(end, Ordering::Relaxed);
        let mut cost = self.latency.per_read + self.latency.transfer(want as u64);
        if offset.abs_diff(last) > self.latency.seek_threshold {
            cost += self.latency.seek;
        }

        let ss = u64::from(self.sector_size());
        let (s0, s1) = (offset / ss, end.div_ceil(ss));
        let Some(fault) = self.hit(s0, s1) else {
            self.spend(cost);
            return self.inner.read_at(offset, dst);
        };

        let range = fault.sectors.start.max(s0) * ss..fault.sectors.end.min(s1) * ss;
        let kind = match fault.kind {
            FaultKind::Timeout => {
                cost += self.latency.timeout;
                ReadErrorKind::Timeout
            }
            FaultKind::Bad | FaultKind::Weak { .. } => {
                cost += self.latency.error;
                ReadErrorKind::Io(io::Error::other("simulated bad sector"))
            }
        };
        self.spend(cost);
        Err(ReadError::new(range, kind))
    }
}

/// A tiny PRNG (SplitMix64) to avoid extra dependencies.
///
/// Reference: Steele, Lea, Flood, "Fast Splittable Pseudorandom Number Generators", OOPSLA 2014.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}
