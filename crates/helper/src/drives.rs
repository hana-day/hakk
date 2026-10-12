//! Physical drives that can be scanned.
//!
//! macOS: whole physical disks from `diskutil list -plist physical`, described by
//! `diskutil info -plist` (model, size, internal or external, SSD or not, bus). They are
//! read through the raw device `/dev/rdiskN`, which bypasses the buffer cache. Opening it
//! needs administrator rights, and even root is refused by the system's privacy
//! protection unless the app responsible has Full Disk Access; so the device is opened by
//! `authopen`, which asks for the administrator password and hands the open (read-only)
//! file back. Other systems list none yet.
//!
//! Reference: diskutil(8) (`list`, `info`, `-plist` output keys); authopen(1)
//! (`-stdoutpipe`); unix(4) and recvmsg(2) (`SCM_RIGHTS`).

use std::path::PathBuf;

use hakk_protocol::{Drive, DriveId, DriveKind, DriveWarning};

/// A physical drive and how to read it.
#[derive(Debug, Clone)]
pub(crate) struct PhysicalDrive {
    pub drive: Drive,
    /// The raw device to read, e.g. `/dev/rdisk4`.
    pub device: PathBuf,
    /// The whole-disk identifier, e.g. `disk4`.
    pub disk: String,
    /// Stays the same when the drive is plugged in again (unlike `disk`): model, size and
    /// the partitions' UUIDs. Names the drive's saved scan.
    pub identity: String,
}

/// Prefix of the ids of physical drives.
const ID_PREFIX: &str = "disk:";

/// The physical drives attached now.
pub(crate) fn list() -> Vec<PhysicalDrive> {
    #[cfg(target_os = "macos")]
    {
        mac::list()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// The whole physical disk a path is stored on (e.g. `disk4`), following APFS containers
/// to the disk that holds them. `None` if it cannot be told.
pub(crate) fn disk_of(path: &std::path::Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        mac::disk_of(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// Opens a raw device read-only: directly if this process may, otherwise (macOS) through
/// `authopen`, which asks for the administrator password. `PermissionDenied` if the user
/// declines.
pub(crate) fn open_device(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    match std::fs::File::open(path) {
        #[cfg(target_os = "macos")]
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => mac::authopen(path),
        other => other,
    }
}

/// Whether `id` names a physical drive (as opposed to a disk image).
pub(crate) fn is_physical(id: &DriveId) -> bool {
    id.0.starts_with(ID_PREFIX)
}

/// What kind of drive the bus, media and size suggest.
fn kind(internal: bool, solid_state: bool, removable: bool, bus: &str) -> DriveKind {
    match (internal, solid_state, bus) {
        (_, _, "Secure Digital") => DriveKind::SdCard,
        (true, true, _) => DriveKind::InternalSsd,
        (true, false, _) => DriveKind::InternalHdd,
        // USB sticks report as removable solid-state media.
        (false, true, "USB") if removable => DriveKind::UsbFlash,
        (false, true, _) => DriveKind::ExternalSsd,
        (false, false, _) => DriveKind::ExternalHdd,
    }
}

/// The drive as the GUI shows it.
fn drive(
    disk: &str,
    name: String,
    size: u64,
    kind: DriveKind,
    file_system: Option<String>,
) -> Drive {
    let mut warnings = Vec::new();
    if matches!(kind, DriveKind::InternalSsd | DriveKind::ExternalSsd) {
        warnings.push(DriveWarning::TrimLikely);
    }
    Drive {
        id: DriveId(format!("{ID_PREFIX}{disk}")),
        kind,
        name,
        capacity_bytes: size,
        file_system,
        warnings,
        saved_scan: None,
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::{
        fs::File,
        io,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        path::Path,
        process::{Command, Stdio},
    };

    use plist::{Dictionary, Value};

    use super::{PhysicalDrive, drive, kind};

    /// Runs `diskutil <args> -plist` and parses its output.
    fn diskutil(args: &[&str]) -> Option<Dictionary> {
        let mut cmd = Command::new("/usr/sbin/diskutil");
        let (verb, rest) = args.split_first()?;
        cmd.arg(verb).arg("-plist").args(rest);
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        Value::from_reader_xml(out.stdout.as_slice())
            .ok()?
            .into_dictionary()
    }

    fn string(d: &Dictionary, key: &str) -> Option<String> {
        d.get(key)?.as_string().map(str::to_owned)
    }

    fn flag(d: &Dictionary, key: &str) -> bool {
        d.get(key).and_then(Value::as_boolean).unwrap_or(false)
    }

    fn number(d: &Dictionary, key: &str) -> u64 {
        d.get(key).and_then(Value::as_unsigned_integer).unwrap_or(0)
    }

    pub(super) fn list() -> Vec<PhysicalDrive> {
        let Some(listing) = diskutil(&["list", "physical"]) else {
            return Vec::new();
        };
        let whole: Vec<String> = listing
            .get("WholeDisks")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_string().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let partitions = partitions(&listing);
        let mut out = Vec::new();
        for disk in whole {
            let Some(info) = diskutil(&["info", &disk]) else {
                continue;
            };
            // APFS containers and disk images show up as virtual disks.
            if string(&info, "VirtualOrPhysical").as_deref() == Some("Virtual") {
                continue;
            }
            let size = number(&info, "TotalSize").max(number(&info, "Size"));
            if size == 0 {
                continue;
            }
            let bus = string(&info, "BusProtocol").unwrap_or_default();
            let k = kind(
                flag(&info, "Internal"),
                flag(&info, "SolidState"),
                flag(&info, "RemovableMedia") || flag(&info, "Removable"),
                &bus,
            );
            let mine: Vec<&(String, String, Option<String>)> =
                partitions.iter().filter(|p| p.0 == disk).collect();
            let model = string(&info, "MediaName")
                .map(|m| m.trim().to_owned())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| disk.clone());
            let volumes: Vec<&str> = mine
                .iter()
                .map(|p| p.1.as_str())
                .filter(|v| !v.is_empty())
                .collect();
            let name = if volumes.is_empty() {
                model
            } else {
                format!("{model} ({})", volumes.join(", "))
            };
            let file_system = mine.iter().find_map(|p| p.2.clone());
            let identity = format!(
                "disk|{}|{size}|{}",
                string(&info, "MediaName").unwrap_or_default().trim(),
                uuids(&listing, &disk).join(",")
            );
            out.push(PhysicalDrive {
                identity,
                drive: drive(&disk, name, size, k, file_system),
                device: format!("/dev/r{disk}").into(),
                disk,
            });
        }
        out
    }

    /// The partitions' UUIDs on each whole disk (GPT partition GUIDs, or volume UUIDs).
    fn uuids(listing: &Dictionary, disk: &str) -> Vec<String> {
        let mut out = Vec::new();
        let disks = listing
            .get("AllDisksAndPartitions")
            .and_then(Value::as_array);
        for d in disks.into_iter().flatten().filter_map(Value::as_dictionary) {
            if string(d, "DeviceIdentifier").as_deref() != Some(disk) {
                continue;
            }
            let parts = d.get("Partitions").and_then(Value::as_array);
            for p in parts.into_iter().flatten().filter_map(Value::as_dictionary) {
                out.extend(string(p, "DiskUUID").or_else(|| string(p, "VolumeUUID")));
            }
        }
        out
    }

    /// `(whole disk, volume name, file system)` of each partition with a volume.
    fn partitions(listing: &Dictionary) -> Vec<(String, String, Option<String>)> {
        let mut out = Vec::new();
        let Some(disks) = listing
            .get("AllDisksAndPartitions")
            .and_then(Value::as_array)
        else {
            return out;
        };
        for d in disks.iter().filter_map(Value::as_dictionary) {
            let Some(whole) = string(d, "DeviceIdentifier") else {
                continue;
            };
            let parts = d.get("Partitions").and_then(Value::as_array);
            for p in parts.into_iter().flatten().filter_map(Value::as_dictionary) {
                let volume = string(p, "VolumeName").unwrap_or_default();
                let id = string(p, "DeviceIdentifier");
                // EFI and recovery partitions have no volume name; skip them.
                let content = string(p, "Content").unwrap_or_default();
                if volume.is_empty() && content.contains("EFI") {
                    continue;
                }
                let fs = id
                    .and_then(|id| diskutil(&["info", &id]))
                    .and_then(|i| string(&i, "FilesystemName"));
                if volume.is_empty() && fs.is_none() {
                    continue;
                }
                out.push((whole.clone(), volume, fs));
            }
        }
        out
    }

    /// The mount point of the volume holding `path` (statfs `f_mntonname`).
    fn mount_point(path: &Path) -> Option<String> {
        use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut st = MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: `c` is a valid NUL-terminated path and `st` points to writable memory the
        // size of `statfs`, which the call fills in when it returns 0.
        if unsafe { libc::statfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: statfs returned 0, so it initialized `st`.
        let st = unsafe { st.assume_init() };
        let name: Vec<u8> = st
            .f_mntonname
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c.to_ne_bytes()[0])
            .collect();
        String::from_utf8(name).ok()
    }

    /// Opens `path` read-only through `/usr/libexec/authopen -stdoutpipe`: it asks for
    /// authorization (the administrator password) and sends the open descriptor back over
    /// a socket.
    pub(super) fn authopen(path: &Path) -> io::Result<File> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` has room for the two descriptors socketpair writes.
        if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: socketpair succeeded, so both descriptors are open and owned by us alone.
        let (ours, theirs) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let mut child = Command::new("/usr/libexec/authopen")
            .arg("-stdoutpipe")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(theirs))
            .spawn()?;
        let received = receive_fd(&ours);
        let status = child.wait()?;
        match received? {
            Some(fd) => Ok(File::from(fd)),
            None if status.success() => Err(io::Error::other("authopen sent no file")),
            // authopen exits with an error when the user cancels the password prompt.
            None => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("authopen refused {}", path.display()),
            )),
        }
    }

    /// Receives one descriptor sent with `SCM_RIGHTS` on `socket`; `None` if the other side
    /// closed without sending one.
    fn receive_fd(socket: &OwnedFd) -> io::Result<Option<OwnedFd>> {
        let mut data = [0u8; 64];
        let mut iov = libc::iovec {
            iov_base: data.as_mut_ptr().cast(),
            iov_len: data.len(),
        };
        // Room for one control message carrying one int, suitably aligned.
        let mut control = [0u64; 8];
        // SAFETY: an all-zero msghdr is valid; the fields used are set below.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &raw mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen =
            libc::socklen_t::try_from(std::mem::size_of_val(&control)).map_err(io::Error::other)?;
        // SAFETY: `msg` points at buffers that live for the call and whose sizes it states.
        let n = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut msg, 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: recvmsg filled `msg`; CMSG_FIRSTHDR reads only within its control buffer.
        let cmsg = unsafe { libc::CMSG_FIRSTHDR(&raw const msg) };
        if cmsg.is_null() {
            return Ok(None);
        }
        // SAFETY: `cmsg` is non-null and points into `control`.
        let header = unsafe { &*cmsg };
        if header.cmsg_level != libc::SOL_SOCKET || header.cmsg_type != libc::SCM_RIGHTS {
            return Ok(None);
        }
        // SAFETY: an SCM_RIGHTS message carries at least one int (a descriptor) as its data;
        // read_unaligned copes with the data's alignment.
        let fd = unsafe { std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<libc::c_int>()) };
        if fd < 0 {
            return Ok(None);
        }
        // SAFETY: the kernel installed `fd` in this process for us; nothing else owns it.
        Ok(Some(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    pub(super) fn disk_of(path: &Path) -> Option<String> {
        // The folder may not exist yet; its nearest existing ancestor decides.
        let existing = path.ancestors().find(|p| p.exists())?;
        let info = diskutil(&["info", &mount_point(existing)?])?;
        // An APFS volume lives in a container (a virtual disk); its physical store is a
        // partition of the real disk.
        let store = info
            .get("APFSPhysicalStores")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_dictionary)
            .and_then(|d| string(d, "APFSPhysicalStore"));
        match store {
            Some(part) => string(&diskutil(&["info", &part])?, "ParentWholeDisk"),
            None => string(&info, "ParentWholeDisk"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind(false, false, false, "USB"), DriveKind::ExternalHdd);
        assert_eq!(kind(false, true, true, "USB"), DriveKind::UsbFlash);
        assert_eq!(kind(false, true, false, "USB"), DriveKind::ExternalSsd);
        assert_eq!(
            kind(true, true, false, "Apple Fabric"),
            DriveKind::InternalSsd
        );
        assert_eq!(
            kind(false, false, true, "Secure Digital"),
            DriveKind::SdCard
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_mac_lists_its_disk() {
        let drives = list();
        assert!(!drives.is_empty(), "no physical disk listed");
        assert!(
            drives
                .iter()
                .all(|d| d.device.to_string_lossy().starts_with("/dev/rdisk"))
        );
        // The home folder is on one of them.
        let home = std::env::var("HOME").unwrap();
        let disk = disk_of(Path::new(&home)).unwrap();
        assert!(
            drives.iter().any(|d| d.disk == disk),
            "{disk} not among {drives:?}"
        );
    }

    #[cfg(target_os = "macos")]
    use std::path::Path;
}
