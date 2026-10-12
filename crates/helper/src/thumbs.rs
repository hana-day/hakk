//! Thumbnails of found files, made with the operating system's own decoders.
//!
//! The file's data is read from the drive (through its extents, like a recovery) into a
//! temporary file, which macOS turns into a small picture: `sips` (ImageIO) for photos,
//! including HEIC and camera RAW formats, and `qlmanage` (Quick Look, AVFoundation) for a
//! frame of a video. Results are kept in a cache folder per scan, so each file is decoded
//! once per size. Other systems have no thumbnails yet.
//!
//! Reference: sips(1), qlmanage(1).

use std::{
    fs,
    io::{self, Write},
    ops::Range,
    path::{Path, PathBuf},
};

use blocksource::BlockSource;
use index::{FileRecord, Media};

/// Largest photo read for a thumbnail.
const MAX_PHOTO: u64 = 200 << 20;
/// Bytes of a video read for a thumbnail (the first frames and, usually, the index).
const MAX_VIDEO: u64 = 256 << 20;
/// Bytes read per request to the source.
const CHUNK: usize = 1 << 20;

/// A thumbnail: media type and bytes.
pub(crate) struct Picture {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

/// Makes (or takes from `cache`) a picture of `record`, at most `size` pixels on its longer
/// side, reading its `extents` from `source`.
pub(crate) fn thumbnail(
    record: &FileRecord,
    extents: &[Range<u64>],
    source: &dyn BlockSource,
    size: u32,
    cache: &Path,
) -> io::Result<Picture> {
    fs::create_dir_all(cache)?;
    let cached = cache.join(format!("{}-{size}.jpg", record.id));
    if let Ok(bytes) = fs::read(&cached) {
        return Ok(Picture {
            mime: "image/jpeg",
            bytes,
        });
    }
    let video = record.kind.media() == Media::Video;
    let cap = if video { MAX_VIDEO } else { MAX_PHOTO };
    if !video && record.size > cap {
        return Err(io::Error::other("too large to preview"));
    }
    // Decoders tell formats by the extension.
    let input = cache.join(format!("in-{}.{}", record.id, record.kind.code()));
    let written = write_data(source, extents, record.size.min(cap), &input);
    let made = written.and_then(|()| {
        if video {
            video_frame(&input, size, cache, &cached)
        } else {
            photo(&input, size, &cached)
        }
    });
    let _ = fs::remove_file(&input);
    made?;
    Ok(Picture {
        mime: "image/jpeg",
        bytes: fs::read(&cached)?,
    })
}

/// Copies the first `len` bytes of the file at `extents` into `path`.
fn write_data(
    source: &dyn BlockSource,
    extents: &[Range<u64>],
    len: u64,
    path: &Path,
) -> io::Result<()> {
    let mut out = io::BufWriter::new(fs::File::create(path)?);
    let mut left = len;
    let mut buf = vec![0u8; CHUNK];
    for e in extents {
        let mut at = e.start;
        while at < e.end && left > 0 {
            let want = usize::try_from((e.end - at).min(left).min(CHUNK as u64)).unwrap_or(CHUNK);
            let chunk = buf
                .get_mut(..want)
                .ok_or_else(|| io::Error::other("buffer"))?;
            let n = source.read_at(at, chunk).map_err(io::Error::other)?;
            if n == 0 {
                break;
            }
            out.write_all(chunk.get(..n).unwrap_or_default())?;
            at += n as u64;
            left -= n as u64;
        }
    }
    out.flush()
}

#[cfg(target_os = "macos")]
fn photo(input: &Path, size: u32, output: &Path) -> io::Result<()> {
    run(std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg", "-s", "formatOptions", "80", "-Z"])
        .arg(size.to_string())
        .arg(input)
        .arg("--out")
        .arg(output))
}

#[cfg(target_os = "macos")]
fn video_frame(input: &Path, size: u32, dir: &Path, output: &Path) -> io::Result<()> {
    run(std::process::Command::new("/usr/bin/qlmanage")
        .args(["-t", "-s"])
        .arg(size.to_string())
        .arg("-o")
        .arg(dir)
        .arg(input))?;
    // Quick Look writes `<input name>.png`; turn it into the cached JPEG.
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let png: PathBuf = dir.join(format!("{name}.png"));
    let made = photo(&png, size, output);
    let _ = fs::remove_file(&png);
    made
}

#[cfg(target_os = "macos")]
fn run(cmd: &mut std::process::Command) -> io::Result<()> {
    let out = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn photo(_input: &Path, _size: u32, _output: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no thumbnails on this system yet",
    ))
}

#[cfg(not(target_os = "macos"))]
fn video_frame(_input: &Path, _size: u32, _dir: &Path, _output: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no thumbnails on this system yet",
    ))
}
