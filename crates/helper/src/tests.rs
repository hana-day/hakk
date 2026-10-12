//! End-to-end tests: a disk image, the real scan pipeline and index, a stand-in analyzer.

// Offsets and lengths in these tests are small constants.
#![allow(clippy::cast_possible_truncation)]

use std::{
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use hakk_protocol::{Destination, DestinationId, FileOrigin, Helper, HelperEvent, RecoveryRequest};
use scan::{Analyzer, Block, Context};

use super::*;

const IMAGE_LEN: usize = 2 << 20;
const JPEG_AT: [usize; 2] = [64 << 10, 1 << 20];
const JPEG_LEN: u64 = 4096;

/// Reports a "JPEG" wherever a sector starts with the JPEG start-of-image marker.
struct SoiFinder;

impl Analyzer for SoiFinder {
    fn name(&self) -> &str {
        "soi-finder"
    }

    fn block(&self, block: &Block, cx: &mut Context<'_>) {
        for (i, sector) in block.data().chunks(512).enumerate() {
            if sector.starts_with(&[0xFF, 0xD8, 0xFF]) {
                let offset = block.offset() + (i * 512) as u64;
                cx.report(index::NewFile::carved(
                    index::FileKind::Jpeg,
                    offset,
                    JPEG_LEN,
                ));
            }
        }
    }
}

struct Harness {
    helper: RealHelper,
    events: mpsc::Receiver<HelperEvent>,
    image: Vec<u8>,
    image_path: PathBuf,
    dest: tempfile::TempDir,
    _dir: tempfile::TempDir,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut image = vec![0u8; IMAGE_LEN];
    for (n, at) in JPEG_AT.iter().enumerate() {
        image[*at..*at + 3].copy_from_slice(&[0xFF, 0xD8, 0xFF]);
        for (i, b) in image[*at + 3..*at + JPEG_LEN as usize]
            .iter_mut()
            .enumerate()
        {
            *b = (i + n) as u8;
        }
    }
    let image_path = dir.path().join("card.img");
    std::fs::write(&image_path, &image).unwrap();

    let dest = tempfile::tempdir().unwrap();
    let dest_path = dest.path().to_string_lossy().into_owned();
    let (tx, events) = mpsc::channel();
    let tx = Mutex::new(tx);
    let helper = RealHelper::with_options(
        Box::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        }),
        Arc::new(|| vec![Box::new(SoiFinder) as Box<dyn Analyzer>]),
        dir.path().join("work"),
        Box::new(move |_| {
            vec![Destination {
                id: DestinationId("test".into()),
                kind: hakk_protocol::DriveKind::Folder,
                name: "test".into(),
                path: dest_path.clone(),
                free_bytes: 1 << 30,
                blocked: None,
            }]
        }),
    );
    Harness {
        helper,
        events,
        image,
        image_path,
        dest,
        _dir: dir,
    }
}

/// Collects events until `done` matches one, failing after a timeout.
fn until(
    events: &mpsc::Receiver<HelperEvent>,
    done: impl Fn(&HelperEvent) -> bool,
) -> Vec<HelperEvent> {
    let mut seen = Vec::new();
    loop {
        let e = events
            .recv_timeout(Duration::from_secs(20))
            .expect("timed out waiting for the helper");
        let stop = done(&e);
        seen.push(e);
        if stop {
            return seen;
        }
    }
}

#[test]
fn scans_an_image_and_recovers_what_it_found() {
    let h = harness();
    let drive = h
        .helper
        .open_image(&h.image_path.to_string_lossy())
        .unwrap();
    assert_eq!(drive.capacity_bytes, IMAGE_LEN as u64);
    assert_eq!(h.helper.list_drives().unwrap(), vec![drive.clone()]);
    assert_eq!(drive.saved_scan, None);

    let scan_id = h.helper.start_scan(&drive.id, true).unwrap();
    let events = until(&h.events, |e| matches!(e, HelperEvent::ScanFinished(_)));

    let found: Vec<_> = events
        .iter()
        .flat_map(|e| match e {
            HelperEvent::FilesFound(f) => f.files.clone(),
            _ => Vec::new(),
        })
        .collect();
    let mut offsets: Vec<_> = found.iter().filter_map(|f| f.offset).collect();
    offsets.sort_unstable();
    assert_eq!(offsets, JPEG_AT.map(|a| a as u64));
    assert!(
        found
            .iter()
            .all(|f| matches!(f.origin, FileOrigin::Carved { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HelperEvent::ScanProgress(_)))
    );
    let Some(HelperEvent::ScanFinished(summary)) = events.last() else {
        panic!("no summary");
    };
    assert_eq!(summary.scan_id, scan_id);
    assert!(!summary.cancelled);
    assert_eq!(
        summary.photos, 0,
        "4 KiB JPEGs count as too small and are hidden"
    );

    h.helper
        .recover(RecoveryRequest {
            scan_id,
            files: found.iter().map(|f| f.id).collect(),
            destination: DestinationId("test".into()),
        })
        .unwrap();
    let events = until(&h.events, |e| matches!(e, HelperEvent::RecoveryFinished(_)));
    let Some(HelperEvent::RecoveryFinished(report)) = events.last() else {
        panic!("no report");
    };
    assert_eq!((report.recovered, report.failed), (2, 0));

    for f in &found {
        let at = f.offset.unwrap() as usize;
        let written = std::fs::read(h.dest.path().join("unknown-date").join(&f.name)).unwrap();
        assert_eq!(written, h.image[at..at + JPEG_LEN as usize]);
    }
}

/// The files and the offsets found in `events`.
fn found_offsets(events: &[HelperEvent]) -> Vec<u64> {
    let mut offsets: Vec<u64> = events
        .iter()
        .flat_map(|e| match e {
            HelperEvent::FilesFound(f) => f.files.clone(),
            _ => Vec::new(),
        })
        .filter_map(|f| f.offset)
        .collect();
    offsets.sort_unstable();
    offsets
}

#[test]
fn a_finished_scan_is_saved_and_opened_again() {
    let h = harness();
    let drive = h
        .helper
        .open_image(&h.image_path.to_string_lossy())
        .unwrap();
    assert_eq!(drive.saved_scan, None);
    h.helper.start_scan(&drive.id, true).unwrap();
    until(&h.events, |e| matches!(e, HelperEvent::ScanFinished(_)));

    // The drive now shows its saved scan.
    let listed = h.helper.list_drives().unwrap();
    let saved = listed[0].saved_scan.clone().expect("saved scan");
    assert!(saved.complete);
    assert_eq!(saved.percent_done, 100);
    assert_eq!(saved.files_found, 2);

    // Continuing it reports the same files again without finding them twice.
    h.helper.start_scan(&drive.id, false).unwrap();
    let events = until(&h.events, |e| matches!(e, HelperEvent::ScanFinished(_)));
    assert_eq!(found_offsets(&events), JPEG_AT.map(|a| a as u64));
    assert_eq!(h.helper.list_drives().unwrap()[0].saved_scan, Some(saved));

    // Starting afresh forgets it.
    h.helper.start_scan(&drive.id, true).unwrap();
    let events = until(&h.events, |e| matches!(e, HelperEvent::ScanFinished(_)));
    assert_eq!(found_offsets(&events), JPEG_AT.map(|a| a as u64));
}

#[test]
fn refuses_unknown_drives_and_destinations() {
    let h = harness();
    assert_eq!(
        h.helper.start_scan(&DriveId("nope".into()), true),
        Err(HelperError::DriveNotFound)
    );
    let drive = h
        .helper
        .open_image(&h.image_path.to_string_lossy())
        .unwrap();
    let scan_id = h.helper.start_scan(&drive.id, true).unwrap();
    let err = h.helper.recover(RecoveryRequest {
        scan_id,
        files: vec![hakk_protocol::FileId(1)],
        destination: DestinationId("elsewhere".into()),
    });
    assert_eq!(err, Err(HelperError::DestinationNotFound));
    assert!(matches!(
        h.helper.open_image("/no/such/image.img"),
        Err(HelperError::CannotOpen { .. })
    ));
}

/// Scans a scenario image with the real analyzers, recovers everything found, and checks
/// the copies against the scenario's manifest. Run with
/// `HAKK_SCENARIO=work/<scenario> cargo test -p hakk-helper -- --ignored --nocapture`.
#[test]
#[ignore = "needs a scenario image (HAKK_SCENARIO)"]
fn recovers_a_scenario_image() {
    use sha2::{Digest, Sha256};
    use std::collections::HashSet;

    let dir = PathBuf::from(std::env::var("HAKK_SCENARIO").expect("HAKK_SCENARIO is not set"));
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    let work = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let dest_path = dest.path().to_string_lossy().into_owned();
    let (tx, events) = mpsc::channel();
    let tx = Mutex::new(tx);
    let helper = RealHelper::with_options(
        Box::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        }),
        Arc::new(scan::analyzers::default_set),
        work.path().to_path_buf(),
        Box::new(move |_| {
            vec![Destination {
                id: DestinationId("test".into()),
                kind: hakk_protocol::DriveKind::Folder,
                name: "test".into(),
                path: dest_path.clone(),
                free_bytes: 1 << 40,
                blocked: None,
            }]
        }),
    );
    let drive = helper
        .open_image(&dir.join("image.img").to_string_lossy())
        .unwrap();
    let scan_id = helper.start_scan(&drive.id, true).unwrap();
    let seen = until(&events, |e| matches!(e, HelperEvent::ScanFinished(_)));
    let mut shown = HashMap::new();
    for e in &seen {
        match e {
            HelperEvent::FilesFound(f) => shown.extend(f.files.iter().map(|f| (f.id, f.clone()))),
            HelperEvent::FilesMerged(m) => m.merges.iter().for_each(|m| {
                shown.remove(&m.from);
            }),
            _ => {}
        }
    }
    helper
        .recover(RecoveryRequest {
            scan_id,
            files: shown.keys().copied().collect(),
            destination: DestinationId("test".into()),
        })
        .unwrap();
    until(&events, |e| matches!(e, HelperEvent::RecoveryFinished(_)));

    let mut recovered = HashSet::new();
    for day in std::fs::read_dir(dest.path()).unwrap().flatten() {
        for file in std::fs::read_dir(day.path()).unwrap().flatten() {
            let digest = Sha256::digest(std::fs::read(file.path()).unwrap());
            recovered.insert(
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            );
        }
    }
    let files = manifest["files"].as_array().unwrap();
    let intact: Vec<_> = files
        .iter()
        .filter(|f| f["intact_bytes"] == f["size"])
        .collect();
    let matched = intact
        .iter()
        .filter(|f| recovered.contains(f["sha256"].as_str().unwrap()))
        .count();
    let named = shown
        .values()
        .filter(|f| matches!(f.origin, FileOrigin::FileSystem { .. }))
        .count();
    println!(
        "manifest: {} files, {} intact; shown: {} ({} named); recovered intact: {matched}/{}",
        files.len(),
        intact.len(),
        shown.len(),
        named,
        intact.len()
    );
    for f in &intact {
        if !recovered.contains(f["sha256"].as_str().unwrap()) {
            println!("  missing: {} ({} bytes)", f["path"], f["size"]);
        }
    }
    assert!(matched > 0, "nothing recovered intact");
}
