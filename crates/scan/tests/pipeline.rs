#![allow(clippy::single_range_in_vec_init, clippy::expect_used)]

mod common;

use std::{ops::Range, sync::Arc, time::Duration};

use blocksource::MemSource;
use common::*;
use scan::{
    Event, MemStore, Outcome, Phase, Priority, RunState, Scan, ScanError, Store, StoreError,
    index::{Index, Media, Origin, Query, ScanPhase, ScanProgress, SourceInfo},
};

const BLOCK: usize = 64 << 10;

fn source_with(len: usize, sigs: &[u64]) -> (Arc<Vec<u8>>, Arc<MemSource>) {
    let mut data = filler(len, 42);
    plant(&mut data, sigs);
    let data = Arc::new(data);
    (
        data.clone(),
        Arc::new(MemSource::new(data.as_ref().clone())),
    )
}

/// Signatures straddling block boundaries at every split point, plus some inside blocks.
fn boundary_sigs() -> Vec<u64> {
    let mut v = Vec::new();
    for (i, k) in (0..=SIG.len() as u64).enumerate() {
        let boundary = (i as u64 + 1) * BLOCK as u64;
        v.push(boundary - k);
    }
    v.extend([100, 3 * BLOCK as u64 + 517, 15 * BLOCK as u64 + 9]);
    v.sort_unstable();
    v
}

#[test]
fn signatures_across_block_boundaries_are_found_exactly_once() {
    let sigs = boundary_sigs();
    let (_, src) = source_with(16 * BLOCK, &sigs);
    let store = MemStore::new();
    let handle = Scan::new(src)
        .analyzer(SigCarver)
        .block_size(BLOCK)
        .workers(4)
        .start(store.clone())
        .unwrap();
    let (summary, _) = finish(handle);
    assert_eq!(summary.outcome, Outcome::Completed);
    assert_eq!(carved_offsets(&store.data().found), sigs);
    assert_eq!(summary.found.photos, sigs.len() as u64);
}

#[test]
fn every_byte_is_delivered_once_through_a_shared_buffer() {
    let len = 10 * BLOCK + 4096; // a short last block
    let (data, src) = source_with(len, &[]);
    let cov = Coverage::new(data);
    let store = MemStore::new();
    let handle = Scan::new(src)
        .analyzer(cov.clone())
        .analyzer(SigCarver)
        .block_size(BLOCK)
        .start(store.clone())
        .unwrap();
    finish(handle);
    let ranges: Vec<Range<u64>> = cov.ranges().into_iter().map(|(r, _)| r).collect();
    let mut expect = 0;
    for r in &ranges {
        assert_eq!(r.start, expect, "gap or overlap at {r:?}");
        expect = r.end;
    }
    assert_eq!(expect, len as u64);
    assert_eq!(*cov.mismatches.lock().unwrap(), 0);
    let d = store.data();
    assert_eq!(d.progress.done, [0..len as u64]);
    assert_eq!(d.progress.phase, ScanPhase::Complete);
}

#[test]
fn free_space_mode_reads_only_the_given_ranges() {
    let sigs = [10_000, 200_000, 300_000 - 3];
    let (data, src) = source_with(8 * BLOCK, &sigs);
    let cov = Coverage::new(data);
    let store = MemStore::new();
    // Unaligned on purpose; widened to 512-byte sectors.
    let ranges = [9_990..20_000, 290_000..300_001];
    let handle = Scan::new(src)
        .analyzer(cov.clone())
        .analyzer(SigCarver)
        .block_size(BLOCK)
        .ranges(ranges)
        .start(store.clone())
        .unwrap();
    let (summary, _) = finish(handle);
    let planned = [9_728..20_480, 289_792..300_032];
    assert_eq!(store.data().planned, planned);
    assert_eq!(
        summary.planned_bytes,
        (20_480 - 9_728) + (300_032 - 289_792)
    );
    let seen: u64 = cov.ranges().iter().map(|(r, _)| r.end - r.start).sum();
    assert_eq!(seen, summary.planned_bytes);
    // 200_000 lies outside the plan. The one at 299_997 starts inside and is completed
    // through the lookahead.
    assert_eq!(carved_offsets(&store.data().found), [10_000, 299_997]);
}

#[test]
fn analyzer_reads_arrive_and_named_files_are_stored() {
    let (_, src) = source_with(8 * BLOCK, &[]);
    let store = MemStore::new();
    let meta = MetaReader {
        range: (7 * BLOCK as u64)..(7 * BLOCK as u64 + 4096),
        priority: Priority::Immediate,
    };
    let handle = Scan::new(src)
        .analyzer(meta)
        .block_size(BLOCK)
        .start(store.clone())
        .unwrap();
    let (summary, events) = finish(handle);
    let found = store.data().found;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name.as_deref(), Some("from-metadata.mov"));
    assert_eq!(
        found[0].extents,
        [7 * BLOCK as u64..7 * BLOCK as u64 + 4096]
    );
    assert_eq!(summary.found.videos, 1);
    let phases: Vec<Phase> = events
        .iter()
        .filter_map(|e| match e {
            Event::Progress(p) => Some(p.phase),
            _ => None,
        })
        .collect();
    assert_eq!(phases.first(), Some(&Phase::Metadata));
    assert_eq!(phases.last(), Some(&Phase::Done));
}

#[test]
fn requests_past_the_end_are_truncated() {
    let (_, src) = source_with(4 * BLOCK, &[]);
    let store = MemStore::new();
    let len = 4 * BLOCK as u64;
    let meta = MetaReader {
        range: len - 100..len + 10_000,
        priority: Priority::Deferred,
    };
    let handle = Scan::new(src)
        .analyzer(meta)
        .block_size(BLOCK)
        .start(store.clone())
        .unwrap();
    finish(handle);
    assert_eq!(store.data().found[0].size, 100);
}

#[test]
fn pause_stops_reading_and_resume_continues() {
    let (_, src) = source_with(32 * BLOCK, &[]);
    let rec = Arc::new(Recorder::new(src, 1 << 20));
    let gate = Gate::closed();
    let handle = Scan::new(rec.clone())
        .analyzer(GateAt {
            at: 0,
            gate: gate.clone(),
        })
        .block_size(BLOCK)
        .workers(1)
        .start(MemStore::new())
        .unwrap();
    let events = handle.events();
    gate.until_waiting();
    handle.pause();
    // Wait for the reader to acknowledge.
    wait_for(
        &events,
        |e| matches!(e, Event::Progress(p) if p.state == RunState::Paused),
    );
    let reads_when_paused = rec.log().len();
    gate.open();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(rec.log().len(), reads_when_paused, "read while paused");
    handle.resume();
    let summary = handle.wait().unwrap();
    assert_eq!(summary.outcome, Outcome::Completed);
    assert_eq!(rec.bytes_read(), 32 * BLOCK as u64);
}

fn wait_for(events: &crossbeam_channel::Receiver<Event>, f: impl Fn(&Event) -> bool) {
    loop {
        let e = events
            .recv_timeout(Duration::from_secs(10))
            .expect("event not seen");
        if f(&e) {
            return;
        }
    }
}

#[test]
fn cancelled_scan_resumes_from_the_index() {
    let sigs = boundary_sigs();
    let len = 16 * BLOCK;
    let (_, src) = source_with(len, &sigs);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.hakk");

    // First session: stop half way.
    let gate = Gate::closed();
    let handle = Scan::new(src.clone())
        .analyzer(SigCarver)
        .analyzer(GateAt {
            at: 8 * BLOCK as u64,
            gate: gate.clone(),
        })
        .block_size(BLOCK)
        .workers(1)
        .label("test disk")
        .start(Index::open(&path).unwrap())
        .unwrap();
    gate.until_waiting();
    handle.cancel();
    gate.open();
    let summary = handle.wait().unwrap();
    assert_eq!(summary.outcome, Outcome::Cancelled);

    let ix = Index::open(&path).unwrap();
    let state = ix.scan_state().unwrap().unwrap();
    assert_eq!(state.source.label, "test disk");
    assert_eq!(state.progress.phase, ScanPhase::Scanning);
    let done: u64 = state.progress.done.iter().map(|r| r.end - r.start).sum();
    assert!(done > 0 && done < len as u64, "done = {done}");
    let first_session = ix.files(&Query::default()).unwrap().len();
    assert!(first_session < sigs.len());

    // Second session (the app was reopened).
    let known: Vec<(Option<u64>, Media)> = ix
        .files(&Query::default())
        .unwrap()
        .iter()
        .map(|f| (f.offset, f.kind.media()))
        .collect();
    let rec = Arc::new(Recorder::new(src, 1 << 20));
    let handle = Scan::new(rec.clone())
        .analyzer(SigCarver)
        .block_size(BLOCK)
        .resume_from(state)
        .known_files(known)
        .start(Index::open(&path).unwrap())
        .unwrap();
    let summary = handle.wait().unwrap();
    assert_eq!(summary.outcome, Outcome::Completed);
    assert_eq!(summary.found.photos, sigs.len() as u64);
    assert_eq!(rec.bytes_read(), len as u64 - done, "re-read what was done");

    let ix = Index::open(&path).unwrap();
    let mut offsets: Vec<u64> = ix
        .files(&Query::default())
        .unwrap()
        .iter()
        .filter_map(|f| f.offset)
        .collect();
    offsets.sort_unstable();
    assert_eq!(offsets, sigs, "every file exactly once");
    let state = ix.scan_state().unwrap().unwrap();
    assert_eq!(state.progress.phase, ScanPhase::Complete);
    assert_eq!(state.progress.done, [0..len as u64]);
}

#[test]
fn resume_rejects_a_different_source() {
    let (_, src) = source_with(4 * BLOCK, &[]);
    let state = index::ScanState {
        source: SourceInfo {
            len: 123,
            sector_size: 512,
            label: String::new(),
        },
        planned: vec![0..123],
        progress: ScanProgress::default(),
    };
    let err = Scan::new(src)
        .resume_from(state)
        .start(MemStore::new())
        .unwrap_err();
    assert!(matches!(err, ScanError::SourceMismatch { .. }), "{err}");
}

#[test]
fn a_block_is_not_done_until_reads_it_asked_for_are_analyzed() {
    let (_, src) = source_with(8 * BLOCK, &[]);
    let store = MemStore::new();
    let gate = Gate::closed();
    let at = 2 * BLOCK as u64 + 10;
    let handle = Scan::new(src)
        .analyzer(Follower {
            at,
            follow: 6 * BLOCK as u64..6 * BLOCK as u64 + 100,
            gate: gate.clone(),
        })
        .block_size(BLOCK)
        .checkpoint_interval(Duration::ZERO)
        .start(store.clone())
        .unwrap();
    // The deferred read is served after the main pass. While its reply is being analyzed,
    // checkpoints must not claim the block that asked for it.
    gate.until_waiting();
    let block = 2 * BLOCK as u64..3 * BLOCK as u64;
    let overlaps = |done: &[Range<u64>]| {
        done.iter()
            .any(|r| r.start < block.end && block.start < r.end)
    };
    let mut saved = store.data();
    for _ in 0..200 {
        if saved
            .progress
            .done
            .iter()
            .map(|r| r.end - r.start)
            .sum::<u64>()
            >= 7 * BLOCK as u64
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        saved = store.data();
    }
    assert!(
        saved.progress.done.iter().any(|r| r.contains(&0)),
        "{:?}",
        saved.progress.done
    );
    assert!(
        !overlaps(&saved.progress.done),
        "block {block:?} saved as done in {:?}",
        saved.progress.done
    );
    assert!(saved.found.is_empty());

    // Once the reply is analyzed, the block and its finding are saved together.
    gate.open();
    handle.wait().unwrap();
    let saved = store.data();
    assert_eq!(saved.progress.done, [0..8 * BLOCK as u64]);
    assert_eq!(saved.found.len(), 1);
}

#[test]
fn a_panicking_analyzer_does_not_stop_the_scan() {
    let sigs = [5 * BLOCK as u64 + 3];
    let (_, src) = source_with(8 * BLOCK, &sigs);
    let store = MemStore::new();
    let handle = Scan::new(src)
        .analyzer(PanicAt(BLOCK as u64))
        .analyzer(SigCarver)
        .block_size(BLOCK)
        .start(store.clone())
        .unwrap();
    let (summary, events) = finish(handle);
    assert_eq!(summary.outcome, Outcome::Completed);
    assert_eq!(carved_offsets(&store.data().found), sigs);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::AnalyzerPanicked { analyzer, offset: Some(o) } if analyzer == "panicky" && *o == BLOCK as u64
    )));
}

struct FailingStore;

impl Store for FailingStore {
    fn begin(&mut self, _: &SourceInfo, _: &[Range<u64>]) -> Result<(), StoreError> {
        Ok(())
    }

    fn commit(&mut self, _: &[scan::Finding], _: &ScanProgress) -> Result<(), StoreError> {
        Err("disk full".into())
    }
}

#[test]
fn a_failing_store_stops_the_scan() {
    let (_, src) = source_with(64 * BLOCK, &[]);
    let handle = Scan::new(src)
        .analyzer(SigCarver)
        .block_size(BLOCK)
        .checkpoint_interval(Duration::ZERO)
        .start(FailingStore)
        .unwrap();
    let err = handle.wait().unwrap_err();
    assert!(matches!(err, ScanError::Store(_)), "{err}");
}

#[test]
fn invalid_configuration_is_rejected() {
    let (_, src) = source_with(4 * BLOCK, &[]);
    for scan in [
        Scan::new(src.clone()).block_size(1000),
        Scan::new(src.clone()).block_size(1 << 30),
        Scan::new(src.clone()).workers(0),
        Scan::new(src.clone()).retry_chunk(100),
    ] {
        assert!(matches!(
            scan.start(MemStore::new()),
            Err(ScanError::InvalidConfig(_))
        ));
    }
}

#[test]
fn dropping_the_handle_cancels() {
    let (_, src) = source_with(64 * BLOCK, &[]);
    let gate = Gate::closed();
    let handle = Scan::new(src)
        .analyzer(GateAt {
            at: 0,
            gate: gate.clone(),
        })
        .block_size(BLOCK)
        .start(MemStore::new())
        .unwrap();
    gate.until_waiting();
    let opener = {
        let gate = gate.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            gate.open();
        })
    };
    drop(handle); // must not hang
    opener.join().unwrap();
}

#[test]
fn foreground_reads_during_the_scan() {
    let (data, src) = source_with(4 * BLOCK, &[]);
    let handle = Scan::new(src)
        .block_size(BLOCK)
        .start(MemStore::new())
        .unwrap();
    // A lease pauses the scan, so read through it and release it before waiting.
    let fg = handle.foreground();
    let mut buf = vec![0u8; 1000];
    use blocksource::BlockSource;
    fg.read_exact_at(5000, &mut buf).unwrap();
    assert_eq!(buf, data[5000..6000]);
    drop(fg);
    handle.wait().unwrap();
}

#[test]
fn files_carved_and_named_are_merged_in_the_index() {
    // A carving result at the same offset as a file system record.
    let at = 3 * BLOCK as u64;
    let (_, src) = source_with(8 * BLOCK, &[at]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.hakk");
    let handle = Scan::new(src)
        .analyzer(SigCarver)
        .analyzer(MetaReader {
            range: at..at + 8,
            priority: Priority::Immediate,
        })
        .block_size(BLOCK)
        .start(Index::open(&path).unwrap())
        .unwrap();
    handle.wait().unwrap();
    let mut ix = Index::open(&path).unwrap();
    // Both are photo/video? The carver reports JPEG, the metadata MOV: different media.
    assert_eq!(ix.merge_duplicates().unwrap(), []);
    let files = ix.files(&Query::default()).unwrap();
    assert_eq!(files.len(), 2);
    assert!(files.iter().any(|f| f.origin == Origin::FileSystem));
}
