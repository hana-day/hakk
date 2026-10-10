//! Deterministic test file content from a seed.
//!
//! Every file is made so its 512-byte blocks are (nearly) unique, so that placement can be
//! found by matching content. Compressed data and samples are therefore random bytes.

mod exif;
mod jpeg;
mod mp4;
pub mod profile;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::{rng::Rng, scenario::ContentKind};
use profile::Profiles;

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

/// Makes a file of roughly `target` bytes (approximate; varies by kind), following
/// `profile` from `profiles` if given.
pub fn generate(
    kind: ContentKind,
    profile: Option<&str>,
    profiles: &Profiles,
    target: u64,
    rng: &mut Rng,
) -> Result<Generated> {
    match (kind, profile) {
        (ContentKind::Jpeg, None) => jpeg::generate(target, rng),
        (ContentKind::Mp4, None) => Ok(mp4::generate(target, rng)),
        (ContentKind::Jpeg, Some(name)) => match profiles.jpeg.get(name) {
            Some(p) => jpeg::generate_profiled(p, target, rng),
            None => bail!("no jpeg profile named {name:?}"),
        },
        (ContentKind::Mp4, Some(name)) => match profiles.video.get(name) {
            Some(p) => Ok(mp4::generate_profiled(p, target, rng)),
            None => bail!("no video profile named {name:?}"),
        },
    }
}

/// Where a file goes: `dir` if given, else the profile's directory, else a default; named
/// by the profile's template, else `IMG_nnnn.EXT`.
pub fn path(
    kind: ContentKind,
    profile: Option<&str>,
    profiles: &Profiles,
    dir: Option<&str>,
    n: u32,
) -> String {
    let (pdir, template) = match (kind, profile) {
        (ContentKind::Jpeg, Some(name)) => profiles
            .jpeg
            .get(name)
            .map(|p| (p.dir.as_str(), p.name.as_str()))
            .unzip(),
        (ContentKind::Mp4, Some(name)) => profiles
            .video
            .get(name)
            .map(|p| (p.dir.as_str(), p.name.as_str()))
            .unzip(),
        _ => (None, None),
    };
    let dir = dir.or(pdir).unwrap_or("DCIM/100HAKK");
    let name = match template {
        Some(t) => profile::file_name(t, n),
        None => format!("IMG_{n:04}.{}", extension(kind)),
    };
    format!("{dir}/{name}")
}

pub fn extension(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Jpeg => "JPG",
        ContentKind::Mp4 => "MP4",
    }
}
