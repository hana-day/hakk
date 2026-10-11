#![allow(clippy::single_range_in_vec_init)]

use index::{
    BadRange, Confidence, FileKind, Index, Media, NewFile, Origin, Preview, Query, SCHEMA_VERSION,
    ScanPhase, ScanProgress, SourceInfo, Status, Volume,
};

fn named(
    name: &str,
    folder: &[&str],
    kind: FileKind,
    extents: Vec<std::ops::Range<u64>>,
) -> NewFile {
    let size = extents.iter().map(|e| e.end - e.start).sum();
    NewFile {
        origin: Origin::FileSystem,
        kind,
        status: Status::Unknown,
        extents,
        size,
        name: Some(name.into()),
        volume: Some(Volume {
            index: 0,
            label: Some("EOS_DIGITAL".into()),
        }),
        folder: folder.iter().map(|s| s.to_string()).collect(),
        name_exact: true,
        fs_ref: Some(format!("test:{name}")),
        deleted: true,
        confidence: Some(Confidence::High),
        preview: Preview::default(),
    }
}

fn source() -> SourceInfo {
    SourceInfo {
        len: 1 << 30,
        sector_size: 512,
        label: "test disk".into(),
    }
}

#[test]
fn stores_carved_and_named_files() {
    let mut ix = Index::open_in_memory().unwrap();
    let mut carved = NewFile::carved(FileKind::Jpeg, 4096, 1000);
    carved.preview.width = Some(640);
    let fragmented = named(
        "clip.mov",
        &["DCIM", "100CANON"],
        FileKind::Mov,
        vec![1 << 20..(1 << 20) + 8192, 3 << 20..(3 << 20) + 4096],
    );
    assert_eq!(ix.insert(&[carved, fragmented]).unwrap(), 2);

    let all = ix.files(&Query::default()).unwrap();
    assert_eq!(all.len(), 2);
    let c = &all[0];
    assert_eq!(
        (c.origin, c.kind, c.offset.unwrap(), c.size),
        (Origin::Carving, FileKind::Jpeg, 4096, 1000)
    );
    assert_eq!(c.preview.width, Some(640));
    assert_eq!(c.folder_id, None);

    let f = &all[1];
    assert_eq!(f.name.as_deref(), Some("clip.mov"));
    assert_eq!(f.offset, Some(1 << 20));
    assert_eq!(f.confidence, Some(Confidence::High));
    assert_eq!(
        f.volume.as_ref().unwrap().label.as_deref(),
        Some("EOS_DIGITAL")
    );
    assert_eq!(
        ix.extents(f.id).unwrap(),
        vec![1 << 20..(1 << 20) + 8192, 3 << 20..(3 << 20) + 4096]
    );
    let folder = f.folder_id.unwrap();
    assert_eq!(ix.folder_path(folder).unwrap(), ["DCIM", "100CANON"]);
    let roots = ix.root_folders(0).unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].1, "DCIM");

    let videos = ix
        .files(&Query {
            media: Some(Media::Video),
            ..Query::default()
        })
        .unwrap();
    assert_eq!(videos.len(), 1);
    let counts = ix.counts().unwrap();
    assert_eq!((counts.photos, counts.videos, counts.unknown), (1, 1, 2));
}

#[test]
fn re_reported_files_are_ignored() {
    let mut ix = Index::open_in_memory().unwrap();
    let files = [
        NewFile::carved(FileKind::Jpeg, 0, 10),
        named("a.jpg", &[], FileKind::Jpeg, vec![512..600]),
    ];
    assert_eq!(ix.insert(&files).unwrap(), 2);
    // A resumed scan reports the same files again.
    assert_eq!(ix.insert(&files).unwrap(), 0);
    // Same offset but a different kind is a different candidate.
    assert_eq!(
        ix.insert(&[NewFile::carved(FileKind::Mp4, 0, 10)]).unwrap(),
        1
    );
    assert_eq!(ix.files(&Query::default()).unwrap().len(), 3);
}

#[test]
fn rejects_files_without_extents() {
    let mut ix = Index::open_in_memory().unwrap();
    let mut f = NewFile::carved(FileKind::Jpeg, 0, 10);
    f.extents.clear();
    assert!(ix.insert(&[f]).is_err());
}

#[test]
fn incremental_listing_by_id() {
    let mut ix = Index::open_in_memory().unwrap();
    ix.insert(&[NewFile::carved(FileKind::Jpeg, 0, 10)])
        .unwrap();
    let seen = ix.last_id().unwrap();
    ix.insert(&[
        NewFile::carved(FileKind::Jpeg, 512, 10),
        NewFile::carved(FileKind::Png, 1024, 10),
    ])
    .unwrap();
    let new = ix
        .files(&Query {
            after_id: Some(seen),
            ..Query::default()
        })
        .unwrap();
    assert_eq!(
        new.iter().map(|f| f.offset.unwrap()).collect::<Vec<_>>(),
        [512, 1024]
    );
}

#[test]
fn merges_carved_duplicates_into_named_files() {
    let mut ix = Index::open_in_memory().unwrap();
    let mut carved = NewFile::carved(FileKind::Mp4, 1 << 20, 5000);
    carved.status = Status::Recoverable;
    carved.preview.duration_ms = Some(12_000);
    let unrelated = NewFile::carved(FileKind::Jpeg, 9 << 20, 100);
    // Carved as MP4 but named .mov: same media category, so the same file.
    let mut named_mov = named(
        "clip.mov",
        &["DCIM"],
        FileKind::Mov,
        vec![1 << 20..(1 << 20) + 5000],
    );
    named_mov.preview.width = Some(1920);
    // A photo at the same offset as nothing carved.
    let lone = named("x.jpg", &[], FileKind::Jpeg, vec![7 << 20..(7 << 20) + 10]);
    ix.insert(&[carved, unrelated, named_mov, lone]).unwrap();

    assert_eq!(ix.merge_duplicates().unwrap().len(), 1);
    assert_eq!(ix.merge_duplicates().unwrap(), [], "idempotent");

    let visible = ix.files(&Query::default()).unwrap();
    assert_eq!(visible.len(), 3);
    let mov = visible.iter().find(|f| f.kind == FileKind::Mov).unwrap();
    // Information only the carver knew moved to the named record.
    assert_eq!(mov.preview.duration_ms, Some(12_000));
    assert_eq!(mov.preview.width, Some(1920));
    assert_eq!(mov.status, Status::Recoverable);

    let all = ix
        .files(&Query {
            include_merged: true,
            ..Query::default()
        })
        .unwrap();
    let hidden = all.iter().find(|f| f.kind == FileKind::Mp4).unwrap();
    assert_eq!(hidden.merged_into, Some(mov.id));
    assert_eq!(ix.counts().unwrap().videos, 1);
}

#[test]
fn merge_rejects_bad_targets() {
    let mut ix = Index::open_in_memory().unwrap();
    ix.insert(&[
        NewFile::carved(FileKind::Jpeg, 0, 10),
        NewFile::carved(FileKind::Jpeg, 512, 10),
        NewFile::carved(FileKind::Jpeg, 1024, 10),
    ])
    .unwrap();
    assert!(ix.merge(1, 1).is_err());
    assert!(ix.merge(1, 99).is_err());
    ix.merge(1, 2).unwrap();
    assert!(ix.merge(3, 1).is_err(), "cannot merge into a merged record");
    // Moving 2 into 3 also moves what was merged into 2.
    ix.merge(2, 3).unwrap();
    assert_eq!(ix.file(1).unwrap().unwrap().merged_into, Some(3));
}

#[test]
fn status_updates() {
    let mut ix = Index::open_in_memory().unwrap();
    ix.insert(&[NewFile::carved(FileKind::Mp4, 0, 10)]).unwrap();
    ix.set_status(1, Status::MissingIndex).unwrap();
    assert_eq!(ix.file(1).unwrap().unwrap().status, Status::MissingIndex);
    assert_eq!(ix.counts().unwrap().missing_index, 1);
    assert!(ix.set_status(42, Status::Damaged).is_err());
}

#[test]
fn scan_state_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.hakk");
    let planned = vec![0..1 << 20, 4 << 20..8 << 20];
    let progress = ScanProgress {
        done: vec![0..1 << 20, 4 << 20..5 << 20],
        bad: vec![BadRange {
            range: 5 << 20..(5 << 20) + 4096,
            attempts: 1,
        }],
        phase: ScanPhase::Scanning,
        retry_passes: 0,
    };
    {
        let mut ix = Index::open(&path).unwrap();
        assert_eq!(ix.scan_state().unwrap(), None);
        ix.begin_scan(&source(), &planned).unwrap();
        let added = ix
            .commit(
                &[named("a.jpg", &["DCIM"], FileKind::Jpeg, vec![4096..8192])],
                &progress,
            )
            .unwrap();
        assert_eq!(added, 1);
    }
    let mut ix = Index::open(&path).unwrap();
    assert_eq!(ix.schema_version().unwrap(), SCHEMA_VERSION);
    let state = ix.scan_state().unwrap().unwrap();
    assert_eq!(state.source, source());
    assert_eq!(state.planned, planned);
    assert_eq!(state.progress, progress);
    assert_eq!(ix.files(&Query::default()).unwrap().len(), 1);

    // Folder ids are looked up again after reopening, not duplicated.
    ix.insert(&[named("b.jpg", &["DCIM"], FileKind::Jpeg, vec![9000..9100])])
        .unwrap();
    assert_eq!(ix.root_folders(0).unwrap().len(), 1);

    // A new scan clears coverage but keeps files.
    ix.begin_scan(&source(), &[0..1 << 30]).unwrap();
    let state = ix.scan_state().unwrap().unwrap();
    assert_eq!(state.progress, ScanProgress::default());
    assert_eq!(ix.files(&Query::default()).unwrap().len(), 2);
}

#[test]
fn readers_see_commits_while_writer_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.hakk");
    let mut writer = Index::open(&path).unwrap();
    let reader = Index::open(&path).unwrap();
    writer
        .insert(&[NewFile::carved(FileKind::Jpeg, 0, 10)])
        .unwrap();
    assert_eq!(reader.files(&Query::default()).unwrap().len(), 1);
}

#[test]
fn offsets_beyond_i64_are_rejected() {
    let mut ix = Index::open_in_memory().unwrap();
    let f = NewFile::carved(FileKind::Jpeg, u64::MAX - 10, 5);
    assert!(matches!(ix.insert(&[f]), Err(index::Error::OutOfRange(_))));
}

#[test]
fn file_system_entries_without_a_location_are_kept() {
    let mut ix = Index::open_in_memory().unwrap();
    let mut lost = named("IMG_0001.JPG", &["DCIM"], FileKind::Jpeg, Vec::new());
    lost.size = 3_000_000;
    lost.name_exact = false;
    lost.confidence = Some(Confidence::Low);
    ix.insert(&[lost]).unwrap();
    let f = &ix.files(&Query::default()).unwrap()[0];
    assert_eq!(f.offset, None);
    assert_eq!(f.size, 3_000_000);
    assert!(!f.name_exact);
    assert_eq!(ix.extents(f.id).unwrap(), []);
    // Nothing to merge with without a location.
    ix.insert(&[NewFile::carved(FileKind::Jpeg, 0, 10)])
        .unwrap();
    assert_eq!(ix.merge_duplicates().unwrap(), []);

    // Carving results always have one.
    let mut carved = NewFile::carved(FileKind::Jpeg, 0, 10);
    carved.extents.clear();
    assert!(ix.insert(&[carved]).is_err());
}

#[test]
fn folders_belong_to_their_volume() {
    let mut ix = Index::open_in_memory().unwrap();
    let mut a = named("a.jpg", &["DCIM"], FileKind::Jpeg, vec![0..10]);
    let mut b = named("b.jpg", &["DCIM"], FileKind::Jpeg, vec![512..600]);
    b.volume = Some(Volume {
        index: 1,
        label: None,
    });
    a.fs_ref = Some("p0:a".into());
    b.fs_ref = Some("p1:b".into());
    ix.insert(&[a, b]).unwrap();
    let files = ix.files(&Query::default()).unwrap();
    assert_ne!(files[0].folder_id, files[1].folder_id);
    assert_eq!(ix.root_folders(0).unwrap().len(), 1);
    assert_eq!(ix.root_folders(1).unwrap().len(), 1);
    let volumes = ix.volumes().unwrap();
    assert_eq!(volumes.len(), 2);
    assert_eq!(volumes[1].label, None);

    // A later report with the label fills it in.
    let mut c = named("c.jpg", &[], FileKind::Jpeg, vec![1024..1100]);
    c.volume = Some(Volume {
        index: 1,
        label: Some("BACKUP".into()),
    });
    ix.insert(&[c]).unwrap();
    assert_eq!(ix.volumes().unwrap()[1].label.as_deref(), Some("BACKUP"));
    assert_eq!(
        ix.root_folders(1).unwrap().len(),
        1,
        "same volume, not a new one"
    );
}

#[test]
fn dates_are_local_date_times() {
    let mut ix = Index::open_in_memory().unwrap();
    let mut f = named("a.jpg", &[], FileKind::Jpeg, vec![0..10]);
    f.preview.taken_at = Some("2025-08-14T15:30:22".into());
    f.preview.modified_at = Some("2025-08-14T15:30:22.5".into());
    ix.insert(&[f.clone()]).unwrap();
    let r = &ix.files(&Query::default()).unwrap()[0];
    assert_eq!(r.preview.taken_at.as_deref(), Some("2025-08-14T15:30:22"));
    assert_eq!(
        r.preview.modified_at.as_deref(),
        Some("2025-08-14T15:30:22.5")
    );

    for bad in [
        "2025-08-14 15:30:22",
        "2025-08-14T15:30:22Z",
        "1723649422",
        "2025-08-14",
    ] {
        f.fs_ref = Some(bad.into());
        f.preview.taken_at = Some(bad.into());
        assert!(ix.insert(&[f.clone()]).is_err(), "{bad}");
    }
}

#[test]
fn repaired_files_are_counted() {
    let mut ix = Index::open_in_memory().unwrap();
    ix.insert(&[NewFile::carved(FileKind::Mp4, 0, 10)]).unwrap();
    ix.set_status(1, Status::Repaired).unwrap();
    assert_eq!(ix.counts().unwrap().repaired, 1);
}

#[test]
fn carved_files_inside_a_complete_one_are_folded_into_it() {
    let mut ix = Index::open_in_memory().unwrap();
    let mut video = NewFile::carved(FileKind::Mp4, 1 << 20, 4 << 20);
    video.status = Status::Recoverable;
    let mut frame = NewFile::carved(FileKind::Jpeg, (1 << 20) + 8192, 50_000);
    frame.status = Status::Recoverable;
    frame.preview.width = Some(640);
    // Starts inside but runs past the end: not part of it.
    let straddling = NewFile::carved(FileKind::Jpeg, (5 << 20) - 512, 4096);
    // Inside a broken file: the container is not trusted.
    let broken = NewFile::carved(FileKind::Mp4, 9 << 20, 1 << 20);
    let inside_broken = NewFile::carved(FileKind::Jpeg, (9 << 20) + 512, 100);
    ix.insert(&[video, frame, straddling, broken, inside_broken])
        .unwrap();

    let merges = ix.merge_duplicates().unwrap();
    assert_eq!(merges, [(2, 1)]);
    let video = ix.file(1).unwrap().unwrap();
    assert_eq!(video.preview.width, None, "nothing copied from a part");
    assert_eq!(ix.files(&Query::default()).unwrap().len(), 4);
    assert_eq!(ix.merge_duplicates().unwrap(), []);
}

#[test]
fn two_writers_on_separate_connections_do_not_fail() {
    // The scan commits checkpoints while the GUI side merges duplicates.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("scan.hakk");
    let mut ix = Index::open(&path).unwrap();
    ix.begin_scan(&source(), &[0..1 << 30]).unwrap();
    let scanner = {
        let path = path.clone();
        std::thread::spawn(move || {
            let mut ix = Index::open(&path).unwrap();
            for i in 0..300u64 {
                let progress = ScanProgress {
                    done: vec![0..(i + 1) * 4096],
                    ..ScanProgress::default()
                };
                ix.commit(&[NewFile::carved(FileKind::Jpeg, i * 4096, 100)], &progress)
                    .unwrap();
            }
        })
    };
    for _ in 0..300 {
        ix.merge_duplicates().unwrap();
        ix.files(&Query::default()).unwrap();
    }
    scanner.join().unwrap();
    assert_eq!(ix.files(&Query::default()).unwrap().len(), 300);
}

#[test]
fn changes_are_tracked_by_revision() {
    let mut ix = Index::open_in_memory().unwrap();
    assert_eq!(ix.revision().unwrap(), 0);
    ix.insert(&[
        NewFile::carved(FileKind::Jpeg, 0, 10),
        named("a.jpg", &[], FileKind::Jpeg, vec![0..10]),
        NewFile::carved(FileKind::Jpeg, 512, 10),
    ])
    .unwrap();
    let seen = ix.revision().unwrap();
    assert_eq!(seen, 3);
    let changed = |ix: &Index, after: i64| -> Vec<i64> {
        ix.files(&Query {
            changed_after: Some(after),
            include_merged: true,
            ..Query::default()
        })
        .unwrap()
        .iter()
        .map(|f| f.id)
        .collect()
    };
    assert_eq!(changed(&ix, seen), [] as [i64; 0]);

    // Re-reporting, or saving progress alone, changes nothing.
    ix.insert(&[NewFile::carved(FileKind::Jpeg, 0, 10)])
        .unwrap();
    ix.commit(&[], &ScanProgress::default()).unwrap();
    assert_eq!(ix.revision().unwrap(), seen);

    // A status update, a merge (both records) and a new file are all visible at once.
    ix.set_status(3, Status::Damaged).unwrap();
    ix.merge_duplicates().unwrap();
    ix.insert(&[NewFile::carved(FileKind::Png, 4096, 10)])
        .unwrap();
    assert_eq!(changed(&ix, seen), [1, 2, 3, 4]);
    let after = ix
        .files(&Query {
            changed_after: Some(seen),
            include_merged: true,
            ..Query::default()
        })
        .unwrap();
    let carved = after.iter().find(|f| f.id == 1).unwrap();
    assert_eq!(carved.merged_into, Some(2));
    let latest = after.iter().map(|f| f.rev).max().unwrap();
    assert_eq!(latest, ix.revision().unwrap());
    assert_eq!(changed(&ix, latest), [] as [i64; 0]);
}

/// A deleted, fragmented FAT file: the file system can only guess "contiguous", the carver
/// reassembled the real fragments.
#[test]
fn verified_carved_extents_replace_a_guessed_placement() {
    let guessed = |name: &str, conf, size: u64| {
        let mut f = named(
            name,
            &["DCIM"],
            FileKind::Jpeg,
            vec![1 << 20..(1 << 20) + size],
        );
        f.confidence = Some(conf);
        f.deleted = true;
        f
    };
    let carved = |size: u64, extents: Vec<std::ops::Range<u64>>| {
        let mut c = NewFile::carved(FileKind::Jpeg, 1 << 20, size);
        c.extents = extents;
        c.status = Status::Recoverable;
        c
    };
    let real = vec![1 << 20..(1 << 20) + 6000, 9 << 20..(9 << 20) + 4000];
    let run = |named: NewFile, carved: NewFile| {
        let mut ix = Index::open_in_memory().unwrap();
        ix.insert(&[named, carved]).unwrap();
        assert_eq!(ix.merge_duplicates().unwrap(), [(2, 1)]);
        let f = ix.file(1).unwrap().unwrap();
        (f.status, ix.extents(1).unwrap(), f.name, f.offset)
    };

    // Guessed placement, complete carve of the recorded size: take the carver's extents,
    // keep the name.
    let (status, extents, name, offset) = run(
        guessed("IMG_0026.JPG", Confidence::Low, 10_000),
        carved(10_000, real.clone()),
    );
    assert_eq!(extents, real);
    assert_eq!(status, Status::Recoverable);
    assert_eq!(name.as_deref(), Some("IMG_0026.JPG"));
    assert_eq!(offset, Some(1 << 20));

    // The carve does not add up to the recorded size: a wrong reassembly. Keep the file
    // system's extents, and do not claim they were verified.
    let (status, extents, ..) = run(
        guessed("IMG_0021.JPG", Confidence::Medium, 10_000),
        carved(
            9_000,
            vec![1 << 20..(1 << 20) + 5000, 9 << 20..(9 << 20) + 4000],
        ),
    );
    assert_eq!(extents, [1 << 20..(1 << 20) + 10_000]);
    assert_eq!(status, Status::Unknown);

    // A known placement (live chain) wins over the carver.
    let (status, extents, ..) = run(
        guessed("LIVE.JPG", Confidence::High, 10_000),
        carved(10_000, real.clone()),
    );
    assert_eq!(extents, [1 << 20..(1 << 20) + 10_000]);
    assert_eq!(status, Status::Unknown);

    // Same bytes: the carver's verdict applies.
    let (status, ..) = run(
        guessed("SAME.JPG", Confidence::Low, 10_000),
        carved(10_000, vec![1 << 20..(1 << 20) + 10_000]),
    );
    assert_eq!(status, Status::Recoverable);
}
