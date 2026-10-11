//! Where scan results and progress are persisted.

use std::{
    error::Error,
    ops::Range,
    sync::{Arc, Mutex},
};

use index::{Index, ScanProgress, SourceInfo};

use crate::Finding;

pub type StoreError = Box<dyn Error + Send + Sync>;

/// Persists what a scan finds together with how far it got.
///
/// Called only from the scan's coordinator thread. A failing store stops the scan, since
/// results that cannot be saved would be lost.
pub trait Store: Send {
    /// A new scan (not a resumed one) is starting.
    fn begin(&mut self, source: &SourceInfo, planned: &[Range<u64>]) -> Result<(), StoreError>;

    /// Saves files found since the last commit together with the current progress, as one
    /// unit: a range is never saved as done without the files found in it.
    fn commit(&mut self, found: &[Finding], progress: &ScanProgress) -> Result<(), StoreError>;
}

impl Store for Index {
    fn begin(&mut self, source: &SourceInfo, planned: &[Range<u64>]) -> Result<(), StoreError> {
        Ok(self.begin_scan(source, planned)?)
    }

    fn commit(&mut self, found: &[Finding], progress: &ScanProgress) -> Result<(), StoreError> {
        Index::commit(self, found, progress)?;
        Ok(())
    }
}

/// Keeps everything in memory. Clones share the same contents, so a test can keep one
/// clone and inspect it while the scan owns another.
#[derive(Debug, Clone, Default)]
pub struct MemStore {
    inner: Arc<Mutex<MemStoreData>>,
}

#[derive(Debug, Clone, Default)]
pub struct MemStoreData {
    pub source: Option<SourceInfo>,
    pub planned: Vec<Range<u64>>,
    pub found: Vec<Finding>,
    pub progress: ScanProgress,
    pub commits: usize,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of the current contents.
    pub fn data(&self) -> MemStoreData {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Store for MemStore {
    fn begin(&mut self, source: &SourceInfo, planned: &[Range<u64>]) -> Result<(), StoreError> {
        let mut d = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        d.source = Some(source.clone());
        d.planned = planned.to_vec();
        d.progress = ScanProgress::default();
        Ok(())
    }

    fn commit(&mut self, found: &[Finding], progress: &ScanProgress) -> Result<(), StoreError> {
        let mut d = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        d.found.extend_from_slice(found);
        d.progress = progress.clone();
        d.commits += 1;
        Ok(())
    }
}
