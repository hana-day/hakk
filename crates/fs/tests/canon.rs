//! Real-world check against the NPS "canon2" SD card images (Canon PowerShot SD800 IS,
//! FAT12), which come with ground truth (`report.xml`: path, size, byte runs and SHA-1 of
//! every file).
//!
//! The images are not redistributable in this repository. Point `HAKK_CANON2_DIR` at the
//! directory holding `nps-2009-canon2-gen{1..6}.raw` and `report.xml`, or place them in
//! `samples/nps-canon2/` at the workspace root, and run:
//! `cargo test -p fs --test canon -- --ignored`.

// Test code: relax the strict lints meant for library code.
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

use std::path::PathBuf;

use blocksource::{BlockSource, ImageFile};
use fs::{Confidence, Entry, Extent, Volume};

fn sample_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("HAKK_CANON2_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/nps-canon2")
        });
    dir.join("report.xml").is_file().then_some(dir)
}

struct Truth {
    path: String,
    size: u64,
    sha1: String,
    image: String,
    runs: Vec<Extent>,
}

fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = s.find(&open)? + open.len();
    let end = s.get(start..)?.find(&format!("</{name}>"))? + start;
    s.get(start..end)
}

fn attr(s: &str, name: &str) -> Option<u64> {
    let key = format!("{name}=\"");
    let start = s.find(&key)? + key.len();
    let end = s.get(start..)?.find('"')? + start;
    s.get(start..end)?.parse().ok()
}

/// A minimal reader for the fixed layout of `report.xml`.
fn load_truth(dir: &std::path::Path) -> Vec<Truth> {
    let xml = std::fs::read_to_string(dir.join("report.xml")).unwrap();
    xml.split("<fileobject>")
        .skip(1)
        .map(|o| Truth {
            path: tag(o, "filename").unwrap().to_owned(),
            size: tag(o, "filesize").unwrap().parse().unwrap(),
            sha1: tag(o, "sha1").unwrap().to_owned(),
            image: tag(o, "image").unwrap().to_owned(),
            runs: o
                .split("<run ")
                .skip(1)
                .map(|r| Extent {
                    offset: attr(r, "img_offset").unwrap(),
                    len: attr(r, "len").unwrap(),
                })
                .collect(),
        })
        .collect()
}

fn scan(path: &std::path::Path) -> (ImageFile, Volume) {
    let src = ImageFile::open(path).unwrap();
    let mut vols = fs::scan_disk(&src).unwrap();
    assert_eq!(vols.len(), 1);
    let (part, vol) = vols.remove(0);
    assert_eq!(part.offset, 51 * 512);
    let vol = vol.unwrap();
    assert_eq!(vol.kind, fs::FsKind::Fat12);
    assert_eq!(vol.cluster_size, 16384);
    assert!(vol.warnings.is_empty(), "{:?}", vol.warnings);
    (src, vol)
}

fn sha1(src: &ImageFile, extents: &[Extent]) -> String {
    let mut h = sha1_smol::Sha1::new();
    for e in extents {
        let mut buf = vec![0u8; usize::try_from(e.len).unwrap()];
        src.read_exact_at(e.offset, &mut buf).unwrap();
        h.update(&buf);
    }
    h.digest().to_string()
}

fn find<'a>(vol: &'a Volume, t: &Truth, deleted: bool) -> Option<&'a Entry> {
    vol.entries
        .iter()
        .find(|e| e.deleted == deleted && e.path.eq_ignore_ascii_case(&t.path) && e.size == t.size)
}

#[test]
#[ignore = "needs the NPS canon2 images (see the module docs)"]
fn gen6_matches_ground_truth() {
    let Some(dir) = sample_dir() else {
        eprintln!("canon2 samples not found; skipping");
        return;
    };
    let truth = load_truth(&dir);
    let (src, vol) = scan(&dir.join("nps-2009-canon2-gen6.raw"));

    // Every live file: exact path, size, placement and content.
    let resident: Vec<&Truth> = truth
        .iter()
        .filter(|t| t.image == "nps-2009-canon2-gen6.raw")
        .collect();
    assert_eq!(resident.len(), 34);
    for t in &resident {
        let e = find(&vol, t, false).unwrap_or_else(|| panic!("{} not found", t.path));
        assert_eq!(e.extents, t.runs, "{}", t.path);
        assert_eq!(e.confidence, Confidence::High);
        assert_eq!(sha1(&src, &e.extents), t.sha1, "{}", t.path);
    }

    // Files deleted before gen6 whose directory entries survive: the name (with its first
    // letter guessed) and the assumed contiguous placement must be right. (Their data was
    // overwritten by later files that were deleted in turn, which the file system cannot
    // tell; content checks are the carver's job.)
    let deleted: Vec<&str> = truth
        .iter()
        .filter(|t| t.image != "nps-2009-canon2-gen6.raw")
        .filter_map(|t| {
            let e = find(&vol, t, true)?;
            assert!(!e.name_exact);
            assert_eq!(e.extents, t.runs, "{}", t.path);
            Some(t.path.as_str())
        })
        .collect();
    assert_eq!(
        deleted,
        [
            "DCIM/100CANON/IMG_0025.JPG",
            "DCIM/100CANON/IMG_0030.JPG",
            "DCIM/100CANON/IMG_0035.JPG"
        ]
    );
}

#[test]
#[ignore = "needs the NPS canon2 images (see the module docs)"]
fn every_generation_scans_cleanly() {
    let Some(dir) = sample_dir() else {
        eprintln!("canon2 samples not found; skipping");
        return;
    };
    let truth = load_truth(&dir);
    for g in 1..=6 {
        let name = format!("nps-2009-canon2-gen{g}.raw");
        let path = dir.join(&name);
        if !path.is_file() {
            continue;
        }
        let (_, vol) = scan(&path);
        // Live files referenced against this generation are listed with exact runs.
        for t in truth.iter().filter(|t| t.image == name) {
            let e = find(&vol, t, false).unwrap_or_else(|| panic!("{name}: {}", t.path));
            assert_eq!(e.extents, t.runs, "{name}: {}", t.path);
        }
        // Free space and live files never overlap.
        for e in vol.entries.iter().filter(|e| !e.deleted) {
            for x in &e.extents {
                assert!(
                    vol.free
                        .iter()
                        .all(|f| f.end() <= x.offset || x.end() <= f.offset),
                    "{name}: {} overlaps free space",
                    e.path
                );
            }
        }
    }
}
