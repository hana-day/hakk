//! Lists the files (live and deleted) on a disk image.
//!
//! Usage: `cargo run -p fs --example fsls -- <image> [--free]`

use blocksource::ImageFile;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        return Err("usage: fsls <image> [--free]".into());
    };
    let show_free = args.iter().any(|a| a == "--free");
    let src = ImageFile::open(path)?;
    for (part, vol) in fs::scan_disk(&src)? {
        println!(
            "partition at {:#x} ({} bytes, {:?})",
            part.offset, part.len, part.origin
        );
        let vol = match vol {
            Ok(v) => v,
            Err(e) => {
                println!("  {e}");
                continue;
            }
        };
        println!(
            "  {} label={:?} cluster={} entries={}",
            vol.kind,
            vol.label,
            vol.cluster_size,
            vol.entries.len()
        );
        for w in &vol.warnings {
            println!("  warning: {w}");
        }
        for e in &vol.entries {
            let first = e.extents.first().map_or(0, |x| x.offset);
            println!(
                "  {} {:>10} {:>3} ext @{:<#10x} {:?}/{:?}{} {}{}",
                if e.deleted { "DEL " } else { "    " },
                e.size,
                e.extents.len(),
                first,
                e.layout,
                e.confidence,
                if e.name_exact { "" } else { " ~name" },
                e.path,
                if e.is_dir { "/" } else { "" },
            );
        }
        let free: u64 = vol.free.iter().map(|f| f.len).sum();
        println!("  free: {free} bytes in {} runs", vol.free.len());
        if show_free {
            for f in &vol.free {
                println!("    {:#x} +{}", f.offset, f.len);
            }
        }
    }
    Ok(())
}
