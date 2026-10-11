//! Carves an image with the structure-aware carver: `cargo run -p xtask --example carve -- IMAGE OUT`.

use std::fs;

use blocksource::ImageFile;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [image, out] = args.as_slice() else {
        anyhow::bail!("usage: carve IMAGE OUT");
    };
    fs::create_dir_all(out)?;
    let src = ImageFile::open(image)?;
    for f in carver::scan::scan(&src, &carver::scan::Options::default()) {
        println!(
            "{:>10} {:>9} {:?} {:?} fragments={:?}",
            f.offset, f.len, f.kind, f.status, f.fragments
        );
        fs::write(
            format!("{out}/f{:010}.{}", f.offset, f.kind.extension()),
            f.read(&src),
        )?;
    }
    Ok(())
}
