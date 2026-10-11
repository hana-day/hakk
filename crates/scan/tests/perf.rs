//! HDD-scale behaviour, measured on a simulated disk.
//!
//! The source is `blocksource::sim::Faulty` with `Latency::HDD` (150 MB/s, 8 ms per seek,
//! 0.5 s per failed read). Time is only accumulated on a `VirtualClock`, so these tests
//! estimate how long a real HDD would take without waiting for it.
#![allow(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::single_range_in_vec_init,
    clippy::unwrap_used
)]

mod common;

use std::{ops::Range, sync::Arc, time::Duration};

use blocksource::{
    BlockSource, MemSource,
    sim::{FaultKind, Faulty, Latency, VirtualClock},
};
use common::*;
use scan::{Event, MemStore, Outcome, Phase, Priority, Scan};

const MIB: u64 = 1 << 20;
const BLOCK: usize = 1 << 20;
/// Same rule as `Latency::HDD`: a jump of more than 1 MiB is a seek.
const SEEK_THRESHOLD: u64 = MIB;

type Disk = Recorder<Faulty<MemSource>>;

fn hdd(data: Vec<u8>) -> Arc<Disk> {
    hdd_with(Faulty::new(MemSource::new(data)).latency(Latency::HDD))
}

fn hdd_with(faulty: Faulty<MemSource>) -> Arc<Disk> {
    Arc::new(Recorder::new(faulty, SEEK_THRESHOLD))
}

fn clock(disk: &Disk) -> VirtualClock {
    disk.inner().clock().clone()
}

/// Ideal time to read `bytes` front to back in `reads` reads with no seek.
fn sequential_time(bytes: u64, reads: u64) -> Duration {
    let l = Latency::HDD;
    Duration::from_secs_f64(bytes as f64 / l.bytes_per_sec as f64) + l.per_read * reads as u32
}

#[test]
fn sequential_scan_reads_once_without_seeking() {
    let len = 64 * MIB;
    let disk = hdd(vec![0; len as usize]);
    let clock = clock(&disk);
    let handle = Scan::new(disk.clone())
        .analyzer(SigCarver)
        .analyzer(SigCarver) // a second analyzer shares the same buffers
        .block_size(BLOCK)
        .workers(2)
        .clock(clock.clone())
        .progress_interval(Duration::from_millis(50))
        .start(MemStore::new())
        .unwrap();
    let (summary, events) = finish(handle);
    assert_eq!(summary.outcome, Outcome::Completed);

    let log = disk.log();
    assert_eq!(disk.seeks(), 0);
    assert_eq!(log.len() as u64, len / MIB, "one read per block");
    assert_eq!(disk.bytes_read(), len, "every byte read exactly once");

    let ideal = sequential_time(len, len / MIB);
    let took = clock.elapsed();
    eprintln!("64 MiB sequential: {took:?} simulated (ideal {ideal:?})");
    assert!(
        took <= ideal + Duration::from_millis(1),
        "{took:?} vs {ideal:?}"
    );

    // The reported speed matches the simulated disk.
    let rates: Vec<f64> = events
        .iter()
        .filter_map(|e| match e {
            Event::Progress(p) if p.phase == Phase::Scanning => p.bytes_per_sec,
            _ => None,
        })
        .collect();
    let rate = *rates.last().expect("no speed reported");
    let disk_rate = Latency::HDD.bytes_per_sec as f64;
    assert!(
        (rate - disk_rate).abs() / disk_rate < 0.25,
        "reported {rate:.0} B/s, disk does {disk_rate:.0} B/s"
    );
}

#[test]
fn restore_during_scan_costs_two_seeks_and_resumes_in_place() {
    let len = 64 * MIB;
    let mut data = filler(len as usize, 7);
    data.truncate(len as usize);
    let disk = hdd(data.clone());
    let clock = clock(&disk);
    let gate = Gate::closed();
    let handle = Scan::new(disk.clone())
        .analyzer(SigCarver)
        .analyzer(GateAt {
            at: 8 * MIB,
            gate: gate.clone(),
        })
        .block_size(BLOCK)
        .workers(1)
        .clock(clock.clone())
        .start(MemStore::new())
        .unwrap();

    // Hold the analyzers so the scan is mid-way when the restore starts.
    gate.until_waiting();
    std::thread::sleep(Duration::from_millis(50)); // let the reader fill the queue
    let before = disk.log().len();

    // Restore a 4 MiB file far ahead of the scan, in 1 MiB reads.
    let file = 48 * MIB..52 * MIB;
    let fg = handle.foreground();
    let mut restored = Vec::new();
    for off in (file.start..file.end).step_by(MIB as usize) {
        let mut buf = vec![0u8; MIB as usize];
        fg.read_exact_at(off, &mut buf).unwrap();
        restored.extend(buf);
    }
    drop(fg);
    let after = disk.log().len();
    gate.open();
    handle.wait().unwrap();

    assert_eq!(restored, data[file.start as usize..file.end as usize]);
    let log = disk.log();
    let restore_reads = &log[before..after];
    assert_eq!(restore_reads.len(), 4, "the scan read during the restore");
    assert_eq!(restore_reads[0].offset, file.start);

    // The scan's own reads are one contiguous sweep: it resumed exactly where it stopped.
    let mut pos = 0;
    for r in log[..before].iter().chain(&log[after..]) {
        assert_eq!(r.offset, pos, "scan jumped");
        pos += r.len;
    }
    assert_eq!(pos, len);
    // One seek to the file and one back.
    assert_eq!(disk.seeks(), 2);

    // Without one place ordering reads, the restore would interleave with the scan and
    // seek twice per restore read.
    let naive = hdd(vec![0; len as usize]);
    let mut scan_pos = 0;
    let mut buf = vec![0u8; MIB as usize];
    for off in (file.start..file.end).step_by(MIB as usize) {
        naive.read_at(off, &mut buf).unwrap();
        naive.read_at(scan_pos, &mut buf).unwrap();
        scan_pos += MIB;
    }
    while scan_pos < len {
        naive.read_at(scan_pos, &mut buf).unwrap();
        scan_pos += MIB;
    }
    let ours = clock.elapsed();
    let theirs = naive.inner().clock().elapsed();
    eprintln!(
        "restore during scan: {ours:?} with {} seeks; interleaved {theirs:?} with {} seeks",
        disk.seeks(),
        naive.seeks()
    );
    assert_eq!(naive.seeks(), 8);
    assert!(ours < theirs);
}

#[test]
fn bad_sectors_are_skipped_then_retried_at_the_end() {
    let len = 32 * MIB;
    let sectors = len / 512;
    let weak: [Range<u64>; 3] = [1000..1004, 30_000..30_002, 50_000..50_008];
    let mut faulty = Faulty::new(MemSource::new(vec![0; len as usize])).latency(Latency::HDD);
    for w in &weak {
        faulty = faulty.fault(w.clone(), FaultKind::Weak { failures: 1 });
    }
    let faulty = faulty.random_bad(7, 200, 16);
    let bad_faults: Vec<Range<u64>> = faulty
        .faults()
        .iter()
        .filter(|f| f.kind == FaultKind::Bad)
        .map(|f| f.sectors.start * 512..f.sectors.end * 512)
        .collect();
    let bad_bytes: u64 = bad_faults.iter().map(|r| r.end - r.start).sum();
    let n_faults = faulty.faults().len();
    assert!(sectors > 0 && n_faults > 150, "{n_faults} faults");

    let disk = hdd_with(faulty);
    let clock = clock(&disk);
    let store = MemStore::new();
    let handle = Scan::new(disk.clone())
        .analyzer(SigCarver)
        .block_size(BLOCK)
        .retry_passes(2)
        .clock(clock.clone())
        .start(store.clone())
        .unwrap();
    let (summary, events) = finish(handle);
    assert_eq!(summary.outcome, Outcome::Completed);

    // While retrying, progress names the pass and points at the area being retried.
    let retrying: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Progress(p) if matches!(p.phase, Phase::Retrying { .. }) => Some(p),
            _ => None,
        })
        .collect();
    assert!(!retrying.is_empty());
    for p in &retrying {
        let r = p.retry.expect("retry progress");
        assert!(matches!(p.phase, Phase::Retrying { pass } if pass == r.pass));
        assert_eq!(r.passes, 2);
        assert_eq!(r.bytes_left, p.bad_bytes);
        assert_eq!((p.bytes_per_sec, p.remaining), (None, None));
    }

    // The main pass only moved forward and reached the end before any retry.
    let log = disk.log();
    let first_back = log
        .windows(2)
        .position(|w| w[1].offset < w[0].offset)
        .map_or(log.len(), |i| i + 1);
    let main_pass = &log[..first_back];
    assert_eq!(main_pass.iter().map(|r| r.offset + r.len).max(), Some(len));
    // Retries touched only ranges that had failed.
    let failed: Vec<Range<u64>> = main_pass
        .iter()
        .filter(|r| !r.ok)
        .map(|r| r.offset..r.offset + r.len)
        .collect();
    for r in &log[first_back..] {
        assert!(
            failed
                .iter()
                .any(|f| f.start <= r.offset && r.offset < f.end),
            "retry outside a failed range: {r:?}"
        );
    }

    // Weak sectors were recovered; exactly the bad ones remain, each tried three times.
    let saved = store.data();
    let remaining: Vec<Range<u64>> = saved.progress.bad.iter().map(|b| b.range.clone()).collect();
    let mut expect = scan::RangeSet::new();
    for r in &bad_faults {
        expect.add(r.clone());
    }
    assert_eq!(remaining, expect.to_vec());
    assert!(saved.progress.bad.iter().all(|b| b.attempts == 3));
    assert_eq!(summary.bad_bytes, bad_bytes);
    assert_eq!(summary.retry_passes, 2);
    let done: u64 = saved.progress.done.iter().map(|r| r.end - r.start).sum();
    assert_eq!(done, len - bad_bytes, "everything readable was analyzed");

    // No sector-by-sector probing: about one failed read per fault per pass.
    let failed_reads = log.iter().filter(|r| !r.ok).count();
    assert!(
        failed_reads <= 3 * n_faults + 16,
        "{failed_reads} failed reads for {n_faults} faults"
    );
    let ideal = sequential_time(len, len / MIB);
    let errors = Latency::HDD.error * failed_reads as u32;
    let took = clock.elapsed();
    eprintln!(
        "32 MiB with {n_faults} faults: {took:?} simulated ({failed_reads} failed reads, \
         {} seeks; error-free pass {ideal:?})",
        disk.seeks()
    );
    // All the time is accounted for by transfers, failed reads and two seeks (out to the
    // retries is not needed: the main pass ends near them; back is). Nothing else waits.
    let transfers = sequential_time(disk.bytes_read(), log.len() as u64);
    let expected = transfers + errors + Latency::HDD.seek * disk.seeks() as u32;
    let diff = took.abs_diff(expected);
    assert!(diff < Duration::from_millis(1), "{took:?} vs {expected:?}");
    // Errors dominate; the main pass did not grind on them (at most 4 per block).
    let main_errors = main_pass.iter().filter(|r| !r.ok).count();
    assert!(main_errors <= 4 * (len / MIB) as usize, "{main_errors}");
}

#[test]
fn free_space_scan_seeks_once_per_gap() {
    let len = 64 * MIB;
    let disk = hdd(vec![0; len as usize]);
    let ranges: Vec<Range<u64>> = (0..8)
        .map(|i| {
            let start = 2 * MIB + i * 8 * MIB;
            start..start + MIB / 2
        })
        .collect();
    let handle = Scan::new(disk.clone())
        .analyzer(SigCarver)
        .block_size(BLOCK)
        .ranges(ranges.clone())
        .start(MemStore::new())
        .unwrap();
    let (summary, _) = finish(handle);
    assert_eq!(disk.bytes_read(), 8 * MIB / 2);
    assert_eq!(summary.scanned_bytes, 8 * MIB / 2);
    assert_eq!(disk.seeks(), ranges.len());
}

#[test]
fn metadata_reads_cost_seeks_only_when_immediate() {
    let len = 64 * MIB;
    let meta = len - MIB..len - MIB + 4096;
    let run = |priority| {
        let disk = hdd(vec![0; len as usize]);
        let handle = Scan::new(disk.clone())
            .analyzer(MetaReader {
                range: meta.clone(),
                priority,
            })
            .block_size(BLOCK)
            .start(MemStore::new())
            .unwrap();
        let (summary, _) = finish(handle);
        assert_eq!(summary.found.videos, 1);
        (disk.log(), disk.seeks())
    };

    // Read first (named files show up immediately): there and back.
    let (log, seeks) = run(Priority::Immediate);
    assert_eq!(log[0].offset, meta.start);
    assert_eq!(seeks, 2);

    // Deferred: read after the main pass, which ends right next to it.
    let (log, seeks) = run(Priority::Deferred);
    assert_eq!(log.last().map(|r| r.offset), Some(meta.start));
    assert_eq!(seeks, 0);
}

/// Reports every failure as the whole request, like a device that gives no detail.
struct Imprecise<S>(S);

impl<S: BlockSource> BlockSource for Imprecise<S> {
    fn len(&self) -> u64 {
        self.0.len()
    }

    fn sector_size(&self) -> u32 {
        self.0.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, blocksource::ReadError> {
        self.0.read_at(offset, buf).map_err(|e| {
            blocksource::ReadError::new(offset..offset + buf.len() as u64, e.into_kind())
        })
    }
}

#[test]
fn imprecise_errors_are_narrowed_down_to_the_bad_sectors() {
    let len = 4 * MIB;
    let faulty = Faulty::new(MemSource::new(vec![0; len as usize]))
        .latency(Latency::HDD)
        .bad(100..101)
        .bad(3000..3003)
        .bad(7000..7001);
    let clock = faulty.clock().clone();
    let disk = Arc::new(Recorder::new(Imprecise(faulty), SEEK_THRESHOLD));
    let store = MemStore::new();
    let handle = Scan::new(disk.clone())
        .block_size(BLOCK)
        .retry_passes(1)
        .clock(clock.clone())
        .start(store.clone())
        .unwrap();
    let (summary, _) = finish(handle);
    let bad: Vec<Range<u64>> = store
        .data()
        .progress
        .bad
        .iter()
        .map(|b| b.range.clone())
        .collect();
    assert_eq!(
        bad,
        [
            100 * 512..101 * 512,
            3000 * 512..3003 * 512,
            7000 * 512..7001 * 512
        ]
    );
    assert_eq!(summary.bad_bytes, 5 * 512);
    let failed = disk.log().iter().filter(|r| !r.ok).count();
    eprintln!(
        "imprecise errors: {failed} failed reads, {:?} simulated",
        clock.elapsed()
    );
    // Narrowing a whole failed block down to sectors takes a few dozen reads, not one
    // per sector (2048 per MiB).
    assert!(failed < 100, "{failed} failed reads");
}
