//! Scenario definition: the "history" of a disk, written in TOML.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::{content::profile::Profiles, size::Size};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub seed: u64,
    /// Size of the whole image.
    pub size: Size,
    pub fs: FsKind,
    /// Cluster size; the formatter's default if absent.
    #[serde(default)]
    pub cluster: Option<Size>,
    /// Start sector of a single MBR partition (e.g. 51, as a Canon camera formats a 32 MB
    /// SD card so the data area lands on a 16 KiB boundary). No partition table if absent.
    #[serde(default)]
    pub partition_start: Option<u64>,
    /// MBR partition type byte, if not the usual one for `fs` (Canon writes 0x04 for FAT12).
    #[serde(default)]
    pub partition_type: Option<u8>,
    pub steps: Vec<Step>,
    /// Device profiles, loaded from `profiles.toml` next to the scenario or one level up.
    #[serde(skip)]
    pub profiles: Profiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FsKind {
    Exfat,
    Fat12,
    Fat16,
    Fat32,
    /// No file system: files are placed directly on the disk with `place` steps, like the
    /// NIST CFReDS carving images. No OS driver is involved, so results match on every OS.
    None,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum Step {
    /// Write files.
    Write {
        count: u32,
        kind: ContentKind,
        min_size: Size,
        max_size: Size,
        /// Directory to write to; defaults to the profile's, or `DCIM/100HAKK`.
        #[serde(default)]
        dir: Option<String>,
        /// Device profile from `profiles.toml` (structure, directory and naming).
        #[serde(default)]
        profile: Option<String>,
    },
    /// Delete live files picked by the seed.
    Delete {
        /// Fraction of live files to delete (0.0 to 1.0).
        fraction: f64,
        /// If set, only pick files under this directory.
        #[serde(default)]
        dir: Option<String>,
    },
    /// Quick format (every file counts as deleted).
    Format,
    /// Place files directly on a disk without a file system (`fs = "none"`), with gaps of
    /// filler and the given fragmentation layout. Every placed file counts as deleted.
    Place {
        count: u32,
        kind: ContentKind,
        min_size: Size,
        max_size: Size,
        #[serde(default)]
        profile: Option<String>,
        layout: Placement,
        /// Number of fragments per file `[min, max]` (ignored by `contiguous`).
        #[serde(default = "default_fragments")]
        fragments: [u32; 2],
        /// Filler between files and fragments `[min, max]`.
        #[serde(default = "default_gap")]
        gap: [Size; 2],
        /// What the filler is made of.
        #[serde(default)]
        fill: Fill,
        /// Files and fragments start on multiples of this (a sector or a cluster).
        #[serde(default = "default_align")]
        align: Size,
    },
}

/// How placed files are laid out. Reference: NIST CFReDS "Forensic Images for File
/// Carving" (levels L0-L5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    /// Each file in one piece (L0).
    Contiguous,
    /// Fragments in order with gaps between them (L1).
    Sequential,
    /// Fragments out of order (L2).
    Shuffled,
    /// One fragment never written (L3).
    Missing,
    /// Another whole file sits between a file's two halves (L4).
    Nested,
    /// Fragments of two files alternate (L5).
    Braided,
}

/// Filler content: `"random"`, `"zero"`, or a byte such as `"0xB9"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fill {
    #[default]
    Random,
    Byte(u8),
}

impl<'de> Deserialize<'de> for Fill {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "random" => Ok(Fill::Random),
            "zero" => Ok(Fill::Byte(0)),
            hex => hex
                .strip_prefix("0x")
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .map(Fill::Byte)
                .ok_or_else(|| serde::de::Error::custom(format!("invalid fill: {s:?}"))),
        }
    }
}

fn default_fragments() -> [u32; 2] {
    [2, 4]
}

fn default_gap() -> [Size; 2] {
    [Size(4 << 10), Size(1 << 20)]
}

fn default_align() -> Size {
    Size(512)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    Jpeg,
    Mp4,
    Heic,
    Mts,
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let mut sc: Self =
            toml::from_str(&text).with_context(|| format!("{} is malformed", path.display()))?;
        let dir = path.parent().unwrap_or(Path::new("."));
        if let Some(p) = [dir.join("profiles.toml"), dir.join("../profiles.toml")]
            .into_iter()
            .find(|p| p.exists())
        {
            sc.profiles = Profiles::load(&p)?;
        }
        sc.validate()?;
        Ok(sc)
    }

    fn check_profile(&self, i: usize, kind: ContentKind, profile: Option<&str>) -> Result<()> {
        if let Some(name) = profile {
            let known = match kind {
                ContentKind::Jpeg => self.profiles.jpeg.contains_key(name),
                ContentKind::Mp4 => self.profiles.video.contains_key(name),
                ContentKind::Heic => self.profiles.heif.contains_key(name),
                ContentKind::Mts => false,
            };
            if !known {
                bail!("steps[{i}]: unknown {kind:?} profile {name:?}");
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.size.0 < 8 << 20 {
            bail!("image size must be at least 8MiB");
        }
        if self
            .cluster
            .is_some_and(|c| c.0 < 512 || !c.0.is_power_of_two())
        {
            bail!("cluster must be a power of two of at least 512 bytes");
        }
        for (i, step) in self.steps.iter().enumerate() {
            let place = matches!(step, Step::Place { .. });
            if place != (self.fs == FsKind::None) {
                bail!("steps[{i}]: `place` steps go with fs = \"none\", and only them");
            }
            match step {
                Step::Write {
                    min_size,
                    max_size,
                    kind,
                    profile,
                    ..
                } => {
                    self.check_profile(i, *kind, profile.as_deref())?;
                    if min_size > max_size {
                        bail!("steps[{i}]: min_size > max_size");
                    }
                    if min_size.0 < 4096 {
                        bail!(
                            "steps[{i}]: min_size must be at least 4KiB (needed to locate files)"
                        );
                    }
                }
                Step::Delete { fraction, .. } => {
                    if !(0.0..=1.0).contains(fraction) {
                        bail!("steps[{i}]: fraction must be within 0.0..=1.0");
                    }
                }
                Step::Format => {}
                Step::Place {
                    min_size,
                    max_size,
                    fragments,
                    gap,
                    align,
                    kind,
                    profile,
                    ..
                } => {
                    self.check_profile(i, *kind, profile.as_deref())?;
                    if min_size > max_size || gap[0] > gap[1] || fragments[0] > fragments[1] {
                        bail!("steps[{i}]: a [min, max] range is reversed");
                    }
                    if fragments[0] < 2 {
                        bail!("steps[{i}]: fragments must be at least 2");
                    }
                    if align.0 == 0 || !align.0.is_power_of_two() || align.0 % 512 != 0 {
                        bail!("steps[{i}]: align must be a power of two and a multiple of 512");
                    }
                }
            }
        }
        Ok(())
    }
}
