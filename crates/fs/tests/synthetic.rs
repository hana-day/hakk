//! Hand-built images with known live and deleted entries.

// Test code: relax the strict lints meant for library code.
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

mod common;

use blocksource::MemSource;
use common::{Dir, ExfatImage, FatImage, FatKind, FileSpec, SECTOR};
use fs::{Confidence, Entry, Extent, FsKind, Layout, Volume};

fn scan(data: Vec<u8>) -> Volume {
    let src = MemSource::new(data);
    let mut v = fs::scan_disk(&src).unwrap();
    assert_eq!(v.len(), 1);
    v.remove(0).1.unwrap()
}

fn get<'a>(v: &'a Volume, path: &str) -> &'a Entry {
    v.entries
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| {
            panic!(
                "{path} not in {:#?}",
                v.entries.iter().map(|e| &e.path).collect::<Vec<_>>()
            )
        })
}

fn ext(offset: usize, len: u64) -> Extent {
    Extent {
        offset: offset as u64,
        len,
    }
}

const DIR: u8 = 0x10;
const ARCHIVE: u8 = 0x20;

fn fat_image(kind: FatKind) -> FatImage {
    let mut img = FatImage::new(kind);
    let root = Dir::Root;
    img.short(root, 0, b"CARD       ", 0x08, 0, 0);
    // A live, fragmented file.
    img.short(root, 1, b"PHOTO1  JPG", ARCHIVE, 10, 1500);
    img.chain(&[10, 11, 20]);
    // A live directory with a live file and a deleted long-named file.
    img.short(root, 2, b"SUB        ", DIR, 30, 0);
    img.chain(&[30]);
    img.dots(30, 0);
    img.short(Dir::Cluster(30), 2, b"INNER   TXT", ARCHIVE, 31, 100);
    img.chain(&[31]);
    let mut short = *b"LONGPH~1JPG";
    let i = img.long(Dir::Cluster(30), 3, "Long Photo Name.jpg", &short, true);
    short[0] = 0xE5;
    img.short(Dir::Cluster(30), i, &short, ARCHIVE, 40, 1100);
    // A deleted short-named file next to a live sibling with the same pattern.
    img.short(root, 3, b"IMG_0001JPG", ARCHIVE, 60, 10);
    img.chain(&[60]);
    img.short(root, 4, b"\xE5MG_0002JPG", ARCHIVE, 50, 600);
    // A deleted file whose second cluster is now used by another file.
    img.short(root, 5, b"\xE5MG_0003JPG", ARCHIVE, 61, 1000);
    img.short(root, 6, b"OTHER   BIN", ARCHIVE, 62, 10);
    img.chain(&[62]);
    // A deleted directory holding a deleted file.
    img.short(root, 7, b"\xE5LDDIR     ", DIR, 70, 0);
    img.dots(70, 0);
    img.short(Dir::Cluster(70), 2, b"\xE5ECRET  TXT", ARCHIVE, 71, 300);
    // A directory that chains to itself and lists itself and its parent's sibling.
    img.short(root, 8, b"LOOP       ", DIR, 80, 0);
    img.set_fat(80, 80);
    img.dots(80, 0);
    img.short(Dir::Cluster(80), 2, b"SELF       ", DIR, 80, 0);
    img.short(Dir::Cluster(80), 3, b"SUBAGAIN   ", DIR, 30, 0);
    img
}

#[test]
fn fat_live_and_deleted() {
    for kind in [FatKind::Fat12, FatKind::Fat16, FatKind::Fat32] {
        let img = fat_image(kind);
        let off = |c| img.cluster_offset(c);
        let v = scan(img.data.clone());
        assert_eq!(
            v.kind,
            match kind {
                FatKind::Fat12 => FsKind::Fat12,
                FatKind::Fat16 => FsKind::Fat16,
                FatKind::Fat32 => FsKind::Fat32,
            }
        );
        assert_eq!(v.label.as_deref(), Some("CARD"));
        assert_eq!(v.cluster_size, 512);
        assert!(v.warnings.is_empty(), "{kind:?}: {:?}", v.warnings);

        let e = get(&v, "PHOTO1.JPG");
        assert!(!e.deleted && e.name_exact);
        assert_eq!(e.extents, [ext(off(10), 1024), ext(off(20), 476)]);
        assert_eq!((e.layout, e.confidence), (Layout::Chain, Confidence::High));
        let t = e.modified.unwrap();
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute, t.second),
            (2009, 1, 2, 3, 4, 6)
        );
        assert_eq!(e.created.unwrap().second, 7);
        assert_eq!(e.created.unwrap().millis, 500);

        assert!(get(&v, "SUB").is_dir);
        assert_eq!(get(&v, "SUB/INNER.TXT").extents, [ext(off(31), 100)]);

        let e = get(&v, "SUB/Long Photo Name.jpg");
        assert!(e.deleted && e.name_exact);
        assert_eq!(e.extents, [ext(off(40), 1100)]);
        assert_eq!(
            (e.layout, e.confidence),
            (Layout::Assumed, Confidence::Medium)
        );

        let e = get(&v, "IMG_0002.JPG");
        assert!(e.deleted && !e.name_exact);
        assert_eq!(e.extents, [ext(off(50), 600)]);
        assert_eq!(e.confidence, Confidence::Medium);

        let e = get(&v, "IMG_0003.JPG");
        assert_eq!(e.confidence, Confidence::Low);

        assert!(get(&v, "_LDDIR").deleted);
        let e = get(&v, "_LDDIR/_ECRET.TXT");
        assert!(e.deleted && !e.name_exact);
        // One cluster: no contiguity to assume.
        assert_eq!(e.confidence, Confidence::High);

        // The loop is walked once.
        assert!(get(&v, "LOOP/SELF").is_dir);
        assert!(v.entries.iter().all(|e| !e.path.starts_with("LOOP/SELF/")));
        assert!(
            v.entries
                .iter()
                .all(|e| !e.path.starts_with("LOOP/SUBAGAIN/"))
        );

        // Free space: everything but the allocated clusters.
        let used = if kind == FatKind::Fat32 { 9 } else { 8 };
        let free: u64 = v.free.iter().map(|f| f.len).sum();
        assert_eq!(free, (u64::from(img.count) - used) * 512, "{kind:?}");
        assert_eq!(
            v.free[0],
            ext(
                off(if kind == FatKind::Fat32 { 3 } else { 2 }),
                (8 - u64::from(kind == FatKind::Fat32)) * 512
            )
        );
    }
}

#[test]
fn fat_behind_mbr() {
    let img = fat_image(FatKind::Fat16);
    let mut disk = vec![0u8; 63 * SECTOR];
    disk[446 + 4] = 0x06;
    disk[446 + 8..446 + 12].copy_from_slice(&63u32.to_le_bytes());
    disk[446 + 12..446 + 16].copy_from_slice(&((img.data.len() / SECTOR) as u32).to_le_bytes());
    disk[510] = 0x55;
    disk[511] = 0xAA;
    let base = disk.len();
    disk.extend_from_slice(&img.data);
    let v = scan(disk);
    assert_eq!(v.offset, base as u64);
    assert_eq!(
        get(&v, "PHOTO1.JPG").extents[0].offset,
        (base + img.cluster_offset(10)) as u64
    );
}

fn exfat_image() -> ExfatImage {
    let mut img = ExfatImage::new(2000);
    let root = img.root;
    let spec = |name, first, size, no_fat_chain, in_use| FileSpec {
        name,
        dir: false,
        first,
        size,
        no_fat_chain,
        in_use,
    };
    let mut i = 2;
    i = img.set(root, i, &spec("chain.jpg", 10, 1400, false, true));
    img.chain(&[10, 11, 15]);
    img.allocate(10, 2);
    img.allocate(15, 1);
    i = img.set(root, i, &spec("contig.mp4", 20, 2000, true, true));
    img.allocate(20, 4);
    i = img.set(root, i, &spec("gone_contig.jpg", 30, 1500, true, false));
    i = img.set(root, i, &spec("gone_frag.jpg", 40, 1300, false, false));
    img.chain(&[40, 41, 50]);
    i = img.set(root, i, &spec("gone_nofat.jpg", 60, 1000, false, false));
    i = img.set(root, i, &spec("reused.jpg", 10, 1000, true, false));
    i = img.set(
        root,
        i,
        &spec("a very long file name, deleted.jpg", 100, 10, true, false),
    );
    // An orphan File Name entry (its set was overwritten).
    let mut orphan = [0u8; 32];
    orphan[0] = 0x41;
    img.raw_entry(root, i, orphan);
    i += 1;
    let mut dir = spec("DCIM", 70, 512, true, true);
    dir.dir = true;
    img.set(root, i, &dir);
    img.allocate(70, 1);
    let mut old = spec("old", 80, 512, true, false);
    old.dir = true;
    img.set(70, 0, &old);
    let j = img.set(80, 0, &spec("x.jpg", 90, 100, true, false));
    // A set with a bad checksum inside a deleted directory is ignored.
    let k = img.set(80, j, &spec("bad.jpg", 91, 100, true, false));
    let at = img.cluster_offset(80) + j * 32 + 2;
    img.data[at] ^= 0xFF;
    let _ = k;
    img
}

#[test]
fn exfat_live_and_deleted() {
    let img = exfat_image();
    let off = |c| img.cluster_offset(c);
    let v = scan(img.data.clone());
    assert_eq!(v.kind, FsKind::Exfat);
    assert_eq!(v.label.as_deref(), Some("HAKK"));
    assert!(v.warnings.is_empty(), "{:?}", v.warnings);

    let e = get(&v, "chain.jpg");
    assert_eq!(e.extents, [ext(off(10), 1024), ext(off(15), 376)]);
    assert_eq!((e.layout, e.confidence), (Layout::Chain, Confidence::High));
    let t = e.modified.unwrap();
    assert_eq!(
        (t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis),
        (2024, 5, 6, 7, 8, 11, 230)
    );
    assert_eq!(t.utc_offset_minutes, Some(540));

    let e = get(&v, "contig.mp4");
    assert_eq!(e.extents, [ext(off(20), 2000)]);
    assert_eq!(e.layout, Layout::Contiguous);

    let e = get(&v, "gone_contig.jpg");
    assert!(e.deleted && e.name_exact);
    assert_eq!(e.extents, [ext(off(30), 1500)]);
    assert_eq!(
        (e.layout, e.confidence),
        (Layout::Contiguous, Confidence::High)
    );

    let e = get(&v, "gone_frag.jpg");
    assert_eq!(e.extents, [ext(off(40), 1024), ext(off(50), 276)]);
    assert_eq!(
        (e.layout, e.confidence),
        (Layout::StaleChain, Confidence::Medium)
    );

    let e = get(&v, "gone_nofat.jpg");
    assert_eq!(e.extents, [ext(off(60), 1000)]);
    assert_eq!(
        (e.layout, e.confidence),
        (Layout::Assumed, Confidence::Medium)
    );

    assert_eq!(get(&v, "reused.jpg").confidence, Confidence::Low);
    assert!(get(&v, "a very long file name, deleted.jpg").name_exact);

    let e = get(&v, "DCIM/old/x.jpg");
    assert!(e.deleted);
    assert_eq!(e.extents, [ext(off(90), 100)]);
    assert!(v.entries.iter().all(|e| !e.path.ends_with("bad.jpg")));

    // Free space follows the allocation bitmap.
    let free: u64 = v.free.iter().map(|f| f.len).sum();
    assert_eq!(free, (2000 - 13) * 512);
}

#[test]
fn exfat_backup_boot_sector() {
    let mut img = exfat_image();
    let (main, _) = img.data.split_at_mut(12 * SECTOR);
    let main = main.to_vec();
    img.data[12 * SECTOR..24 * SECTOR].copy_from_slice(&main);
    // Damage the main boot sector but keep its file system name.
    img.data[108] = 0;
    let v = scan(img.data);
    assert!(v.entries.iter().any(|e| e.path == "chain.jpg"));
}

#[test]
fn unrecognized_and_tiny_inputs() {
    for len in [0, 1, 511, 512, 4096] {
        let src = MemSource::new(vec![0u8; len]);
        let v = fs::scan_disk(&src).unwrap();
        assert!(v.iter().all(|(_, r)| r.is_err()));
    }
}

mod fuzz {
    use proptest::prelude::*;

    use super::*;

    fn scan_any(data: Vec<u8>) {
        let src = MemSource::new(data);
        if let Ok(vols) = fs::scan_disk(&src) {
            for (_, v) in vols {
                if let Ok(v) = v {
                    // Extents are well formed even for garbage.
                    for e in &v.entries {
                        for x in &e.extents {
                            assert!(x.offset.checked_add(x.len).is_some());
                        }
                    }
                }
            }
        }
    }

    fn mutate(mut data: Vec<u8>, edits: &[(usize, u8)], limit: usize) -> Vec<u8> {
        let n = data.len().min(limit);
        for &(at, v) in edits {
            data[at % n] = v;
        }
        data
    }

    proptest! {
        #[test]
        fn fat12_mutations(edits in prop::collection::vec((any::<usize>(), any::<u8>()), 1..64)) {
            // Boot sector, FATs, root directory and the first clusters.
            let img = fat_image(FatKind::Fat12);
            let limit = img.cluster_offset(100);
            scan_any(mutate(img.data, &edits, limit));
        }

        #[test]
        fn exfat_mutations(edits in prop::collection::vec((any::<usize>(), any::<u8>()), 1..64)) {
            let img = exfat_image();
            let limit = img.cluster_offset(100);
            scan_any(mutate(img.data, &edits, limit));
        }

        #[test]
        fn exfat_entry_mutations(edits in prop::collection::vec((any::<usize>(), any::<u8>()), 1..64)) {
            // Concentrate on the FAT and directories.
            let img = exfat_image();
            let start = 24 * SECTOR;
            let span = img.cluster_offset(100) - start;
            let mut data = img.data;
            for &(at, v) in &edits {
                data[start + at % span] = v;
            }
            scan_any(data);
        }

        #[test]
        fn random_boot_sectors(head in prop::collection::vec(any::<u8>(), 512), sig in any::<bool>(), exfat in any::<bool>()) {
            let mut data = vec![0u8; 64 * 1024];
            data[..512].copy_from_slice(&head);
            if exfat {
                data[3..11].copy_from_slice(b"EXFAT   ");
                data[11..64].fill(0);
            }
            if sig {
                data[510] = 0x55;
                data[511] = 0xAA;
            }
            scan_any(data);
        }
    }
}
