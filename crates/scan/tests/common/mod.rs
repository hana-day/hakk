//! Test helpers: dummy analyzers and a source wrapper that records reads.
#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::single_range_in_vec_init,
    clippy::unwrap_used
)]

use std::{
    ops::Range,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use blocksource::{BlockSource, ReadError};
use scan::{
    Analyzer, Block, Context, Event, Finding, Priority, ReadReply, ScanHandle,
    index::{FileKind, NewFile, Origin, Preview, Status},
};

pub const SIG: &[u8] = b"HAKKSIG!";

/// One read as seen by the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadLog {
    pub offset: u64,
    pub len: u64,
    pub ok: bool,
}

/// Records every read and counts seeks the way `blocksource::sim::Faulty` does (a read
/// starting further than `seek_threshold` from the end of the previous one).
pub struct Recorder<S> {
    inner: S,
    seek_threshold: u64,
    log: Mutex<Vec<ReadLog>>,
}

impl<S: BlockSource> Recorder<S> {
    pub fn new(inner: S, seek_threshold: u64) -> Self {
        Self {
            inner,
            seek_threshold,
            log: Mutex::new(Vec::new()),
        }
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn log(&self) -> Vec<ReadLog> {
        self.log.lock().unwrap().clone()
    }

    pub fn seeks(&self) -> usize {
        count_seeks(&self.log(), self.seek_threshold)
    }

    pub fn bytes_read(&self) -> u64 {
        self.log().iter().map(|r| r.len).sum()
    }
}

pub fn count_seeks(log: &[ReadLog], threshold: u64) -> usize {
    let mut head = 0u64;
    let mut seeks = 0;
    for r in log {
        if r.offset.abs_diff(head) > threshold {
            seeks += 1;
        }
        head = r.offset + r.len;
    }
    seeks
}

impl<S: BlockSource> BlockSource for Recorder<S> {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        let r = self.inner.read_at(offset, buf);
        let len = match &r {
            Ok(n) => *n as u64,
            Err(_) => buf.len() as u64,
        };
        self.log.lock().unwrap().push(ReadLog {
            offset,
            len,
            ok: r.is_ok(),
        });
        r
    }
}

/// Deterministic filler bytes that never contain [`SIG`].
pub fn filler(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // Lower-case letters only: SIG is upper case.
            b'a' + (x % 26) as u8
        })
        .collect()
}

/// Puts [`SIG`] at each offset.
pub fn plant(data: &mut [u8], offsets: &[u64]) {
    for &o in offsets {
        let o = o as usize;
        data[o..o + SIG.len()].copy_from_slice(SIG);
    }
}

/// Finds [`SIG`] at any byte offset, reporting hits that start in the block's own range
/// (the lookahead rule).
#[derive(Default)]
pub struct SigCarver;

impl Analyzer for SigCarver {
    fn name(&self) -> &str {
        "sig-carver"
    }

    fn lookahead(&self) -> usize {
        SIG.len() - 1
    }

    fn block(&self, block: &Block, cx: &mut Context<'_>) {
        let data = block.with_lookahead();
        for (i, w) in data.windows(SIG.len()).enumerate() {
            if i >= block.data().len() {
                break;
            }
            if w == SIG {
                cx.report(NewFile::carved(
                    FileKind::Jpeg,
                    block.offset() + i as u64,
                    SIG.len() as u64,
                ));
            }
        }
    }
}

/// Records the blocks it sees and checks their contents against the source.
pub struct Coverage {
    pub expected: Arc<Vec<u8>>,
    pub seen: Mutex<Vec<(Range<u64>, bool)>>,
    pub mismatches: Mutex<usize>,
}

impl Coverage {
    pub fn new(expected: Arc<Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
            expected,
            seen: Mutex::new(Vec::new()),
            mismatches: Mutex::new(0),
        })
    }

    /// Block ranges sorted by offset.
    pub fn ranges(&self) -> Vec<(Range<u64>, bool)> {
        let mut v = self.seen.lock().unwrap().clone();
        v.sort_by_key(|(r, _)| r.start);
        v
    }
}

impl Analyzer for Coverage {
    fn name(&self) -> &str {
        "coverage"
    }

    fn block(&self, block: &Block, _cx: &mut Context<'_>) {
        self.seen
            .lock()
            .unwrap()
            .push((block.range(), block.is_retry()));
        let start = block.offset() as usize;
        let mut ok = true;
        for (i, (&got, &want)) in block
            .data()
            .iter()
            .zip(&self.expected[start..start + block.data().len()])
            .enumerate()
        {
            let pos = block.offset() + i as u64;
            if got != want && !block.holes().iter().any(|h| h.contains(&pos)) {
                ok = false;
                break;
            }
        }
        if !ok {
            *self.mismatches.lock().unwrap() += 1;
        }
    }
}

/// Requests "metadata" at `range` before the scan and reports a named file from it.
pub struct MetaReader {
    pub range: Range<u64>,
    pub priority: Priority,
}

impl Analyzer for MetaReader {
    fn name(&self) -> &str {
        "meta"
    }

    fn start(&self, cx: &mut Context<'_>) {
        cx.read(self.range.clone(), self.priority, 7);
    }

    fn block(&self, _block: &Block, _cx: &mut Context<'_>) {}

    fn read_done(&self, reply: &ReadReply, cx: &mut Context<'_>) {
        assert_eq!(reply.tag, 7);
        cx.report(Finding {
            origin: Origin::FileSystem,
            kind: FileKind::Mov,
            status: Status::Unknown,
            extents: vec![reply.offset..reply.offset + reply.data.len() as u64],
            size: reply.data.len() as u64,
            name: Some("from-metadata.mov".into()),
            volume: None,
            folder: vec!["DCIM".into()],
            name_exact: true,
            fs_ref: Some("meta:1".into()),
            deleted: true,
            confidence: None,
            preview: Preview::default(),
        });
    }
}

/// A switch analyzers can wait on.
#[derive(Default)]
pub struct Gate {
    open: Mutex<bool>,
    cv: Condvar,
    /// Number of callers currently blocked.
    waiting: Mutex<usize>,
}

impl Gate {
    pub fn closed() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }

    pub fn wait(&self) {
        *self.waiting.lock().unwrap() += 1;
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.cv.wait(open).unwrap();
        }
        *self.waiting.lock().unwrap() -= 1;
    }

    /// Blocks until at least one caller waits at the gate.
    pub fn until_waiting(&self) {
        for _ in 0..10_000 {
            if *self.waiting.lock().unwrap() > 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("nobody reached the gate");
    }
}

/// Blocks at the gate on the first block at or after `at`.
pub struct GateAt {
    pub at: u64,
    pub gate: Arc<Gate>,
}

impl Analyzer for GateAt {
    fn name(&self) -> &str {
        "gate"
    }

    fn block(&self, block: &Block, _cx: &mut Context<'_>) {
        if block.range().contains(&self.at) {
            self.gate.wait();
        }
    }
}

/// Panics on the block containing `at`.
pub struct PanicAt(pub u64);

impl Analyzer for PanicAt {
    fn name(&self) -> &str {
        "panicky"
    }

    fn block(&self, block: &Block, _cx: &mut Context<'_>) {
        if block.range().contains(&self.0) {
            panic!("simulated analyzer bug");
        }
    }
}

/// On the block containing `at`, asks for a deferred read of `follow` (like a carver
/// following a structure past the lookahead) and reports a file when it arrives.
/// `read_done` waits at `gate`.
pub struct Follower {
    pub at: u64,
    pub follow: Range<u64>,
    pub gate: Arc<Gate>,
}

impl Analyzer for Follower {
    fn name(&self) -> &str {
        "follower"
    }

    fn block(&self, block: &Block, cx: &mut Context<'_>) {
        if block.range().contains(&self.at) {
            cx.read(self.follow.clone(), Priority::Deferred, 1);
        }
    }

    fn read_done(&self, reply: &ReadReply, cx: &mut Context<'_>) {
        self.gate.wait();
        cx.report(NewFile::carved(
            FileKind::Mp4,
            self.at,
            reply.data.len() as u64,
        ));
    }
}

/// Waits for the scan to finish and returns all events.
pub fn finish(handle: ScanHandle) -> (scan::Summary, Vec<Event>) {
    let events = handle.events();
    let summary = handle.wait().expect("scan failed");
    (summary, events.try_iter().collect())
}

/// Offsets of the carved files, sorted.
pub fn carved_offsets(found: &[Finding]) -> Vec<u64> {
    let mut v: Vec<u64> = found
        .iter()
        .filter(|f| f.origin == Origin::Carving && f.kind == FileKind::Jpeg)
        .filter_map(|f| f.offset())
        .collect();
    v.sort_unstable();
    v
}
