//! シナリオ（ディスクの「歴史」）の定義。TOML で書く。

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::size::Size;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub seed: u64,
    /// イメージ全体の大きさ。
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
    /// ファイルを書き込む。
    Write {
        count: u32,
        kind: ContentKind,
        min_size: Size,
        max_size: Size,
        #[serde(default = "default_dir")]
        dir: String,
    },
    /// 残っているファイルからシードに従って選んで削除する。
    Delete {
        /// 残っているファイルのうち削除する割合（0.0〜1.0）。
        fraction: f64,
        /// 指定すると、このフォルダ（の下）にあるファイルだけから選ぶ。
        #[serde(default)]
        dir: Option<String>,
    },
    /// クイックフォーマット（全ファイルが削除扱いになる）。
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
            fs::read_to_string(path).with_context(|| format!("{} を読めない", path.display()))?;
        let sc: Self =
            toml::from_str(&text).with_context(|| format!("{} の形式が不正", path.display()))?;
        sc.validate()?;
        Ok(sc)
    }

    fn validate(&self) -> Result<()> {
        if self.size.0 < 8 << 20 {
            bail!("イメージは 8MiB 以上にする");
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
                        bail!("steps[{i}]: min_size は 4KiB 以上にする（配置の特定に必要）");
                    }
                }
                Step::Delete { fraction, .. } => {
                    if !(0.0..=1.0).contains(fraction) {
                        bail!("steps[{i}]: fraction は 0.0〜1.0");
                    }
                }
                Step::Format => {}
            }
        }
        Ok(())
    }
}
