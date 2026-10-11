//! Adapters plugging the file system analysis (`fs`) and carving (`carver`) crates into
//! the scan as [`Analyzer`]s.
//!
//! Both crates are pull parsers over a [`blocksource::BlockSource`]; here they read
//! through [`crate::Context::source`], so their reads are ordered by the scan like any
//! other.

mod carving;
mod filesystems;

pub use carving::Carving;
pub use filesystems::{FileSystems, VolumeInfo};

use crate::Analyzer;

/// The analyzers a normal scan runs: file system analysis first (named files appear
/// first), then carving.
pub fn default_set() -> Vec<Box<dyn Analyzer>> {
    vec![Box::new(FileSystems::new()), Box::new(Carving::new())]
}
