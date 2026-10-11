//! Walks a whole source and carves every JPEG and MP4/QuickTime file it finds.
//!
//! Files start on sector boundaries (they start on cluster boundaries in any file system),
//! so only sector starts are checked for headers. After a complete file the scan resumes
//! after it; after a broken one it resumes at the next sector, since the rest of the data
//! may belong to other files.

use blocksource::BlockSource;

use crate::{Found, Kind, Status, fragments, jpeg, mp4, reader::Reader};

/// Scan limits.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Longest JPEG to follow.
    pub max_jpeg: u64,
    /// Longest video to follow.
    pub max_video: u64,
    /// Search for the missing fragments of broken JPEGs (`None` to skip it).
    pub reassemble: Option<fragments::Options>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_jpeg: 64 << 20,
            max_video: 64 << 30,
            reassemble: Some(fragments::Options::default()),
        }
    }
}

const CHUNK: usize = 4 << 20;

/// Scans `src` from start to end.
pub fn scan<S: BlockSource + ?Sized>(src: &S, opts: &Options) -> Vec<Found> {
    let ss = u64::from(src.sector_size()).max(512);
    let step = usize::try_from(ss).unwrap_or(512);
    let mut parse = Reader::new(src);
    let mut buf = vec![0u8; CHUNK];
    let mut found = Vec::new();
    let mut pos = 0u64;
    let mut skip_until = 0u64;
    while pos < src.len() {
        let Ok(n) = src.read_at(pos, &mut buf) else {
            // Unreadable: move on (bad ranges are retried by the scan pipeline, not here).
            pos += CHUNK as u64;
            continue;
        };
        if n == 0 {
            break;
        }
        let data = buf.get(..n).unwrap_or_default();
        for off in (0..n).step_by(step) {
            let at = pos + off as u64;
            if at < skip_until {
                continue;
            }
            let head = data.get(off..off + 8).unwrap_or_default();
            let hit = if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
                jpeg::carve(&mut parse, at, opts.max_jpeg).map(|j| {
                    let status = match j.status {
                        jpeg::Status::Complete => Status::Complete,
                        jpeg::Status::Corrupt { at } => Status::Corrupt { at },
                        jpeg::Status::Truncated => Status::Truncated,
                    };
                    let broken = matches!(status, Status::Corrupt { .. });
                    let joined = opts
                        .reassemble
                        .filter(|_| broken)
                        .and_then(|o| fragments::reassemble(&mut parse, at, opts.max_jpeg, &o))
                        .filter(|r| r.complete);
                    match joined {
                        Some(r) => Found {
                            offset: at,
                            len: r.fragments.iter().map(|f| f.1).sum(),
                            kind: Kind::Jpeg,
                            status: Status::Complete,
                            fragments: r.fragments,
                        },
                        None => Found::contiguous(at, j.len, Kind::Jpeg, status),
                    }
                })
            } else if head.get(4..8) == Some(b"ftyp") {
                mp4::carve(&mut parse, at, opts.max_video).map(|m| {
                    let kind = if &m.brand == b"qt  " {
                        Kind::Mov
                    } else {
                        Kind::Mp4
                    };
                    let status = match m.status {
                        mp4::Status::Complete => Status::Complete,
                        mp4::Status::Corrupt { at } => Status::Corrupt { at },
                        mp4::Status::Truncated => Status::Truncated,
                    };
                    Found::contiguous(at, m.len, kind, status)
                })
            } else {
                None
            };
            if let Some(f) = hit {
                if f.status == Status::Complete && f.fragments.len() == 1 {
                    skip_until = at + f.len;
                }
                found.push(f);
            }
        }
        pos += n as u64;
    }
    found
}
