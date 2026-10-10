//! Evaluator: scores recovered files against a ground-truth manifest.
//!
//! Usage: `hakk-eval <manifest.json> <recovered dir> [--tool NAME] [--elapsed SECS] [--json OUT]`
//!
//! Tool-agnostic: PhotoRec output and hakk output are scored the same way.

// A development tool handling small values; truncation lints are noise here.
#![allow(clippy::cast_possible_truncation)]

mod score;

use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};

use crate::score::{Manifest, Report};

struct Args {
    manifest: PathBuf,
    recovered: PathBuf,
    tool: String,
    elapsed: Option<f64>,
    json: Option<PathBuf>,
}

fn parse_args() -> Result<Args> {
    let mut pos = Vec::new();
    let mut tool = "unknown".to_owned();
    let mut elapsed = None;
    let mut json = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--tool" => tool = it.next().context("--tool needs a value")?,
            "--elapsed" => {
                elapsed = Some(it.next().context("--elapsed needs a value")?.parse()?);
            }
            "--json" => json = Some(it.next().context("--json needs a value")?.into()),
            _ if a.starts_with("--") => bail!("unknown option: {a}"),
            _ => pos.push(PathBuf::from(a)),
        }
    }
    let [manifest, recovered] = <[PathBuf; 2]>::try_from(pos).map_err(|_| {
        anyhow::anyhow!(
            "usage: hakk-eval <manifest.json> <recovered dir> [--tool NAME] [--elapsed SECS] [--json OUT]"
        )
    })?;
    Ok(Args {
        manifest,
        recovered,
        tool,
        elapsed,
        json,
    })
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let manifest = Manifest::load(&args.manifest)?;
    let report = Report::compute(
        &manifest,
        &args.manifest,
        &args.recovered,
        &args.tool,
        args.elapsed,
    )?;
    print!("{}", report.table());
    if let Some(path) = &args.json {
        fs::write(path, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(())
}
