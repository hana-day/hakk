//! Copying found files to a destination.
//!
//! Data is read through the read layer only, so the source is never written. Files go
//! into one subfolder per capture day (`2025-08-14`, or `unknown-date`). Unreadable parts
//! are written as zeros and the file is counted as partially damaged.

use std::{
    fs::{self, File},
    io::{self, Write},
    ops::Range,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use blocksource::BlockSource;
use hakk_protocol::{
    FileId, FileStatus, HelperEvent, RecoveryId, RecoveryProgress, RecoveryReport,
};
use index::Index;

use crate::convert;

const CHUNK: usize = 1 << 20;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
const UNKNOWN_DATE: &str = "unknown-date";

pub(crate) struct Job {
    pub id: RecoveryId,
    pub files: Vec<FileId>,
    /// Folder the per-day subfolders are created in.
    pub folder: PathBuf,
    pub index_path: PathBuf,
    /// (user id, group id) to give the written files and folders to: the GUI's user when
    /// the helper runs as root.
    pub owner: Option<(u32, u32)>,
}

struct Item {
    name: String,
    day: String,
    damaged: bool,
    size: u64,
    extents: Vec<Range<u64>>,
}

/// Runs a recovery to the end, emitting progress, and returns the report.
pub(crate) fn run(
    job: &Job,
    source: &dyn BlockSource,
    emit: &dyn Fn(HelperEvent),
) -> RecoveryReport {
    let mut report = RecoveryReport {
        recovery_id: job.id,
        recovered: 0,
        partially_damaged: 0,
        failed: 0,
        folder: job.folder.to_string_lossy().into_owned(),
    };
    let items = match load(job) {
        Ok(items) => items,
        Err(err) => {
            eprintln!("recovery: cannot read the index: {err}");
            report.failed = count(job.files.len());
            return report;
        }
    };
    let total: u64 = items.iter().flatten().map(|i| i.size).sum();
    let mut progress = RecoveryProgress {
        recovery_id: job.id,
        files_done: 0,
        files_total: count(job.files.len()),
        bytes_done: 0,
        bytes_total: total,
    };
    let mut last_emit = Instant::now();
    emit(HelperEvent::RecoveryProgress(progress.clone()));

    for item in items {
        let outcome = item.as_ref().map(|item| {
            copy(item, source, &job.folder, &mut |n| {
                progress.bytes_done += n;
                if last_emit.elapsed() >= PROGRESS_INTERVAL {
                    last_emit = Instant::now();
                    emit(HelperEvent::RecoveryProgress(progress.clone()));
                }
            })
        });
        match outcome {
            Some(Ok(holes)) => {
                report.recovered += 1;
                if holes || item.as_ref().is_some_and(|i| i.damaged) {
                    report.partially_damaged += 1;
                }
            }
            Some(Err(err)) => {
                eprintln!("recovery: failed to write a file: {err}");
                report.failed += 1;
            }
            None => report.failed += 1,
        }
        progress.files_done += 1;
    }
    progress.bytes_done = total;
    if let Some((uid, gid)) = job.owner {
        hand_over(&job.folder, uid, gid);
    }
    emit(HelperEvent::RecoveryProgress(progress));
    report
}

/// Gives `dir` and everything in it to the user `uid` (group `gid`).
#[cfg(unix)]
pub(crate) fn hand_over(dir: &Path, uid: u32, gid: u32) {
    let _ = std::os::unix::fs::lchown(dir, Some(uid), Some(gid));
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            hand_over(&path, uid, gid);
        } else {
            let _ = std::os::unix::fs::lchown(&path, Some(uid), Some(gid));
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn hand_over(_dir: &Path, _uid: u32, _gid: u32) {}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Looks up each file; `None` for files that cannot be recovered (unknown or no extents).
fn load(job: &Job) -> Result<Vec<Option<Item>>, index::Error> {
    let index = Index::open(&job.index_path)?;
    let mut out = Vec::with_capacity(job.files.len());
    for id in &job.files {
        let record = i64::try_from(id.0)
            .ok()
            .map(|id| index.file(id))
            .transpose()?
            .flatten();
        let Some(record) = record else {
            out.push(None);
            continue;
        };
        let extents = index.extents(record.id)?;
        if extents.is_empty() {
            out.push(None);
            continue;
        }
        let file = convert::found_file(&record, Vec::new());
        out.push(Some(Item {
            name: safe_file_name(&file.name),
            day: file
                .date
                .as_ref()
                .and_then(|d| d.local.get(..10))
                .map_or_else(|| UNKNOWN_DATE.to_owned(), str::to_owned),
            damaged: matches!(
                file.status,
                FileStatus::PartiallyDamaged | FileStatus::IndexMissing
            ),
            size: record.size,
            extents,
        }));
    }
    Ok(out)
}

/// Writes one file. Returns whether some of it could not be read.
fn copy(
    item: &Item,
    source: &dyn BlockSource,
    folder: &Path,
    progress: &mut dyn FnMut(u64),
) -> io::Result<bool> {
    let dir = folder.join(&item.day);
    fs::create_dir_all(&dir)?;
    let mut out = create_unique(&dir, &item.name)?;
    let mut left = item.size;
    let mut holes = false;
    let mut buf = vec![0u8; CHUNK];
    for extent in &item.extents {
        let mut at = extent.start;
        while at < extent.end && left > 0 {
            let want = (extent.end - at).min(left).min(CHUNK as u64);
            let want = usize::try_from(want).unwrap_or(CHUNK);
            let Some(chunk) = buf.get_mut(..want) else {
                break;
            };
            match source.read_at(at, chunk) {
                Ok(n) if n == want => {}
                Ok(n) => {
                    // Past the end of the source: the rest is missing.
                    chunk
                        .get_mut(n..)
                        .into_iter()
                        .flatten()
                        .for_each(|b| *b = 0);
                    holes = true;
                }
                Err(_) => {
                    chunk.fill(0);
                    holes = true;
                }
            }
            out.write_all(chunk)?;
            at += want as u64;
            left -= want as u64;
            progress(want as u64);
        }
    }
    if left > 0 {
        // The extents are shorter than the file: what is missing was never located.
        holes = true;
        progress(left);
    }
    out.sync_all()?;
    Ok(holes)
}

/// Creates `dir/name`, or `dir/stem (2).ext` and so on if it exists. Never overwrites.
fn create_unique(dir: &Path, name: &str) -> io::Result<File> {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1..10_000 {
        let candidate = match (n, &ext) {
            (1, _) => name.to_owned(),
            (_, Some(ext)) => format!("{stem} ({n}).{ext}"),
            (_, None) => format!("{stem} ({n})"),
        };
        match File::create_new(dir.join(candidate)) {
            Ok(f) => return Ok(f),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many files with the same name",
    ))
}

/// Makes a name from the disk safe as a file name on every OS.
fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        "file".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blocksource::MemSource;

    fn item(extents: Vec<Range<u64>>, size: u64) -> Item {
        Item {
            name: "IMG_0001.JPG".into(),
            day: "2025-08-14".into(),
            damaged: false,
            size,
            extents,
        }
    }

    #[test]
    fn joins_extents_and_truncates_to_the_file_size() {
        let data: Vec<u8> = (0..=255u8).cycle().take(8192).collect();
        let src = MemSource::new(data.clone());
        let dir = tempfile::tempdir().unwrap();
        let holes = copy(
            &item(vec![4096..5120, 0..1024], 1500),
            &src,
            dir.path(),
            &mut |_| {},
        )
        .unwrap();
        assert!(!holes);
        let out = fs::read(dir.path().join("2025-08-14/IMG_0001.JPG")).unwrap();
        let mut want = data[4096..5120].to_vec();
        want.extend_from_slice(&data[0..476]);
        assert_eq!(out, want);
    }

    #[test]
    fn never_overwrites() {
        let src = MemSource::new(vec![1u8; 1024]);
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            copy(
                &item(std::iter::once(0..512).collect(), 512),
                &src,
                dir.path(),
                &mut |_| {},
            )
            .unwrap();
        }
        assert!(dir.path().join("2025-08-14/IMG_0001 (2).JPG").exists());
    }

    #[test]
    fn missing_data_is_reported_as_holes() {
        let src = MemSource::new(vec![1u8; 1024]);
        let dir = tempfile::tempdir().unwrap();
        let holes = copy(
            &item(std::iter::once(512..1024).collect(), 2048),
            &src,
            dir.path(),
            &mut |_| {},
        )
        .unwrap();
        assert!(holes);
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_file_name("?MG_0025.JPG"), "_MG_0025.JPG");
        assert_eq!(safe_file_name("../x"), "_x");
        assert_eq!(safe_file_name(" . "), "file");
    }
}
