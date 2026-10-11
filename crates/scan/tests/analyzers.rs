//! The fs and carver adapters on a hand-built FAT16 image holding real JPEGs.
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

// The FAT image builder from the fs crate's tests, reused as is.
#[path = "../../fs/tests/common/mod.rs"]
mod fsimg;

use std::sync::Arc;

use blocksource::MemSource;
use fsimg::{Dir, FatImage, FatKind};
use scan::{
    MemStore, Outcome, Scan,
    analyzers::{Carving, FileSystems, default_set},
    index::{Confidence, FileKind, Index, Origin, Query, Status},
};

const DIR: u8 = 0x10;
const ARCHIVE: u8 = 0x20;
const BLOCK: usize = 64 << 10;

/// A real baseline JPEG (noisy so it is several KiB).
fn jpeg(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    let pixels: Vec<u8> = (0..w * h * 3)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            ((i / 3 % w) as u8).wrapping_add((x % 64) as u8)
        })
        .collect();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode(&pixels, w, h, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn mp4_box(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(ty);
    v.extend_from_slice(payload);
    v
}

/// An interrupted recording: `ftyp` and `mdat`, no `moov`.
fn mp4_without_moov() -> Vec<u8> {
    let mut v = mp4_box(b"ftyp", b"isom\0\0\0\0isommp42");
    v.extend(mp4_box(b"mdat", &[0x5A; 8192]));
    v
}

struct Card {
    data: Vec<u8>,
    live: Vec<u8>,
    deleted: Vec<u8>,
    /// Offset of a JPEG no directory entry points to, crossing a block boundary.
    carved_only: u64,
    movie: u64,
}

fn card() -> Card {
    let mut img = FatImage::new(FatKind::Fat16);
    let clusters = |len: usize| len.div_ceil(512) as u32;
    let live = jpeg(128, 96, 1);
    let deleted = jpeg(96, 64, 2);
    let carved = jpeg(80, 80, 3);

    img.short(Dir::Root, 0, b"CARD       ", 0x08, 0, 0);
    img.short(Dir::Root, 1, b"DCIM       ", DIR, 30, 0);
    img.chain(&[30]);
    img.dots(30, 0);
    img.short(
        Dir::Cluster(30),
        2,
        b"IMG_0001JPG",
        ARCHIVE,
        100,
        live.len() as u32,
    );
    img.chain(&(100..100 + clusters(live.len())).collect::<Vec<_>>());
    // Deleted: the name lost its first character and the FAT chain is gone.
    img.short(
        Dir::Cluster(30),
        3,
        b"\xE5MG_0002JPG",
        ARCHIVE,
        200,
        deleted.len() as u32,
    );
    img.short(Dir::Cluster(30), 4, b"NOTES   TXT", ARCHIVE, 300, 10);
    img.chain(&[300]);

    let at = img.cluster_offset(100);
    img.data[at..at + live.len()].copy_from_slice(&live);
    let at = img.cluster_offset(200);
    img.data[at..at + deleted.len()].copy_from_slice(&deleted);
    // Starts 2 KiB before a block boundary and continues into the next block.
    let c = (400..)
        .find(|&c| (img.cluster_offset(c) + 2048).is_multiple_of(BLOCK))
        .unwrap();
    let carved_only = img.cluster_offset(c);
    assert!(carved.len() > 2048);
    img.data[carved_only..carved_only + carved.len()].copy_from_slice(&carved);
    let movie = img.cluster_offset(2000);
    let m = mp4_without_moov();
    img.data[movie..movie + m.len()].copy_from_slice(&m);

    Card {
        data: img.data,
        live,
        deleted,
        carved_only: carved_only as u64,
        movie: movie as u64,
    }
}

#[test]
fn named_and_carved_files_end_up_as_one_list() {
    let card = card();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.hakk");
    let handle = Scan::new(Arc::new(MemSource::new(card.data.clone())))
        .analyzers(default_set())
        .block_size(BLOCK)
        .workers(3)
        .start(Index::open(&path).unwrap())
        .unwrap();
    let summary = handle.wait().unwrap();
    assert_eq!(summary.outcome, Outcome::Completed);

    let mut ix = Index::open(&path).unwrap();
    let all = ix.files(&Query::default()).unwrap();
    // Named files come first: the file system pass runs before the main pass.
    assert_eq!(all[0].origin, Origin::FileSystem);
    assert_eq!(all[1].origin, Origin::FileSystem);

    let live = all
        .iter()
        .find(|f| f.name.as_deref() == Some("IMG_0001.JPG"))
        .unwrap();
    assert!(!live.deleted && live.name_exact);
    assert_eq!(live.confidence, Some(Confidence::High));
    assert_eq!(ix.folder_path(live.folder_id.unwrap()).unwrap(), ["DCIM"]);
    assert_eq!(live.volume.as_ref().unwrap().index, 0);
    assert_eq!(live.size, card.live.len() as u64);
    assert_eq!(
        live.preview.modified_at.as_deref(),
        Some("2009-01-02T03:04:06")
    );

    let deleted = all
        .iter()
        .find(|f| f.origin == Origin::FileSystem && f.deleted)
        .unwrap();
    assert!(!deleted.name_exact, "{:?}", deleted.name);
    assert!(deleted.name.as_deref().unwrap().ends_with("MG_0002.JPG"));
    assert_eq!(deleted.size, card.deleted.len() as u64);
    assert!(
        all.iter().all(|f| f.name.as_deref() != Some("NOTES.TXT")),
        "only photos and videos"
    );

    // The carver found both named JPEGs too; merging folds them into the named records.
    let merges = ix.merge_duplicates().unwrap();
    assert_eq!(merges.len(), 2, "{merges:?}");
    let visible = ix.files(&Query::default()).unwrap();
    assert_eq!(visible.len(), 4, "{visible:#?}");
    let live = ix.file(live.id).unwrap().unwrap();
    assert_eq!(live.status, Status::Recoverable, "verified by the carver");
    assert_eq!(
        (live.preview.width, live.preview.height),
        (Some(128), Some(96))
    );

    let carved = visible
        .iter()
        .find(|f| f.offset == Some(card.carved_only))
        .expect("the JPEG crossing a block boundary");
    assert_eq!(
        (carved.origin, carved.kind),
        (Origin::Carving, FileKind::Jpeg)
    );
    assert_eq!(carved.status, Status::Recoverable);
    assert_eq!(carved.preview.width, Some(80));

    let movie = visible
        .iter()
        .find(|f| f.offset == Some(card.movie))
        .unwrap();
    assert_eq!(movie.kind, FileKind::Mp4);
    assert_eq!(movie.status, Status::MissingIndex);
}

#[test]
fn file_systems_report_their_volumes() {
    let card = card();
    let fsys = Arc::new(FileSystems::new());
    let handle = Scan::new(Arc::new(MemSource::new(card.data)))
        .analyzer(fsys.clone())
        .block_size(BLOCK)
        .start(MemStore::new())
        .unwrap();
    handle.wait().unwrap();
    let volumes = fsys.volumes();
    assert_eq!(volumes.len(), 1);
    let v = &volumes[0];
    assert_eq!(v.kind, Some(fs::FsKind::Fat16));
    assert!(v.error.is_none());
    assert!(
        !v.free.is_empty(),
        "free space for a later free-space-only scan"
    );
}

#[test]
fn a_disk_without_a_file_system_is_still_carved() {
    let mut data = vec![0u8; 1 << 20];
    let photo = jpeg(64, 64, 9);
    data[4096..4096 + photo.len()].copy_from_slice(&photo);
    let fsys = Arc::new(FileSystems::new());
    let store = MemStore::new();
    let handle = Scan::new(Arc::new(MemSource::new(data)))
        .analyzer(fsys.clone())
        .analyzer(Carving::new())
        .block_size(BLOCK)
        .start(store.clone())
        .unwrap();
    handle.wait().unwrap();
    let found = store.data().found;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].offset(), Some(4096));
    assert_eq!(found[0].size, photo.len() as u64);
    assert!(fsys.volumes()[0].error.is_some());
}

#[test]
fn the_default_set_on_an_hdd_reads_the_card_once_in_order() {
    use blocksource::sim::{Faulty, Latency};
    let card = card();
    let len = card.data.len() as u64;
    let disk = Arc::new(Faulty::new(MemSource::new(card.data)).latency(Latency::HDD));
    let clock = disk.clock().clone();
    let handle = Scan::new(disk.clone())
        .analyzers(default_set())
        .block_size(BLOCK)
        .clock(clock.clone())
        .start(MemStore::new())
        .unwrap();
    let summary = handle.wait().unwrap();
    assert_eq!(summary.outcome, Outcome::Completed);
    // The metadata lies at the start of the card and every file runs forward from its
    // header, so everything is served by the one sequential pass: no seek at all.
    let ideal = Latency::HDD.per_read * (len / BLOCK as u64) as u32
        + std::time::Duration::from_secs_f64(len as f64 / Latency::HDD.bytes_per_sec as f64);
    let took = clock.elapsed();
    eprintln!("10 MiB card, fs + carving: {took:?} simulated (one clean pass: {ideal:?})");
    assert!(took < ideal + Latency::HDD.seek, "{took:?} vs {ideal:?}");
}
