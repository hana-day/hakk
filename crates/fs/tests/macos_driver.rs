//! Images written by the macOS file system drivers: format with `newfs_msdos` /
//! `newfs_exfat`, write files through the mounted volume, delete some, and check that the
//! scan lists every file with the right name, placement and content.
//!
//! Needs `hdiutil` (no admin rights): `cargo test -p fs --test macos_driver -- --ignored`.
#![cfg(target_os = "macos")]
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

use std::{
    fs::File,
    path::{Path, PathBuf},
    process::Command,
};

use blocksource::{BlockSource, ImageFile};
use fs::{Confidence, FsKind, Volume};

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const RAW: [&str; 2] = ["-imagekey", "diskimage-class=CRawDiskImage"];

/// Attaches `image` and returns the device and, if mounted, the mount point.
fn attach(image: &Path, mount: bool) -> (String, Option<PathBuf>) {
    let mut cmd = Command::new("hdiutil");
    cmd.arg("attach")
        .args(RAW)
        .args(["-nobrowse", "-noverify", "-noautofsck"]);
    if !mount {
        cmd.arg("-nomount");
    }
    let out = run(cmd.arg(image));
    let mut dev = None;
    let mut root = None;
    for line in out.lines() {
        let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
        if let Some(d) = cols.first().filter(|d| d.starts_with("/dev/disk")) {
            dev.get_or_insert_with(|| (*d).to_owned());
        }
        if let Some(m) = cols.get(2).filter(|m| m.starts_with('/')) {
            root = Some(PathBuf::from(m));
        }
    }
    (dev.unwrap(), root)
}

fn detach(dev: &str) {
    run(Command::new("hdiutil").args(["detach", dev]));
}

/// Deterministic content unique to `seed`.
fn content(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

struct Written {
    path: String,
    data: Vec<u8>,
    deleted: bool,
}

/// Formats, fills and partly deletes a volume. Returns the image path and what was written.
fn build(dir: &Path, name: &str, size: u64, newfs: &[&str]) -> (PathBuf, Vec<Written>) {
    let image = dir.join(format!("{name}.img"));
    File::create(&image).unwrap().set_len(size).unwrap();
    let (dev, _) = attach(&image, false);
    run(Command::new(newfs[0]).args(&newfs[1..]).arg(&dev));
    detach(&dev);

    let (dev, root) = attach(&image, true);
    let root = root.unwrap();
    let mut files = Vec::new();
    let mut write = |path: &str, seed: u32, len: usize| {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let data = content(seed, len);
        std::fs::write(&p, &data).unwrap();
        files.push(Written {
            path: path.to_owned(),
            data,
            deleted: false,
        });
    };
    for i in 0..12u32 {
        write(
            &format!("DCIM/100TEST/IMG_{:04}.JPG", i + 1),
            i,
            3000 + i as usize * 9_000,
        );
    }
    write("A long file name with spaces.jpeg", 100, 70_000);
    write("sub/dir/deep/movie clip.mp4", 101, 200_000);
    write("TRASH/ONE.JPG", 102, 5_000);
    write("TRASH/two with a long name.jpg", 103, 40_000);
    run(&mut Command::new("sync"));

    for f in files.iter_mut() {
        let n: u32 = f.path.get(17..21).and_then(|s| s.parse().ok()).unwrap_or(0);
        if n.is_multiple_of(2) && f.path.starts_with("DCIM") || f.path.starts_with("A long") {
            std::fs::remove_file(root.join(&f.path)).unwrap();
            f.deleted = true;
        }
        if f.path.starts_with("TRASH/") {
            f.deleted = true;
        }
    }
    std::fs::remove_dir_all(root.join("TRASH")).unwrap();
    run(&mut Command::new("sync"));
    detach(&dev);
    (image, files)
}

/// Compares paths ignoring ASCII case and the first character of each component (deleted
/// FAT short names lose it; it is guessed, sometimes wrongly).
fn same_path(got: &str, want: &str) -> bool {
    let (g, w): (Vec<&str>, Vec<&str>) = (got.split('/').collect(), want.split('/').collect());
    g.len() == w.len()
        && g.iter().zip(&w).all(|(g, w)| {
            g.eq_ignore_ascii_case(w)
                || (g.len() == w.len()
                    && g.get(1..)
                        .zip(w.get(1..))
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)))
        })
}

fn read(src: &ImageFile, extents: &[fs::Extent]) -> Vec<u8> {
    let mut out = Vec::new();
    for x in extents {
        let mut buf = vec![0u8; x.len as usize];
        src.read_exact_at(x.offset, &mut buf).unwrap();
        out.extend(buf);
    }
    out
}

fn check(image: &Path, files: &[Written], kind: FsKind) -> Volume {
    let src = ImageFile::open(image).unwrap();
    let mut vols = fs::scan_disk(&src).unwrap();
    let (_, vol) = vols.remove(0);
    let vol = vol.unwrap();
    assert_eq!(vol.kind, kind);
    assert!(vol.warnings.is_empty(), "{kind}: {:?}", vol.warnings);
    for f in files {
        let e = vol
            .entries
            .iter()
            .find(|e| !e.is_dir && e.deleted == f.deleted && same_path(&e.path, &f.path))
            .unwrap_or_else(|| {
                panic!(
                    "{kind}: {} (deleted: {}) not found in {:#?}",
                    f.path,
                    f.deleted,
                    vol.entries
                        .iter()
                        .map(|e| (&e.path, e.deleted))
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(e.size, f.data.len() as u64, "{kind}: {}", f.path);
        // Long names survive deletion; only 8.3 names lose their first character.
        let base = f.path.rsplit('/').next().unwrap();
        if base.contains(' ') {
            assert!(e.name_exact, "{kind}: {}", f.path);
        }
        // Nothing was written after deleting, so even deleted files must be intact.
        assert_ne!(e.confidence, Confidence::Low, "{kind}: {}", f.path);
        assert!(
            read(&src, &e.extents) == f.data,
            "{kind}: {} content",
            f.path
        );
    }
    // Free space never overlaps live files.
    for e in vol.entries.iter().filter(|e| !e.deleted) {
        for x in &e.extents {
            assert!(
                vol.free
                    .iter()
                    .all(|f| f.end() <= x.offset || x.end() <= f.offset)
            );
        }
    }
    vol
}

#[test]
#[ignore = "drives hdiutil; run with --ignored on macOS"]
fn macos_drivers() {
    let dir = std::env::temp_dir().join(format!("hakk-fs-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cases: [(&str, u64, &[&str], FsKind); 4] = [
        (
            "fat12",
            4 << 20,
            &["newfs_msdos", "-F", "12", "-v", "HAKK12"],
            FsKind::Fat12,
        ),
        (
            "fat16",
            32 << 20,
            &["newfs_msdos", "-F", "16", "-v", "HAKK16"],
            FsKind::Fat16,
        ),
        (
            "fat32",
            64 << 20,
            &["newfs_msdos", "-F", "32", "-c", "1", "-v", "HAKK32"],
            FsKind::Fat32,
        ),
        (
            "exfat",
            32 << 20,
            &["newfs_exfat", "-v", "HAKKEX"],
            FsKind::Exfat,
        ),
    ];
    for (name, size, newfs, kind) in cases {
        let (image, files) = build(&dir, name, size, newfs);
        let vol = check(&image, &files, kind);
        eprintln!(
            "{kind}: {} entries, {} deleted",
            vol.entries.len(),
            vol.entries.iter().filter(|e| e.deleted).count()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
