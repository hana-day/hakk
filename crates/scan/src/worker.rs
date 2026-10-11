//! Analyzer workers: run every analyzer over each job.

use std::{
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crossbeam_channel::{Receiver, Sender};

use crate::{
    Finding,
    analyzer::{Analyzer, Context, ReadRequest},
    msg::{CoordMsg, Job, ToReader},
};

pub(crate) type Analyzers = Arc<[Box<dyn Analyzer>]>;

pub(crate) struct Worker {
    pub(crate) analyzers: Analyzers,
    pub(crate) jobs: Receiver<Job>,
    pub(crate) reader: Sender<ToReader>,
    pub(crate) coord: Sender<CoordMsg>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) source_len: u64,
    pub(crate) sector_size: u32,
}

impl Worker {
    pub(crate) fn run(self) {
        for job in self.jobs.iter() {
            if self.cancel.load(Ordering::Relaxed) {
                // Drain without working so the reader is never blocked on a full queue.
                continue;
            }
            self.handle(job);
            let _ = self.reader.send(ToReader::JobDone);
        }
    }

    fn handle(&self, job: Job) {
        let mut found = Vec::new();
        let mut spawned = 0;
        match job {
            Job::Block(block) => {
                let origin = block.range();
                for (i, a) in self.analyzers.iter().enumerate() {
                    let requests =
                        self.call(a.as_ref(), Some(block.offset()), &mut found, |a, cx| {
                            a.block(&block, cx)
                        });
                    spawned += self.forward(i, Some(origin.clone()), requests);
                }
                let _ = self.coord.send(CoordMsg::JobFinished {
                    origin: Some(origin),
                    block_holes: Some(block.holes),
                    spawned,
                    found,
                });
            }
            Job::Reply {
                analyzer,
                origin,
                reply,
            } => {
                if let Some(a) = self.analyzers.get(analyzer) {
                    let requests =
                        self.call(a.as_ref(), Some(reply.offset), &mut found, |a, cx| {
                            a.read_done(&reply, cx)
                        });
                    spawned += self.forward(analyzer, origin.clone(), requests);
                }
                let _ = self.coord.send(CoordMsg::JobFinished {
                    origin,
                    block_holes: None,
                    spawned,
                    found,
                });
            }
        }
    }

    /// Runs one analyzer callback, containing panics so one faulty analyzer cannot stall
    /// the scan. Returns the reads it requested.
    fn call(
        &self,
        analyzer: &dyn Analyzer,
        offset: Option<u64>,
        found: &mut Vec<Finding>,
        f: impl FnOnce(&dyn Analyzer, &mut Context<'_>),
    ) -> Vec<ReadRequest> {
        let mut requests = Vec::new();
        let mut cx = Context {
            found,
            requests: &mut requests,
            source_len: self.source_len,
            sector_size: self.sector_size,
        };
        if catch_unwind(AssertUnwindSafe(|| f(analyzer, &mut cx))).is_err() {
            let _ = self.coord.send(CoordMsg::AnalyzerPanicked {
                analyzer: analyzer.name().to_owned(),
                offset,
            });
        }
        requests
    }

    /// Sends requests to the reader (before this job's `JobDone`, so the reader never
    /// thinks the work is over while requests are still on their way).
    fn forward(
        &self,
        analyzer: usize,
        origin: Option<Range<u64>>,
        requests: Vec<ReadRequest>,
    ) -> usize {
        let n = requests.len();
        for req in requests {
            let _ = self.reader.send(ToReader::Request {
                analyzer,
                origin: origin.clone(),
                req,
            });
        }
        n
    }
}
