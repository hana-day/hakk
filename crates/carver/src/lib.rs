//! Carving: finding and extracting files from raw data without the file system.
//!
//! For now this only has a minimal baseline implementation ([`naive`]), to be replaced by
//! the real one in phase 1.

pub mod naive;

/// A candidate file found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub offset: u64,
    pub len: u64,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jpeg,
}

impl Kind {
    pub fn extension(self) -> &'static str {
        match self {
            Kind::Jpeg => "jpg",
        }
    }
}
