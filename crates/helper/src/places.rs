//! Places recovered files can be written to.
//!
//! The user's Pictures folder, plus every mounted external drive. Recovered files go into a
//! `hakk` folder inside the chosen place.

use std::path::{Path, PathBuf};

use hakk_protocol::{Destination, DestinationId, DriveKind};

/// Folder created inside a destination to hold recovered files.
const FOLDER: &str = "hakk";

/// Lists destinations. `blocked_disk` is the whole physical disk being scanned (e.g.
/// `disk4`); destinations stored on it are refused, since writing there could overwrite
/// the deleted data being recovered.
pub(crate) fn destinations(blocked_disk: Option<&str>) -> Vec<Destination> {
    let mut out = Vec::new();
    if let Some(pictures) = pictures_dir() {
        let name = pictures.file_name().map_or_else(
            || "Pictures".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
        out.push(destination(
            "pictures".into(),
            DriveKind::Folder,
            name,
            &pictures,
        ));
    }
    for (name, root) in mounted_drives() {
        out.push(destination(
            format!("drive:{}", root.display()),
            DriveKind::ExternalDrive,
            name,
            &root,
        ));
    }
    if let Some(disk) = blocked_disk {
        for d in &mut out {
            // A destination whose disk cannot be told is refused too: better safe.
            let on = crate::drives::disk_of(Path::new(&d.path));
            if on.as_deref().is_none_or(|on| on == disk) {
                d.blocked = Some(hakk_protocol::DestinationBlock::SameDeviceAsSource);
            }
        }
    }
    out
}

fn destination(id: String, kind: DriveKind, name: String, root: &Path) -> Destination {
    Destination {
        id: DestinationId(id),
        kind,
        name,
        path: root.join(FOLDER).to_string_lossy().into_owned(),
        free_bytes: free_bytes(root).unwrap_or(0),
        blocked: None,
    }
}

fn pictures_dir() -> Option<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    let dir = PathBuf::from(home).join("Pictures");
    dir.is_dir().then_some(dir)
}

/// Mounted drives other than the system drive, as `(display name, root)`.
fn mounted_drives() -> Vec<(String, PathBuf)> {
    if cfg!(windows) {
        // Drive letters from D: on; A: and B: are floppies and C: is the system drive.
        return (b'D'..=b'Z')
            .map(|l| PathBuf::from(format!("{}:\\", char::from(l))))
            .filter(|p| p.is_dir())
            .map(|p| (p.display().to_string(), p))
            .collect();
    }
    let parents: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec![PathBuf::from("/Volumes")]
    } else {
        let user = std::env::var("USER").unwrap_or_default();
        vec![
            PathBuf::from("/media").join(&user),
            PathBuf::from("/run/media").join(&user),
        ]
    };
    let mut out = Vec::new();
    for parent in parents {
        let Ok(entries) = std::fs::read_dir(&parent) else {
            continue;
        };
        for entry in entries.flatten() {
            // On macOS the system volume appears as a symlink to `/`; skip links.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && !kind.is_symlink() {
                out.push((
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.path(),
                ));
            }
        }
    }
    out.sort();
    out
}

#[cfg(unix)]
fn free_bytes(path: &Path) -> Option<u64> {
    use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a valid NUL-terminated path and `st` points to writable memory the
    // size of `statvfs`, which the call fills in when it returns 0.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: statvfs returned 0, so it initialized `st`.
    let st = unsafe { st.assume_init() };
    #[allow(clippy::useless_conversion)] // The field types differ between platforms.
    Some(u64::from(st.f_bavail).saturating_mul(u64::from(st.f_frsize)))
}

#[cfg(windows)]
fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is NUL-terminated and `free` is a valid out pointer; the other
    // outputs are optional and passed as null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_bytes(dir.path()).unwrap() > 0);
    }
}
