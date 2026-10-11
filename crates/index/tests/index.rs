#![allow(clippy::single_range_in_vec_init)]

use index::{
    BadRange, FileKind, Index, Media, NewFile, Origin, Preview, Query, SCHEMA_VERSION, ScanPhase,
    ScanProgress, SourceInfo, Status,
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
        folder: folder.iter().map(|s| s.to_string()).collect(),
        fs_ref: Some(format!("test:{name}")),
        deleted: true,
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
        (c.origin, c.kind, c.offset, c.size),
        (Origin::Carving, FileKind::Jpeg, 4096, 1000)
    );
    assert_eq!(c.preview.width, Some(640));
    assert_eq!(c.folder_id, None);

    let f = &all[1];
    assert_eq!(f.name.as_deref(), Some("clip.mov"));
    assert_eq!(f.offset, 1 << 20);
    assert_eq!(
        ix.extents(f.id).unwrap(),
        vec![1 << 20..(1 << 20) + 8192, 3 << 20..(3 << 20) + 4096]
    );
    let folder = f.folder_id.unwrap();
    assert_eq!(ix.folder_path(folder).unwrap(), ["DCIM", "100CANON"]);
    let roots = ix.subfolders(None).unwrap();
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
        new.iter().map(|f| f.offset).collect::<Vec<_>>(),
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

    assert_eq!(ix.merge_duplicates().unwrap(), 1);
    assert_eq!(ix.merge_duplicates().unwrap(), 0, "idempotent");

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
    assert_eq!(ix.subfolders(None).unwrap().len(), 1);

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
