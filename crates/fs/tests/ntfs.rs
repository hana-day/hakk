//! NTFS on hand-made images: live and deleted files, fragmented data runs (and a
//! fragmented MFT), attribute lists, sparse and resident data, parents that were deleted
//! or reused, and malformed input.
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

mod common;

use blocksource::{BlockSource, MemSource};
use common::{
    SECTOR,
    ntfs::{
        CLUSTER, DATA, DOS, NtfsImage, ROOT, TIME, WIN32, attr_list, file_name, non_resident,
        resident, std_info,
    },
    put32,
};
use fs::{Confidence, Entry, Extent, FsKind, Hole, Layout, Volume};

fn scan(data: Vec<u8>) -> Volume {
    let src = MemSource::new(data);
    let mut vols = fs::scan_disk(&src).unwrap();
    assert_eq!(vols.len(), 1);
    vols.remove(0).1.unwrap()
}

fn get<'a>(v: &'a Volume, path: &str) -> &'a Entry {
    v.entries
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| {
            let all: Vec<&str> = v.entries.iter().map(|e| e.path.as_str()).collect();
            panic!("{path} not found in {all:?}")
        })
}

fn ext(lcn: u64, len: u64) -> Extent {
    Extent {
        offset: NtfsImage::cluster(lcn),
        len,
    }
}

/// Bytes that identify cluster `lcn`.
fn fill(lcn: u64) -> Vec<u8> {
    (0..CLUSTER)
        .map(|i| (lcn as usize * 7 + i * 13) as u8)
        .collect()
}

/// The image every test starts from; see the comments for what each record exercises.
fn image() -> NtfsImage {
    let mut img = NtfsImage::new();
    // A live folder and a live file in it.
    img.record(
        30,
        3,
        true,
        true,
        None,
        &[std_info(TIME), file_name(ROOT, 5, "DCIM", WIN32)],
    );
    img.record(
        31,
        2,
        true,
        false,
        None,
        &[
            std_info(TIME),
            file_name(30, 3, "IMG_0001.JPG", WIN32),
            non_resident(DATA, 0, &[(3, Some(300))], 10_000, 0),
        ],
    );
    img.allocate(300, 3);
    // A deleted, fragmented video (out of order on disk). Freeing the record bumped its
    // sequence number from 4 to 5.
    img.record(
        32,
        5,
        false,
        false,
        None,
        &[
            std_info(TIME),
            file_name(30, 3, "VID.MP4", WIN32),
            file_name(30, 3, "VID~1.MP4", DOS),
            non_resident(
                DATA,
                0,
                &[(2, Some(400)), (3, Some(350)), (1, Some(500))],
                5 * 4096 + 100,
                0,
            ),
        ],
    );
    // A deleted folder with a deleted file in it.
    img.record(
        33,
        8,
        false,
        true,
        None,
        &[std_info(TIME), file_name(ROOT, 5, "old", WIN32)],
    );
    img.record(
        34,
        2,
        false,
        false,
        None,
        &[
            std_info(TIME),
            file_name(33, 7, "a.jpg", WIN32),
            non_resident(DATA, 0, &[(1, Some(510))], 3000, 0),
        ],
    );
    // A deleted file whose folder's record now holds another (live) file.
    img.record(
        35,
        2,
        false,
        false,
        None,
        &[
            file_name(36, 1, "lost.jpg", WIN32),
            non_resident(DATA, 0, &[(1, Some(520))], 4096, 0),
        ],
    );
    img.record(
        36,
        2,
        true,
        false,
        None,
        &[file_name(ROOT, 5, "other.txt", WIN32), resident(DATA, b"x")],
    );
    // Only the DOS alias survives.
    img.record(
        37,
        2,
        false,
        false,
        None,
        &[
            file_name(30, 3, "PICTUR~1.JPG", DOS),
            non_resident(DATA, 0, &[(1, Some(530))], 100, 0),
        ],
    );
    // Deleted, but its cluster now belongs to IMG_0001.JPG.
    img.record(
        38,
        2,
        false,
        false,
        None,
        &[
            file_name(30, 3, "over.jpg", WIN32),
            non_resident(DATA, 0, &[(1, Some(301))], 4096, 0),
        ],
    );
    // Resident data crossing the record's first 512-byte stride.
    let note: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
    img.record(
        39,
        2,
        true,
        false,
        None,
        &[
            std_info(TIME),
            file_name(ROOT, 5, "note.txt", WIN32),
            resident(DATA, &note),
        ],
    );
    // In the MFT's second fragment: a sparse file whose data clusters touch on disk.
    img.record(
        40,
        2,
        true,
        false,
        None,
        &[
            file_name(ROOT, 5, "sparse.bin", WIN32),
            non_resident(
                DATA,
                0,
                &[(1, Some(600)), (2, None), (1, Some(601))],
                4 * 4096,
                0,
            ),
        ],
    );
    img.allocate(600, 2);
    // A deleted file whose $DATA is split over an extension record (via $ATTRIBUTE_LIST).
    img.record(
        41,
        4,
        false,
        false,
        None,
        &[
            std_info(TIME),
            attr_list(&[(0x30, 0, 41, 3), (DATA, 0, 41, 3), (DATA, 3, 42, 1)]),
            file_name(30, 3, "big.mp4", WIN32),
            non_resident(DATA, 0, &[(3, Some(700))], 5 * 4096, 0),
        ],
    );
    img.record(
        42,
        2,
        false,
        false,
        Some(common::ntfs::reference(41, 3)),
        &[non_resident(DATA, 3, &[(2, Some(720))], 0, 0)],
    );
    // Compressed: the clusters do not hold the file as is.
    img.record(
        43,
        2,
        true,
        false,
        None,
        &[
            file_name(30, 3, "comp.jpg", WIN32),
            non_resident(DATA, 0, &[(1, Some(800))], 4096, 0x0001),
        ],
    );
    img.allocate(800, 1);
    // The extension record holding the rest of the data was reused by another file.
    img.record(
        44,
        3,
        false,
        false,
        None,
        &[
            attr_list(&[(DATA, 0, 44, 2), (DATA, 2, 45, 1)]),
            file_name(30, 3, "split.mp4", WIN32),
            non_resident(DATA, 0, &[(2, Some(900))], 4 * 4096, 0),
        ],
    );
    img.record(
        45,
        2,
        false,
        false,
        Some(common::ntfs::reference(60, 1)),
        &[non_resident(DATA, 2, &[(2, Some(950))], 0, 0)],
    );
    // Two deleted folders naming each other as parent, and a file inside.
    img.record(
        46,
        1,
        false,
        true,
        None,
        &[file_name(47, 1, "loopA", WIN32)],
    );
    img.record(
        47,
        1,
        false,
        true,
        None,
        &[file_name(46, 1, "loopB", WIN32)],
    );
    img.record(
        48,
        1,
        false,
        false,
        None,
        &[
            file_name(46, 1, "cyc.jpg", WIN32),
            non_resident(DATA, 0, &[(1, Some(1000))], 10, 0),
        ],
    );
    // A metadata file under $Extend: not listed.
    img.record(
        24,
        1,
        true,
        false,
        None,
        &[file_name(11, 11, "$Quota", WIN32)],
    );
    // An unused record that was never written, and garbage where a record should be.
    let junk = NtfsImage::record_offset(50);
    img.data[junk..junk + 4].copy_from_slice(b"FILE");
    img.data[junk + 4..junk + 1024].fill(0xFF);
    for lcn in [
        300, 301, 302, 350, 351, 352, 400, 401, 500, 510, 600, 601, 700, 701, 702, 720, 721,
    ] {
        img.write(lcn, &fill(lcn));
    }
    img
}

#[test]
fn lists_live_and_deleted_files() {
    let v = scan(image().finish());
    assert_eq!(v.kind, FsKind::Ntfs);
    assert_eq!(v.cluster_size, 4096);
    assert_eq!(v.label.as_deref(), Some("HAKK"));
    // Only the garbage record is reported.
    assert_eq!(v.warnings.len(), 1, "{:?}", v.warnings);
    assert!(v.warnings[0].contains("update sequence"));

    let d = get(&v, "DCIM");
    assert!(d.is_dir && !d.deleted);

    let e = get(&v, "DCIM/IMG_0001.JPG");
    assert!(!e.deleted && e.name_exact);
    assert_eq!(e.size, 10_000);
    assert_eq!(e.extents, [ext(300, 10_000)]);
    assert_eq!((e.layout, e.confidence), (Layout::Runs, Confidence::High));
    assert_eq!(e.record, Some(31));
    let t = e.created.unwrap();
    assert_eq!(
        (t.year, t.month, t.day, t.hour, t.second),
        (2024, 5, 6, 7, 9)
    );
    assert_eq!(t.utc_offset_minutes, Some(0));
}

#[test]
fn deleted_fragmented_file_keeps_its_runs() {
    let v = scan(image().finish());
    let e = get(&v, "DCIM/VID.MP4");
    assert!(e.deleted && e.name_exact);
    assert_eq!(e.size, 5 * 4096 + 100);
    assert_eq!(
        e.extents,
        [ext(400, 8192), ext(350, 3 * 4096), ext(500, 100)]
    );
    assert_eq!(e.confidence, Confidence::High);
    assert_eq!(e.first_cluster, 400);
}

#[test]
fn deleted_folders_and_orphans() {
    let v = scan(image().finish());
    let e = get(&v, "old/a.jpg");
    assert!(e.deleted);
    assert_eq!(e.confidence, Confidence::High);
    assert!(get(&v, "old").deleted);
    // The folder's record now holds other.txt (a different sequence number).
    let e = get(&v, "$OrphanFiles/lost.jpg");
    assert!(e.deleted);
    assert_eq!(e.extents, [ext(520, 4096)]);
    // Reference cycles end.
    let e = v.entries.iter().find(|e| e.name == "cyc.jpg").unwrap();
    assert!(e.path.starts_with("$OrphanFiles/"), "{}", e.path);
    // Metadata files are not listed.
    assert!(v.entries.iter().all(|e| !e.path.contains("$Quota")));
    assert!(v.entries.iter().all(|e| e.record.unwrap() >= 16));
}

#[test]
fn names_and_special_data() {
    let v = scan(image().finish());
    let e = get(&v, "DCIM/PICTUR~1.JPG");
    assert!(!e.name_exact);

    let e = get(&v, "DCIM/over.jpg");
    assert_eq!(e.confidence, Confidence::Low);

    let e = get(&v, "note.txt");
    let note: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(e.resident.as_deref(), Some(note.as_slice()));
    assert_eq!((e.size, e.layout), (600, Layout::Resident));
    assert!(e.extents.is_empty());

    let e = get(&v, "sparse.bin");
    assert_eq!(e.extents, [ext(600, 4096), ext(601, 4096)]);
    assert_eq!(
        e.holes,
        [Hole {
            file_offset: 4096,
            len: 8192
        }]
    );

    let e = get(&v, "DCIM/comp.jpg");
    assert_eq!(e.confidence, Confidence::Low);
    assert!(e.extents.is_empty());
}

#[test]
fn attribute_lists() {
    let v = scan(image().finish());
    let e = get(&v, "DCIM/big.mp4");
    assert!(e.deleted);
    assert_eq!(e.extents, [ext(700, 3 * 4096), ext(720, 2 * 4096)]);
    assert_eq!(e.confidence, Confidence::High);
    // The extension record now belongs to another file: the tail cannot be placed.
    let e = get(&v, "DCIM/split.mp4");
    assert_eq!(e.extents, [ext(900, 2 * 4096)]);
    assert_eq!(e.confidence, Confidence::Low);
}

#[test]
fn free_space_from_bitmap() {
    let v = scan(image().finish());
    let free: u64 = v.free.iter().map(|x| x.len).sum();
    // The volume has 2047 clusters (the backup boot sector's is not counted); allocated:
    // 0-3, 4-11, 20, 200-207, 300-302, 600-601 and 800.
    let used = 4 + 8 + 1 + 8 + 3 + 2 + 1;
    assert_eq!(free, (2047 - used) * 4096);
    assert!(v.free.contains(&ext(12, 8 * 4096)));
    assert!(v.free.iter().all(|x| x.offset != NtfsImage::cluster(300)));
}

#[test]
fn behind_an_mbr_partition() {
    let vol = image().finish();
    let start = 2048 * SECTOR;
    let mut disk = vec![0u8; start + vol.len()];
    disk[start..].copy_from_slice(&vol);
    disk[446 + 4] = 0x07;
    put32(&mut disk, 446 + 8, 2048);
    put32(&mut disk, 446 + 12, (vol.len() / SECTOR) as u32);
    disk[510] = 0x55;
    disk[511] = 0xAA;
    let src = MemSource::new(disk.clone());
    let vols = fs::scan_disk(&src).unwrap();
    let v = vols[0].1.as_ref().unwrap();
    let e = get(v, "DCIM/VID.MP4");
    assert_eq!(e.extents[0].offset, start as u64 + NtfsImage::cluster(400));
    // The recovered bytes are the ones written.
    let mut buf = vec![0u8; 4096];
    src.read_exact_at(e.extents[1].offset, &mut buf).unwrap();
    assert_eq!(buf, fill(350));
}

#[test]
fn backup_boot_sector_and_mft_mirror() {
    let mut data = image().finish();
    // Damage the boot sector's fields (keeping the OEM ID) and MFT record 0.
    data[11..0x50].fill(0);
    let rec0 = NtfsImage::record_offset(0);
    data[rec0..rec0 + 1024].fill(0);
    let v = scan(data);
    assert_eq!(get(&v, "DCIM/VID.MP4").extents.len(), 3);
    // Records in the MFT's second fragment are still found through the mirror's copy.
    assert_eq!(get(&v, "sparse.bin").record, Some(40));
}

#[test]
fn torn_record_is_low_confidence() {
    let mut data = image().finish();
    // Break the check bytes at the end of record 32's second stride.
    let at = NtfsImage::record_offset(32) + 1022;
    data[at] ^= 0xFF;
    let v = scan(data);
    assert_eq!(get(&v, "DCIM/VID.MP4").confidence, Confidence::Low);
    assert!(v.warnings.iter().any(|w| w.starts_with("2 MFT record(s)")));
}

#[test]
fn overlapping_deleted_files_keep_the_newer() {
    let mut img = image();
    // An older deleted photo whose cluster VID.MP4 (2024) later reused.
    let older = TIME - 365 * 86_400 * 10_000_000;
    img.record(
        49,
        2,
        false,
        false,
        None,
        &[
            std_info(older),
            file_name(30, 3, "older.jpg", WIN32),
            non_resident(DATA, 0, &[(1, Some(351))], 4096, 0),
        ],
    );
    let v = scan(img.finish());
    assert_eq!(get(&v, "DCIM/older.jpg").confidence, Confidence::Low);
    assert_eq!(get(&v, "DCIM/VID.MP4").confidence, Confidence::High);
}

mod fuzz {
    use proptest::prelude::*;

    use super::*;

    fn scan_any(data: Vec<u8>) {
        let src = MemSource::new(data);
        if let Ok(vols) = fs::scan_disk(&src) {
            for (_, v) in vols {
                if let Ok(v) = v {
                    for e in &v.entries {
                        for x in &e.extents {
                            assert!(x.offset.checked_add(x.len).is_some());
                        }
                    }
                }
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn mft_mutations(edits in prop::collection::vec((any::<usize>(), any::<u8>()), 1..64)) {
            // Concentrate on the MFT records in use.
            let mut data = image().finish();
            let start = NtfsImage::record_offset(0);
            let span = 64 * 1024;
            let second = NtfsImage::record_offset(32);
            for (i, &(at, v)) in edits.iter().enumerate() {
                let base = if i % 2 == 0 { start } else { second };
                data[base + at % span.min(32 * 1024)] = v;
            }
            scan_any(data);
        }

        #[test]
        fn boot_mutations(edits in prop::collection::vec((0usize..0x60, any::<u8>()), 1..8)) {
            let mut data = image().finish();
            for &(at, v) in &edits {
                data[at] = v;
            }
            scan_any(data);
        }

        #[test]
        fn random_records(rec in prop::collection::vec(any::<u8>(), 1024), n in 16u64..64) {
            let mut data = image().finish();
            let at = NtfsImage::record_offset(n);
            data[at..at + 1024].copy_from_slice(&rec);
            data[at..at + 4].copy_from_slice(b"FILE");
            scan_any(data);
        }
    }
}
