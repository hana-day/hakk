//! Device profiles: the structural traits of files written by real cameras and phones.
//!
//! Profiles hold only structure and numbers observed in real files (segment order, EXIF
//! layout, thumbnail sizes, box layout, chunking), never their content. Generated files
//! follow a profile's structure while their pixels and samples stay random.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    #[serde(default)]
    pub jpeg: BTreeMap<String, JpegProfile>,
    #[serde(default)]
    pub video: BTreeMap<String, VideoProfile>,
}

/// An inclusive `[min, max]` range.
pub type Range = [u64; 2];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JpegProfile {
    pub make: String,
    pub model: String,
    /// Directory files are written to.
    pub dir: String,
    /// File name template; `{n}` is a 4-digit counter.
    pub name: String,
    /// Image aspect ratio `[width, height]`.
    #[serde(default = "default_aspect")]
    pub aspect: [u32; 2],
    /// Segments between SOI and SOS, in order. See [`Segment`].
    pub segments: Vec<Segment>,
    /// Target size of the EXIF APP1 segment (padded with a MakerNote).
    pub exif_len: Range,
    /// Size of the JPEG thumbnail embedded in EXIF, if any.
    pub thumbnail: Option<Range>,
    /// Bytes appended after the final EOI, if any.
    pub trailer: Option<Range>,
}

/// A header segment of a profiled JPEG.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Segment {
    /// The EXIF APP1 segment.
    Exif,
    /// A JFIF APP0 segment.
    Jfif,
    /// Another APPn segment with random content: `{ app = { n = 4, len = [a, b] } }`.
    App { n: u8, len: Range },
    /// All quantization tables in one DQT segment.
    Dqt,
    /// One DQT segment per table.
    DqtEach,
    /// All Huffman tables in one DHT segment.
    Dht,
    /// One DHT segment per table.
    DhtEach,
    /// The frame header.
    Sof,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoProfile {
    pub dir: String,
    pub name: String,
    /// `ftyp` major brand, e.g. `"qt  "`, `"isom"`, `"mp42"`, `"3gp4"`.
    pub brand: String,
    #[serde(default)]
    pub compatible: Vec<String>,
    /// Where `moov` goes.
    pub layout: Layout,
    /// Whether a `wide` box precedes `mdat` (QuickTime).
    #[serde(default)]
    pub wide: bool,
    /// With a leading `moov`: the space reserved up front for `moov` plus the `free` box
    /// filling the rest of it (devices reserve a fixed area before recording starts).
    #[serde(default)]
    pub reserve: Option<Range>,
    /// Video samples per chunk.
    pub video_per_chunk: u32,
    /// Audio samples per chunk; no audio track if absent.
    pub audio_per_chunk: Option<u32>,
    /// Use 64-bit chunk offsets (`co64`) instead of `stco`.
    #[serde(default)]
    pub co64: bool,
    /// Number of timed metadata tracks (`mebx`, written by iOS).
    #[serde(default)]
    pub metadata_tracks: u32,
    /// Sizes of `uuid` boxes appended after `mdat`.
    #[serde(default)]
    pub trailing_uuid: Vec<Range>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// `ftyp mdat moov`: the index is written when recording stops.
    MoovLast,
    /// `ftyp moov free mdat`: space for the index is reserved up front.
    MoovFirst,
}

fn default_aspect() -> [u32; 2] {
    [4, 3]
}

impl Profiles {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let p: Self =
            toml::from_str(&text).with_context(|| format!("{} is malformed", path.display()))?;
        p.validate()?;
        Ok(p)
    }

    fn validate(&self) -> Result<()> {
        for (name, j) in &self.jpeg {
            let count = |s: &[Segment]| j.segments.iter().filter(|x| s.contains(x)).count();
            if count(&[Segment::Dqt, Segment::DqtEach]) != 1
                || count(&[Segment::Dht, Segment::DhtEach]) != 1
                || count(&[Segment::Sof]) != 1
            {
                bail!("jpeg.{name}: segments need exactly one dqt, one dht and one sof");
            }
            if j.exif_len[1] > 65_000 {
                bail!("jpeg.{name}: exif_len must fit in one APP1 segment");
            }
        }
        for (name, v) in &self.video {
            if v.brand.len() != 4 || v.compatible.iter().any(|b| b.len() != 4) {
                bail!("video.{name}: brands are four characters");
            }
            if v.video_per_chunk == 0 || v.audio_per_chunk == Some(0) {
                bail!("video.{name}: samples per chunk must be positive");
            }
        }
        Ok(())
    }
}

/// Expands a file name template.
pub fn file_name(template: &str, n: u32) -> String {
    template.replace("{n}", &format!("{n:04}"))
}
