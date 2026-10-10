//! 物理条件のシミュレーション。
//!
//! 実機がなくても、不良セクタ・遅延・HDDのシークコスト・生デバイスの境界制約を
//! 再現してテストするためのラッパー。すべてシードから決定的に動く。

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

/// 生デバイスと同じく、位置か長さがセクタ境界に揃っていない読み込みを拒否する。
///
/// [`Aligned`](crate::Aligned) のテストや、上位の層が生デバイスの制約を
/// 守っているかの確認に使う。
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

/// 障害の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// 毎回失敗する。
    Bad,
    /// 最初の `failures` 回だけ失敗し、その後は読める（リトライで救える）。
    Weak { failures: u32 },
    /// 応答せず、[`Latency::timeout`] だけ時間を消費して失敗する。
    Timeout,
}

/// 障害を起こす範囲（セクタ番号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    pub sectors: Range<u64>,
    pub kind: FaultKind,
}

/// 読み込みにかかる時間のモデル。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latency {
    /// 1回の読み込みごとの固定コスト。
    pub per_read: Duration,
    /// 転送速度（バイト/秒）。0なら転送時間なし。
    pub bytes_per_sec: u64,
    /// シーク1回のコスト。
    pub seek: Duration,
    /// 直前の読み込みの終端からこれより離れた位置を読むとシークとみなす（バイト）。
    pub seek_threshold: u64,
    /// 失敗した読み込み（[`FaultKind::Bad`]・[`FaultKind::Weak`]）の追加コスト。
    pub error: Duration,
    /// [`FaultKind::Timeout`] のコスト。
    pub timeout: Duration,
}

impl Latency {
    /// 時間がかからない。
    pub const NONE: Self = Self {
        per_read: Duration::ZERO,
        bytes_per_sec: 0,
        seek: Duration::ZERO,
        seek_threshold: u64::MAX,
        error: Duration::ZERO,
        timeout: Duration::ZERO,
    };

    /// 7200rpm級のHDDのおおよその値。
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

/// シミュレーション上の経過時間。複製しても同じ時計を指す。
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

/// 障害注入ラッパー。
///
/// 不良セクタなどの障害と、読み込みにかかる時間を模擬する。時間は既定では
/// [`VirtualClock`] に積算するだけで実際には待たない（[`Faulty::real_sleep`] で待つ）。
#[derive(Debug)]
pub struct Faulty<S> {
    inner: S,
    /// 開始セクタ順に並び、互いに重ならない。
    faults: Vec<Fault>,
    /// `faults` と同じ並び。[`FaultKind::Weak`] がこれまでに失敗した回数。
    weak_failures: Mutex<Vec<u32>>,
    latency: Latency,
    real_sleep: bool,
    clock: VirtualClock,
    /// 直前の読み込みの終端（シーク判定用）。
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

    /// 障害を追加する。
    ///
    /// # Panics
    /// 範囲が空の場合、または既存の障害と重なる場合。
    pub fn fault(mut self, sectors: Range<u64>, kind: FaultKind) -> Self {
        assert!(!sectors.is_empty(), "障害の範囲が空: {sectors:?}");
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
            "障害の範囲が重なっている: {sectors:?}"
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

    /// シードから決定的に、不良セクタの連なりを `runs` 個まき散らす。
    /// 各連なりの長さは 1〜`max_run` セクタ。既存の障害と重なるものは捨てる。
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

    /// 模擬した時間だけ実際にスレッドを止める。
    pub fn real_sleep(mut self, yes: bool) -> Self {
        self.real_sleep = yes;
        self
    }

    /// 注入した障害の一覧（正解データとして評価に使う）。
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

    /// セクタ範囲 `s0..s1` に最初に引っかかる障害を返す。
    /// [`FaultKind::Weak`] はここで失敗回数を数え、回数を使い切っていれば素通りさせる。
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
                ReadErrorKind::Io(io::Error::other("模擬した不良セクタ"))
            }
        };
        self.spend(cost);
        Err(ReadError::new(range, kind))
    }
}

/// 依存を増やさないための小さな擬似乱数（SplitMix64）。
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
