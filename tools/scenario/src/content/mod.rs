//! Deterministic test file content from a seed.
//!
//! Every file is made so its 512-byte blocks are (nearly) unique, so that placement can be
//! found by matching content. Compressed data and samples are therefore random bytes.

mod exif;
mod jpeg;
mod mp4;

use serde::Serialize;

use crate::{rng::Rng, scenario::ContentKind};

pub use exif::Exif;

/// A generated file.
pub struct Generated {
    pub bytes: Vec<u8>,
    pub meta: Meta,
}

/// Metadata kept in the manifest (used to evaluate e.g. file naming).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exif: Option<Exif>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Number of video samples.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples: Option<u32>,
}

/// Makes a file of roughly `target` bytes (approximate; varies by kind).
pub fn generate(kind: ContentKind, target: u64, rng: &mut Rng) -> anyhow::Result<Generated> {
    match kind {
        ContentKind::Jpeg => jpeg::generate(target, rng),
        ContentKind::Mp4 => Ok(mp4::generate(target, rng)),
    }
}

pub fn extension(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Jpeg => "JPG",
        ContentKind::Mp4 => "MP4",
    }
}
