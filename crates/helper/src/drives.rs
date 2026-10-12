//! Physical drives that can be scanned.
//!
//! macOS: whole physical disks from `diskutil list -plist physical`, described by
//! `diskutil info -plist` (model, size, internal or external, SSD or not, bus). They are
//! read through the raw device `/dev/rdiskN`, which bypasses the buffer cache. Other
//! systems list none yet.
//!
//! Reference: diskutil(8) (`list`, `info`, `-plist` output keys).

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
    use std::{path::Path, process::Command};

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
