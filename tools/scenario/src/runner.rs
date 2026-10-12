//! Runs a scenario to produce the image and its ground-truth manifest.

use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    process::Command,
};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::{
    backend::{self, Backend},
    content::{self, Meta},
    locate::{self, Index},
    manifest::{FileEntry, Generator, Manifest, VERSION},
    raw,
    rng::Rng,
    scenario::{FsKind, Scenario, Step},
};

pub const IMAGE_NAME: &str = "image.img";

/// A file being tracked. Its content is kept until the final check.
struct Tracked {
    entry: FileEntry,
    bytes: Vec<u8>,
}

pub fn run(sc: &Scenario, out_dir: &Path) -> Result<Manifest> {
    fs::create_dir_all(out_dir)?;
    let mut rng = Rng::new(sc.seed);
    let label = format!("HK{:08X}", rng.next_u64() as u32);
    if sc.fs == FsKind::None {
        return run_raw(sc, out_dir, &mut rng);
    }
    let backend = backend::native(backend::Format {
        fs: sc.fs,
        label: label.clone(),
        size: sc.size.0,
        cluster: sc.cluster.map(|c| c.0),
        partition_start: sc.partition_start,
        partition_type: sc.partition_type,
    })?;
    let image = backend.create(out_dir)?;

    let mut files: Vec<Tracked> = Vec::new();
    for (step_no, step) in sc.steps.iter().enumerate() {
        match step {
            Step::Write {
                count,
                kind,
                min_size,
                max_size,
                dir,
                profile,
            } => {
                let first_new = files.len();
                with_mounted(backend.as_ref(), &image, |root| {
                    for _ in 0..*count {
                        let id = u32::try_from(files.len())?;
                        let target = rng.range(min_size.0, max_size.0);
                        let p = profile.as_deref();
                        let g = content::generate(*kind, p, &sc.profiles, target, &mut rng.fork())?;
                        let path = content::path(*kind, p, &sc.profiles, dir.as_deref(), id + 1);
                        if let Some(parent) = Path::new(&path).parent() {
                            fs::create_dir_all(root.join(parent))?;
                        }
                        let mut f = File::create(root.join(&path))
                            .with_context(|| format!("cannot create {path}"))?;
                        f.write_all(&g.bytes)?;
                        f.sync_all()?;
                        files.push(Tracked {
                            entry: entry(
                                id,
                                path,
                                *kind,
                                profile.clone(),
                                step_no,
                                &g.bytes,
                                g.meta,
                            ),
                            bytes: g.bytes,
                        });
                    }
                    Ok(())
                })?;
                // Record placement right after writing.
                let snapshot = backend::read_disk(&image, sc.size.0)?;
                let index = Index::new(&snapshot);
                for f in files.iter_mut().skip(first_new) {
                    f.entry.extents = index.locate(&f.bytes);
                    f.entry.fragmented = f.entry.extents.as_ref().is_some_and(|e| e.len() > 1);
                }
            }
            Step::Delete { fraction, dir } => {
                let in_dir = |path: &str| {
                    dir.as_ref()
                        .is_none_or(|d| path.starts_with(&format!("{}/", d.trim_end_matches('/'))))
                };
                let mut live: Vec<usize> = (0..files.len())
                    .filter(|&i| {
                        files
                            .get(i)
                            .is_some_and(|f| f.entry.deleted_at.is_none() && in_dir(&f.entry.path))
                    })
                    .collect();
                rng.shuffle(&mut live);
                let n = (live.len() as f64 * fraction).round() as usize;
                live.truncate(n);
                live.sort_unstable();
                with_mounted(backend.as_ref(), &image, |root| {
                    for &i in &live {
                        if let Some(f) = files.get_mut(i) {
                            fs::remove_file(root.join(&f.entry.path))
                                .with_context(|| format!("cannot delete {}", f.entry.path))?;
                            f.entry.deleted_at = Some(step_no);
                        }
                    }
                    Ok(())
                })?;
            }
            Step::Place { .. } => {
                anyhow::bail!("steps[{step_no}]: `place` needs fs = \"none\"");
            }
            Step::Format => {
                backend.format(&image)?;
                for f in files.iter_mut().filter(|f| f.entry.deleted_at.is_none()) {
                    f.entry.deleted_at = Some(step_no);
                }
            }
        }
    }

    // How much original content survives in the final image.
    let final_image = backend::read_disk(&image, sc.size.0)?;
    fs::write(out_dir.join(IMAGE_NAME), &final_image)?;
    fs::remove_file(&image)?;
    for f in &mut files {
        if let Some(ext) = &f.entry.extents {
            f.entry.intact_bytes = locate::intact_bytes(&final_image, &f.bytes, ext);
        }
    }

    Ok(Manifest {
        version: VERSION,
        scenario: sc.name.clone(),
        seed: sc.seed,
        fs: sc.fs,
        image: IMAGE_NAME.to_owned(),
        image_size: sc.size.0,
        partition_offset: partition_offset(&final_image),
        detected_fs: volume_info(&final_image).0,
        cluster_size: volume_info(&final_image).1,
        generator: generator(),
        files: files.into_iter().map(|f| f.entry).collect(),
    })
}

/// Runs a scenario without a file system: files are placed directly on the disk.
fn run_raw(sc: &Scenario, out_dir: &Path, rng: &mut Rng) -> Result<Manifest> {
    let (disk, placed) = raw::run(sc, rng)?;
    fs::write(out_dir.join(IMAGE_NAME), &disk)?;
    let files = placed
        .into_iter()
        .enumerate()
        .map(|(i, p)| {
            let bytes = &p.generated.bytes;
            let mut e = entry(
                u32::try_from(i).unwrap_or(u32::MAX),
                p.path,
                p.kind,
                p.profile,
                p.step,
                bytes,
                p.generated.meta,
            );
            // Without a file system nothing is allocated: every file counts as deleted.
            e.deleted_at = Some(p.step);
            e.intact_bytes = locate::intact_bytes(&disk, bytes, &p.extents);
            e.fragmented = p.extents.len() > 1;
            e.extents = Some(p.extents);
            e
        })
        .collect();
    Ok(Manifest {
        version: VERSION,
        scenario: sc.name.clone(),
        seed: sc.seed,
        fs: sc.fs,
        image: IMAGE_NAME.to_owned(),
        image_size: sc.size.0,
        partition_offset: 0,
        detected_fs: None,
        cluster_size: None,
        generator: generator(),
        files,
    })
}

/// Mounts, runs `f`, and unmounts whether or not `f` succeeded.
fn with_mounted(
    backend: &dyn Backend,
    image: &Path,
    f: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let m = backend.mount(image)?;
    let result = f(m.root());
    let unmounted = m.unmount();
    result.and(unmounted)
}

fn entry(
    id: u32,
    path: String,
    kind: crate::scenario::ContentKind,
    profile: Option<String>,
    step: usize,
    bytes: &[u8],
    meta: Meta,
) -> FileEntry {
    FileEntry {
        id,
        path,
        kind,
        profile,
        size: bytes.len() as u64,
        sha256: hex(&Sha256::digest(bytes)),
        written_at: step,
        deleted_at: None,
        extents: None,
        fragmented: false,
        intact_bytes: 0,
        meta,
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the file system starts: 0 without a partition table, or the first MBR partition.
/// Reference: MBR partition table (4 x 16-byte entries at offset 446; start LBA at entry +8).
fn partition_offset(image: &[u8]) -> u64 {
    if detect_fs(image.get(..512).unwrap_or_default()).is_some() {
        return 0;
    }
    let lba = image
        .get(446 + 8..446 + 12)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_le_bytes);
    if image.get(510..512) == Some([0x55, 0xAA].as_slice()) && lba > 0 {
        u64::from(lba) * 512
    } else {
        0
    }
}

/// The file system type named in a boot sector, if any.
/// Reference: Microsoft "exFAT file system specification" 3.1.2 (FileSystemName); the NTFS
/// boot sector's OEM ID "NTFS    " at 3 (Microsoft "NTFS overview", Boot Sector);
/// Microsoft "FAT32 File System Specification" (fatgen103) BS_FilSysType at 54 / 82.
fn detect_fs(vbr: &[u8]) -> Option<&'static str> {
    let at = |r: std::ops::Range<usize>| vbr.get(r).unwrap_or_default();
    if at(3..11) == b"EXFAT   " {
        Some("exfat")
    } else if at(3..11) == b"NTFS    " {
        Some("ntfs")
    } else if at(82..90) == b"FAT32   " {
        Some("fat32")
    } else if at(54..62) == b"FAT12   " {
        Some("fat12")
    } else if at(54..62) == b"FAT16   " {
        Some("fat16")
    } else {
        None
    }
}

/// The detected file system and its cluster size, read from the volume boot sector.
/// Reference: exFAT spec 3.1.14 BytesPerSectorShift / 3.1.15 SectorsPerClusterShift;
/// fatgen103 BPB_BytsPerSec (11) and BPB_SecPerClus (13).
fn volume_info(image: &[u8]) -> (Option<String>, Option<u64>) {
    let start = usize::try_from(partition_offset(image)).unwrap_or(0);
    let vbr = image.get(start..start + 512).unwrap_or_default();
    let fs = detect_fs(vbr);
    let cluster = match fs {
        Some("exfat") => {
            let bps = u32::from(*vbr.get(108).unwrap_or(&0));
            let spc = u32::from(*vbr.get(109).unwrap_or(&0));
            1u64.checked_shl(bps + spc)
        }
        Some("ntfs") => {
            // Sectors per cluster above 0x80 encode 2^(256 - n).
            let bps = vbr
                .get(11..13)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map_or(0, u16::from_le_bytes);
            let spc = *vbr.get(13).unwrap_or(&0);
            let spc = if spc <= 0x80 {
                Some(u64::from(spc))
            } else {
                1u64.checked_shl(256 - u32::from(spc))
            };
            spc.map(|s| u64::from(bps) * s)
        }
        Some(_) => {
            let bps = vbr
                .get(11..13)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map_or(0, u16::from_le_bytes);
            let spc = *vbr.get(13).unwrap_or(&0);
            Some(u64::from(bps) * u64::from(spc))
        }
        None => None,
    };
    (fs.map(str::to_owned), cluster)
}

fn generator() -> Generator {
    let os_version = if cfg!(target_os = "macos") {
        Command::new("sw_vers").arg("-productVersion").output()
    } else {
        Command::new("uname").arg("-r").output()
    }
    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    .unwrap_or_default();
    Generator {
        os: std::env::consts::OS.to_owned(),
        os_version,
        tool_version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}
