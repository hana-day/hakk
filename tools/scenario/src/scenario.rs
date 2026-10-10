//! Scenario definition: the "history" of a disk, written in TOML.

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::size::Size;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub seed: u64,
    /// Size of the whole image.
    pub size: Size,
    pub fs: FsKind,
    pub steps: Vec<Step>,
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
        #[serde(default = "default_dir")]
        dir: String,
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

fn default_dir() -> String {
    "DCIM/100HAKK".to_owned()
}

impl Scenario {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let sc: Self =
            toml::from_str(&text).with_context(|| format!("{} is malformed", path.display()))?;
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
                    min_size, max_size, ..
                } => {
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
