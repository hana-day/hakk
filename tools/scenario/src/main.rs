//! Scenario generator: builds a disk image with deleted files and a ground-truth manifest
//! from a seed.
//!
//! Usage: `hakk-scenario <scenario.toml> <output dir>`
//! Writes `image.img` (the disk image) and `manifest.json` (the ground truth) to the output dir.

// A development tool handling small values (image sizes etc.); truncation lints are noise here.
#![allow(clippy::cast_possible_truncation)]

mod backend;
mod content;
mod locate;
mod manifest;
mod raw;
mod rng;
mod runner;
mod scenario;
mod size;

use std::{fs, path::PathBuf, time::Instant};

use anyhow::{Result, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [scenario, out] = args.as_slice() else {
        bail!("usage: hakk-scenario <scenario.toml> <output dir>");
    };
    let sc = scenario::Scenario::load(scenario.as_ref())?;
    let out = PathBuf::from(out);

    let started = Instant::now();
    let manifest = runner::run(&sc, &out)?;
    fs::write(
        out.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    let files = &manifest.files;
    let deleted: Vec<_> = files.iter().filter(|f| f.deleted_at.is_some()).collect();
    let recoverable = deleted.iter().filter(|f| f.intact_bytes == f.size).count();
    let unlocated = files.iter().filter(|f| f.extents.is_none()).count();
    let fragmented = files.iter().filter(|f| f.fragmented).count();
    println!(
        "{}: {} files ({} deleted, {} of them fully intact), {} fragmented, {} unlocated, {:.1}s",
        manifest.scenario,
        files.len(),
        deleted.len(),
        recoverable,
        fragmented,
        unlocated,
        started.elapsed().as_secs_f64(),
    );
    println!("-> {}", out.display());
    Ok(())
}
