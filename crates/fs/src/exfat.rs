//! exFAT.
//!
//! Reference: Microsoft "exFAT file system specification" (learn.microsoft.com),
//! 3.1 Main and Backup Boot Sector Sub-regions, 4.1 First and Second FAT Sub-regions,
//! 6.2-6.4 Generic DirectoryEntry templates, 7.1 Allocation Bitmap Directory Entry,
//! 7.3 Volume Label Directory Entry, 7.4 File Directory Entry, 7.6 Stream Extension
//! Directory Entry and 7.7 File Name Directory Entry.
//!
//! Deleting a file clears the InUse bit (6.2.1.4) of each entry of its set, keeping the
//! name, size and first cluster, and frees its clusters in the allocation bitmap. When the
//! Stream Extension has NoFatChain set the data is one contiguous run, so its placement is
//! known exactly; otherwise the FAT chain may have survived (exFAT tracks allocation in the
//! bitmap, not the FAT) and is followed if it is consistent with the size.

use std::collections::{HashSet, VecDeque};

use crate::{
    Confidence, Entry, Error, FsKind, Layout, Volume,
    disk::Disk,
    heap::{BAD, END, FREE, Heap, INVALID, Run, available_clusters, unvisited_prefix},
    le::{sanitize_name, u8_at, u16_at, u32_at, u64_at, utf16},
    time::Timestamp,
};

const ENTRY: usize = 32;
/// Directories are at most 256 MiB (7.6.7 DataLength Field).
const MAX_DIR_BYTES: u64 = 256 << 20;

/// EntryType bits (6.2.1): TypeCode 0-4, TypeImportance 5, TypeCategory 6, InUse 7.
const IN_USE: u8 = 0x80;
const CATEGORY_SECONDARY: u8 = 0x40;
/// EntryType values without the InUse bit.
const TYPE_FILE: u8 = 0x05;
const TYPE_STREAM: u8 = 0x40;
const TYPE_FILE_NAME: u8 = 0x41;
const TYPE_BITMAP: u8 = 0x81;
const TYPE_VOLUME_LABEL: u8 = 0x83;

const ATTR_DIRECTORY: u16 = 0x10;
/// GeneralSecondaryFlags (6.4.2): AllocationPossible bit 0, NoFatChain bit 1.
const FLAG_ALLOCATION_POSSIBLE: u8 = 0x01;
const FLAG_NO_FAT_CHAIN: u8 = 0x02;

#[derive(Debug, Clone)]
pub(crate) struct Boot {
    cluster_size: u64,
    volume_len: u64,
    fat_offset: u64,
    fat_len: u64,
    heap_offset: u64,
    cluster_count: u32,
    root_cluster: u32,
    /// 3.1.13.1 ActiveFat: which FAT and allocation bitmap are in use.
    active: u8,
}

/// Parses and validates a Main (or Backup) Boot Sector (3.1).
fn parse_boot(b: &[u8]) -> Option<Boot> {
    if b.len() < 512 || b.get(3..11)? != b"EXFAT   " {
        return None;
    }
    // 3.1.3 MustBeZero: guards against FAT tools misreading the volume.
    if b.get(11..64)?.iter().any(|&x| x != 0) {
        return None;
    }
    let bps_shift = u32::from(u8_at(b, 108));
    let spc_shift = u32::from(u8_at(b, 109));
    let num_fats = u8_at(b, 110);
    if !(9..=12).contains(&bps_shift)
        || spc_shift > 25 - bps_shift
        || !(1..=2).contains(&num_fats)
        || u16_at(b, 510) != 0xAA55
    {
        return None;
    }
    let sector_size = 1u64 << bps_shift;
    let fat_offset = u64::from(u32_at(b, 80));
    let fat_len = u64::from(u32_at(b, 84));
    let heap_offset = u64::from(u32_at(b, 88));
    let cluster_count = u32_at(b, 92);
    let root_cluster = u32_at(b, 96);
    let volume_len = u64_at(b, 72);
    if fat_offset < 24
        || fat_len == 0
        || heap_offset < fat_offset + fat_len * u64::from(num_fats)
        || cluster_count == 0
        || cluster_count > 0xFFFF_FFF5
        || u64::from(cluster_count) > volume_len.saturating_sub(heap_offset) >> spc_shift
        || root_cluster < 2
        || u64::from(root_cluster) > u64::from(cluster_count) + 1
        || fat_len * sector_size < (u64::from(cluster_count) + 2) * 4
    {
        return None;
    }
    let active = if num_fats == 2 {
        (u16_at(b, 106) & 1) as u8
    } else {
        0
    };
    Some(Boot {
        cluster_size: sector_size << spc_shift,
        volume_len: volume_len.saturating_mul(sector_size),
        fat_offset: fat_offset * sector_size,
        fat_len: fat_len * sector_size,
        heap_offset: heap_offset * sector_size,
        cluster_count,
        root_cluster,
        active,
    })
}

pub(crate) fn is_boot_sector(sector: &[u8]) -> bool {
    parse_boot(sector).is_some()
}

/// Looks for an exFAT boot sector, falling back to the backup boot region (sector 12).
pub(crate) fn probe(disk: &Disk) -> Result<Option<Boot>, Error> {
    let main = disk.read(0, 512)?;
    if let Some(b) = parse_boot(&main) {
        return Ok(Some(b));
    }
    if main.get(3..11) != Some(b"EXFAT   ".as_slice()) {
        return Ok(None);
    }
    for sector in [512u64, 1024, 2048, 4096] {
        if let Some(b) = disk
            .read(12 * sector, 512)
            .ok()
            .and_then(|b| parse_boot(&b))
        {
            return Ok(Some(b));
        }
    }
    Ok(None)
}

/// Reads the FAT in use into normalized entries (4.1: FFFFFFFFh ends a chain, FFFFFFF7h
/// marks a bad cluster, 2..ClusterCount+1 point to the next cluster).
fn read_fat(disk: &Disk, boot: &Boot, warnings: &mut Vec<String>) -> Vec<u32> {
    let end = boot.cluster_count.saturating_add(2);
    let bytes = (u64::from(end) * 4).min(boot.fat_len);
    let offset = boot.fat_offset + u64::from(boot.active) * boot.fat_len;
    let raw = disk.read_lossy(offset, usize::try_from(bytes).unwrap_or(0), warnings);
    (0..end)
        .map(|c| match u32_at(&raw, c as usize * 4) {
            _ if c < 2 => INVALID,
            0 => FREE,
            0xFFFF_FFFF => END,
            0xFFFF_FFF7 => BAD,
            v if v >= 2 && v < end => v,
            _ => INVALID,
        })
        .collect()
}

/// A File directory entry set (7.4, 7.6, 7.7).
#[derive(Debug, Clone)]
struct Set {
    in_use: bool,
    attributes: u16,
    name: String,
    name_exact: bool,
    checksum_ok: bool,
    no_fat_chain: bool,
    allocation_possible: bool,
    first_cluster: u32,
    data_len: u64,
    valid_len: u64,
    created: Option<Timestamp>,
    modified: Option<Timestamp>,
    accessed: Option<Timestamp>,
}

/// 6.3.3 SetChecksum Field (Figure 2): over every byte of the set except the checksum
/// itself. For a deleted set the InUse bits are put back first, since they were set when
/// the checksum was computed.
fn set_checksum(entries: &[&[u8]], restore_in_use: bool) -> u16 {
    let mut sum = 0u16;
    for (i, e) in entries.iter().enumerate() {
        for (j, &b) in e.iter().enumerate() {
            if i == 0 && (j == 2 || j == 3) {
                continue;
            }
            let b = if j == 0 && restore_in_use {
                b | IN_USE
            } else {
                b
            };
            sum = (if sum & 1 != 0 { 0x8000u16 } else { 0 })
                .wrapping_add(sum >> 1)
                .wrapping_add(u16::from(b));
        }
    }
    sum
}

/// Parses the File entry sets of a directory, live and deleted. Parsing stops at an
/// end-of-directory entry (EntryType 00h, 6.2.1).
fn parse_dir(bytes: &[u8]) -> Vec<Set> {
    let entries: Vec<&[u8]> = bytes.chunks_exact(ENTRY).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(e) = entries.get(i) {
        let t = u8_at(e, 0);
        if t == 0 {
            break;
        }
        if t & !IN_USE != TYPE_FILE {
            i += 1;
            continue;
        }
        let (set, used) = parse_set(entries.get(i..).unwrap_or_default());
        out.extend(set);
        i += used.max(1);
    }
    out
}

/// Parses a set starting with a File entry. Returns it (if its Stream Extension survives)
/// and how many entries it spans.
fn parse_set(entries: &[&[u8]]) -> (Option<Set>, usize) {
    let Some(file) = entries.first() else {
        return (None, 1);
    };
    let in_use = u8_at(file, 0) & IN_USE != 0;
    // 7.4.2: a File entry has at least a Stream Extension and one File Name entry, and at
    // most 17 File Name entries.
    let count = usize::from(u8_at(file, 1));
    if !(2..=18).contains(&count) {
        return (None, 1);
    }
    // Secondaries must follow with the same InUse state; a deleted set may have been
    // partly overwritten by newer entries.
    let secondaries: Vec<&[u8]> = entries
        .iter()
        .skip(1)
        .take(count)
        .take_while(|s| {
            let t = u8_at(s, 0);
            t != 0 && t & CATEGORY_SECONDARY != 0 && (t & IN_USE != 0) == in_use
        })
        .copied()
        .collect();
    let used = 1 + secondaries.len();
    let Some(stream) = secondaries
        .first()
        .filter(|s| u8_at(s, 0) & !IN_USE == TYPE_STREAM)
    else {
        return (None, used);
    };
    let complete = secondaries.len() == count;
    let checksum_ok = complete && {
        let all: Vec<&[u8]> = entries.iter().take(used).copied().collect();
        set_checksum(&all, !in_use) == u16_at(file, 2)
    };
    let name_len = usize::from(u8_at(stream, 3));
    let mut units: Vec<u16> = Vec::new();
    for s in secondaries.iter().skip(1) {
        if u8_at(s, 0) & !IN_USE == TYPE_FILE_NAME {
            units.extend((0..15).map(|k| u16_at(s, 2 + 2 * k)));
        }
    }
    let got_all = name_len > 0 && units.len() >= name_len;
    units.truncate(name_len);
    let (name, clean) = sanitize_name(utf16(&units));
    let flags = u8_at(stream, 1);
    let ts = |at: usize, inc: Option<usize>, utc: usize| {
        // Time in the low half, date in the high half (7.4.8).
        Timestamp::from_dos(u16_at(file, at + 2), u16_at(file, at)).map(|t| {
            let t = match inc {
                Some(i) => t.with_10ms(u8_at(file, i)),
                None => t,
            };
            t.with_exfat_utc_offset(u8_at(file, utc))
        })
    };
    let data_len = u64_at(stream, 24);
    let set = Set {
        in_use,
        attributes: u16_at(file, 4),
        name,
        name_exact: got_all && clean && checksum_ok,
        checksum_ok,
        no_fat_chain: flags & FLAG_NO_FAT_CHAIN != 0,
        allocation_possible: flags & FLAG_ALLOCATION_POSSIBLE != 0,
        first_cluster: u32_at(stream, 20),
        data_len,
        valid_len: u64_at(stream, 8).min(data_len),
        created: ts(8, Some(20), 22),
        modified: ts(12, Some(21), 23),
        accessed: ts(16, None, 24),
    };
    (Some(set), used)
}

/// The root directory's critical entries (7.1, 7.3).
struct RootInfo {
    bitmaps: Vec<(u8, u32, u64)>,
    label: Option<String>,
}

fn parse_root_info(bytes: &[u8]) -> RootInfo {
    let mut info = RootInfo {
        bitmaps: Vec::new(),
        label: None,
    };
    for e in bytes.chunks_exact(ENTRY) {
        match u8_at(e, 0) {
            0 => break,
            // 7.1.2 BitmapFlags bit 0: first (0) or second (1) allocation bitmap.
            TYPE_BITMAP => info
                .bitmaps
                .push((u8_at(e, 1) & 1, u32_at(e, 20), u64_at(e, 24))),
            TYPE_VOLUME_LABEL => {
                let n = usize::from(u8_at(e, 1)).min(11);
                let units: Vec<u16> = (0..n).map(|k| u16_at(e, 2 + 2 * k)).collect();
                info.label = Some(utf16(&units)).filter(|l| !l.is_empty());
            }
            _ => {}
        }
    }
    info
}

struct Job {
    runs: Vec<Run>,
    path: String,
    deleted: bool,
}

pub(crate) fn scan(disk: &Disk, boot: &Boot) -> Volume {
    let mut warnings = Vec::new();
    if boot.volume_len > disk.len() {
        warnings.push(format!(
            "the volume claims {} bytes but only {} are available",
            boot.volume_len,
            disk.len()
        ));
    }
    // Clusters past the end of the source cannot be read; leaving them out also bounds the
    // memory a corrupt boot sector can make us allocate.
    let mut boot = boot.clone();
    boot.cluster_count = available_clusters(
        disk,
        boot.heap_offset,
        boot.cluster_size,
        boot.cluster_count,
    );
    let boot = &boot;
    let next = read_fat(disk, boot, &mut warnings);
    let mut heap = Heap::new(
        disk,
        boot.heap_offset,
        boot.cluster_size,
        boot.cluster_count,
        next,
    );

    // The root directory has no Stream Extension: its chain is in the FAT (3.1.10).
    let root_max = heap.clusters_for(MAX_DIR_BYTES);
    let (root_runs, _, _) = heap.follow(boot.root_cluster, root_max);
    let root_bytes = heap.read_runs(disk, &root_runs, MAX_DIR_BYTES, &mut warnings);
    let info = parse_root_info(&root_bytes);

    let bitmap = info
        .bitmaps
        .iter()
        .find(|b| b.0 == boot.active)
        .or(info.bitmaps.first());
    match bitmap {
        Some(&(_, first, len)) => {
            let need = u64::from(boot.cluster_count).div_ceil(8);
            let n = heap.clusters_for(len.min(need));
            let (mut runs, total, _) = heap.follow(first, n);
            if total < n {
                warnings.push(
                    "the allocation bitmap's chain is broken; reading it as contiguous".into(),
                );
                runs = heap.contiguous(first, n);
            }
            let bytes = heap.read_runs(disk, &runs, need, &mut warnings);
            heap.allocation_from_bitmap(&bytes);
        }
        None => {
            warnings.push("no allocation bitmap; using the FAT for allocation state".into());
            heap.allocation_from_fat();
        }
    }

    let mut entries = Vec::new();
    let mut visited: HashSet<u32> = HashSet::new();
    let mut live: VecDeque<Job> = VecDeque::new();
    let mut deleted_dirs: VecDeque<Job> = VecDeque::new();
    live.push_back(Job {
        runs: root_runs,
        path: String::new(),
        deleted: false,
    });
    while let Some(job) = live.pop_front().or_else(|| deleted_dirs.pop_front()) {
        let runs = unvisited_prefix(&job.runs, &mut visited);
        if runs.is_empty() {
            continue;
        }
        let bytes = heap.read_runs(disk, &runs, MAX_DIR_BYTES, &mut warnings);
        for s in parse_dir(&bytes) {
            // Inside a deleted directory, accept only sets that verify: the clusters may
            // have been reused for anything.
            if job.deleted && !s.checksum_ok {
                continue;
            }
            let deleted = job.deleted || !s.in_use;
            let is_dir = s.attributes & ATTR_DIRECTORY != 0;
            let path = if job.path.is_empty() {
                s.name.clone()
            } else {
                format!("{}/{}", job.path, s.name)
            };
            let size = if s.allocation_possible { s.data_len } else { 0 };
            let p = heap.place(s.first_cluster, size, deleted, s.no_fat_chain, true);
            if is_dir && !p.runs.is_empty() && size <= MAX_DIR_BYTES {
                let q = if deleted {
                    &mut deleted_dirs
                } else {
                    &mut live
                };
                q.push_back(Job {
                    runs: p.runs.clone(),
                    path: path.clone(),
                    deleted,
                });
            }
            let confidence = if s.allocation_possible || s.data_len == 0 {
                p.confidence
            } else {
                Confidence::Low
            };
            entries.push(Entry {
                path,
                name: s.name,
                name_exact: s.name_exact,
                is_dir,
                deleted,
                size: s.data_len,
                valid_size: s.valid_len,
                first_cluster: s.first_cluster,
                extents: p.extents,
                layout: if size == 0 && s.no_fat_chain {
                    Layout::Contiguous
                } else {
                    p.layout
                },
                confidence,
                created: s.created,
                modified: s.modified,
                accessed: s.accessed,
            });
        }
    }

    Volume {
        kind: FsKind::Exfat,
        offset: disk.start(),
        len: disk.len(),
        cluster_size: heap.cluster_size,
        label: info.label,
        entries,
        free: heap.free_extents(),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_skips_its_own_field() {
        let mut a = [0u8; 32];
        a[0] = 0x85;
        let b = [0xC0u8; 32];
        let s1 = set_checksum(&[&a, &b], false);
        a[2] = 0x12;
        a[3] = 0x34;
        assert_eq!(set_checksum(&[&a, &b], false), s1);
        // A deleted copy verifies once InUse is restored.
        let mut da = a;
        da[0] &= !IN_USE;
        let mut db = b;
        db[0] &= !IN_USE;
        assert_eq!(set_checksum(&[&da, &db], true), s1);
    }
}
