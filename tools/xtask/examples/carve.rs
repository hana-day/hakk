//! Carves an image with the structure-aware carver:
//! `cargo run -p xtask --release --example carve -- IMAGE [OUT] [--no-reassemble]`.

use std::{fs, time::Instant};

use blocksource::{BlockSource, ImageFile};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_reassemble = args.iter().any(|a| a == "--no-reassemble");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let Some(image) = pos.first() else {
        anyhow::bail!("usage: carve IMAGE [OUT] [--no-reassemble]");
    };
    let src = ImageFile::open(image)?;
    let mut opts = carver::scan::Options::default();
    if no_reassemble {
        opts.reassemble = None;
    }
    let started = Instant::now();
    let found = carver::scan::scan(&src, &opts);
    let secs = started.elapsed().as_secs_f64();
    println!(
        "{} files in {secs:.2}s ({:.0} MB/s)",
        found.len(),
        src.len() as f64 / 1e6 / secs
    );
    if let Some(out) = pos.get(1) {
        fs::create_dir_all(out)?;
        for f in &found {
            println!(
                "{:>10} {:>9} {:?} {:?} fragments={:?}",
                f.offset, f.len, f.kind, f.status, f.fragments
            );
            fs::write(
                format!("{out}/f{:010}.{}", f.offset, f.kind.extension()),
                f.read(&src),
            )?;
        }
    }
    Ok(())
}
