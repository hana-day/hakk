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
    pub steps: Vec<Step>,
    /// Device profiles, loaded from `profiles.toml` next to the scenario or one level up.
    #[serde(skip)]
    pub profiles: Profiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FsKind {
    Exfat,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentKind {
    Jpeg,
    Mp4,
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

    fn validate(&self) -> Result<()> {
        if self.size.0 < 8 << 20 {
            bail!("image size must be at least 8MiB");
        }
        for (i, step) in self.steps.iter().enumerate() {
            match step {
                Step::Write {
                    min_size,
                    max_size,
                    kind,
                    profile,
                    ..
                } => {
                    if let Some(name) = profile {
                        let known = match kind {
                            ContentKind::Jpeg => self.profiles.jpeg.contains_key(name),
                            ContentKind::Mp4 => self.profiles.video.contains_key(name),
                        };
                        if !known {
                            bail!("steps[{i}]: unknown {kind:?} profile {name:?}");
                        }
                    }
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
            }
        }
        Ok(())
    }
}
