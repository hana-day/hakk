//! The blocking source handed to analyzers ([`crate::Context::source`]).

use std::{
    fmt, io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use blocksource::{BlockSource, ReadError, ReadErrorKind};
use crossbeam_channel::{Sender, bounded};

use crate::msg::SyncRead;

/// Sends each read to the reader thread and waits for the answer. Once the reader is
/// gone (the scan ended normally, e.g. during [`crate::Analyzer::finish`]) it reads the
/// source directly.
#[derive(Clone)]
pub(crate) struct ScanSource {
    pub(crate) tx: Sender<SyncRead>,
    pub(crate) source: Arc<dyn BlockSource>,
    pub(crate) cancel: Arc<AtomicBool>,
}

impl fmt::Debug for ScanSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScanSource").finish_non_exhaustive()
    }
}

impl ScanSource {
    fn cancelled(offset: u64, len: usize) -> ReadError {
        ReadError::new(
            offset..offset.saturating_add(len as u64),
            ReadErrorKind::Io(io::Error::new(io::ErrorKind::Interrupted, "scan cancelled")),
        )
    }
}

impl BlockSource for ScanSource {
    fn len(&self) -> u64 {
        self.source.len()
    }

    fn sector_size(&self) -> u32 {
        self.source.sector_size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, ReadError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Self::cancelled(offset, buf.len()));
        }
        let (reply, answer) = bounded(1);
        let msg = SyncRead {
            offset,
            len: buf.len(),
            reply,
        };
        if self.tx.send(msg).is_ok()
            && let Ok(result) = answer.recv()
        {
            let data = result?;
            let n = data.len().min(buf.len());
            if let (Some(dst), Some(src)) = (buf.get_mut(..n), data.get(..n)) {
                dst.copy_from_slice(src);
            }
            return Ok(n);
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Self::cancelled(offset, buf.len()));
        }
        self.source.read_at(offset, buf)
    }
}
