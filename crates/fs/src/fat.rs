//! FAT12/16/32.
//!
//! Reference: Microsoft "FAT32 File System Specification" (fatgen103, version 1.03),
//! sections "Boot Sector and BPB", "FAT Data Structure", "FAT Type Determination",
//! "FAT32 FSInfo Sector Structure and Backup Boot Sector", "FAT Directory Structure" and
//! "FAT Long Directory Entries".
//!
//! Deleting a file marks its directory entries free (first byte `E5h`, which overwrites the
//! first character of the short name) and zeroes its FAT chain. The name is recovered from
//! the long-name entries when they survive, otherwise its first character is guessed; the
//! data is assumed to be contiguous from the first cluster, which the entry still records.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::{
    Confidence, Entry, Error, FsKind, Layout, Volume,
    disk::Disk,
    heap::{BAD, END, FREE, Heap, INVALID, Stop, available_clusters, unvisited_prefix},
    le::{sanitize_name, u8_at, u16_at, u32_at},
    time::Timestamp,
};

/// Largest directory: 65,536 entries of 32 bytes (fatgen103 "FAT Directory Structure").
const MAX_DIR_BYTES: u64 = 65_536 * 32;
const DIR_ENTRY: usize = 32;

const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
/// ATTR_LONG_NAME and the mask it is tested with (fatgen103 "FAT Long Directory Entries").
const ATTR_LONG_NAME: u8 = 0x0F;
const ATTR_LONG_NAME_MASK: u8 = 0x3F;
const DELETED: u8 = 0xE5;

#[derive(Debug, Clone)]
pub(crate) struct Bpb {
    kind: FsKind,
    bytes_per_sector: u64,
    cluster_size: u64,
    reserved_sectors: u64,
    num_fats: u64,
    root_entries: u64,
    fat_sectors: u64,
    total_sectors: u64,
    cluster_count: u32,
    /// FAT32: first cluster of the root directory.
    root_cluster: u32,
    /// FAT32 BPB_ExtFlags: the FAT in use when mirroring is disabled.
    active_fat: u64,
    label: Option<String>,
}

impl Bpb {
    fn first_data_sector(&self) -> u64 {
        self.reserved_sectors + self.num_fats * self.fat_sectors + self.root_dir_sectors()
    }

    fn root_dir_sectors(&self) -> u64 {
        (self.root_entries * 32).div_ceil(self.bytes_per_sector)
    }
}

/// Parses and validates a BPB.
///
/// Reference: fatgen103 "Boot Sector and BPB" (field offsets and legal values) and "FAT
/// Type Determination" (the type follows from the count of clusters alone).
fn parse_bpb(b: &[u8]) -> Option<Bpb> {
    if b.len() < 512 {
        return None;
    }
    let jump_ok = matches!(b.first(), Some(0xE9)) || matches!(b.first(), Some(0xEB));
    let bps = u64::from(u16_at(b, 11));
    let spc = u64::from(u8_at(b, 13));
    let rsvd = u64::from(u16_at(b, 14));
    let nfats = u64::from(u8_at(b, 16));
    let root_entries = u64::from(u16_at(b, 17));
    let tot16 = u64::from(u16_at(b, 19));
    let media = u8_at(b, 21);
    let fatsz16 = u64::from(u16_at(b, 22));
    let tot32 = u64::from(u32_at(b, 32));
    let fatsz32 = u64::from(u32_at(b, 36));
    if !jump_ok
        || !matches!(bps, 512 | 1024 | 2048 | 4096)
        || !spc.is_power_of_two()
        || spc > 128
        || rsvd == 0
        || !(1..=4).contains(&nfats)
        || !(media == 0xF0 || media >= 0xF8)
    {
        return None;
    }
    let fat_sectors = if fatsz16 != 0 { fatsz16 } else { fatsz32 };
    let total_sectors = if tot16 != 0 { tot16 } else { tot32 };
    if fat_sectors == 0 || total_sectors == 0 {
        return None;
    }
    let mut bpb = Bpb {
        kind: FsKind::Fat12,
        bytes_per_sector: bps,
        cluster_size: bps * spc,
        reserved_sectors: rsvd,
        num_fats: nfats,
        root_entries,
        fat_sectors,
        total_sectors,
        cluster_count: 0,
        root_cluster: 0,
        active_fat: 0,
        label: None,
    };
    let data_sectors = total_sectors.checked_sub(bpb.first_data_sector())?;
    let count = data_sectors / spc;
    bpb.kind = if count < 4085 {
        FsKind::Fat12
    } else if count < 65525 {
        FsKind::Fat16
    } else {
        FsKind::Fat32
    };
    // Clusters 0FFFFFF7h and up are reserved even on FAT32.
    bpb.cluster_count = u32::try_from(count)
        .ok()
        .filter(|&c| c > 0 && c < 0x0FFF_FFF5)?;
    let label_at = if bpb.kind == FsKind::Fat32 {
        if root_entries != 0 || fatsz16 != 0 {
            return None;
        }
        bpb.root_cluster = u32_at(b, 44);
        let ext_flags = u16_at(b, 40);
        if ext_flags & 0x80 != 0 {
            bpb.active_fat = u64::from(ext_flags & 0x0F).min(nfats - 1);
        }
        71
    } else {
        if root_entries == 0 {
            return None;
        }
        43
    };
    // The FAT needs room for an entry per cluster.
    let bits = match bpb.kind {
        FsKind::Fat12 => 12,
        FsKind::Fat16 => 16,
        _ => 32,
    };
    if (count + 2) * bits > fat_sectors * bps * 8 {
        return None;
    }
    // BS_BootSig 29h means BS_VolLab and BS_FilSysType are present.
    if u8_at(b, label_at - 5) == 0x29 {
        bpb.label = b
            .get(label_at..label_at + 11)
            .map(|l| String::from_utf8_lossy(l).trim_end().to_owned())
            .filter(|l| !l.is_empty() && l != "NO NAME");
    }
    Some(bpb)
}

pub(crate) fn is_boot_sector(sector: &[u8]) -> bool {
    parse_bpb(sector).is_some()
}

/// Looks for a FAT boot sector, falling back to the FAT32 backup boot sector (sector 6,
/// fatgen103 "FAT32 FSInfo Sector Structure and Backup Boot Sector").
pub(crate) fn probe(disk: &Disk) -> Result<Option<Bpb>, Error> {
    let main = disk.read(0, 512)?;
    if let Some(b) = parse_bpb(&main) {
        return Ok(Some(b));
    }
    Ok(disk
        .read(6 * 512, 512)
        .ok()
        .and_then(|b| parse_bpb(&b))
        .filter(|b| b.kind == FsKind::Fat32))
}

/// Reads the FAT into normalized entries.
///
/// Reference: fatgen103 "FAT Data Structure" (FAT12 entries are 12 bits packed at byte
/// N + N/2; FAT32 entries use the low 28 bits; end of chain is >= FF8h / FFF8h /
/// 0FFFFFF8h and bad cluster is FF7h / FFF7h / 0FFFFFF7h).
fn read_fat(disk: &Disk, bpb: &Bpb, warnings: &mut Vec<String>) -> Vec<u32> {
    let entries = u64::from(bpb.cluster_count) + 2;
    let bytes = match bpb.kind {
        FsKind::Fat12 => entries + entries / 2 + 1,
        FsKind::Fat16 => entries * 2,
        _ => entries * 4,
    }
    .min(bpb.fat_sectors * bpb.bytes_per_sector);
    let offset = (bpb.reserved_sectors + bpb.active_fat * bpb.fat_sectors) * bpb.bytes_per_sector;
    let raw = disk.read_lossy(offset, usize::try_from(bytes).unwrap_or(0), warnings);
    let (eoc, bad) = match bpb.kind {
        FsKind::Fat12 => (0xFF8, 0xFF7),
        FsKind::Fat16 => (0xFFF8, 0xFFF7),
        _ => (0x0FFF_FFF8, 0x0FFF_FFF7),
    };
    let end = bpb.cluster_count.saturating_add(2);
    (0..end)
        .map(|c| {
            let i = c as usize;
            let v = match bpb.kind {
                FsKind::Fat12 => {
                    let w = u32::from(u16_at(&raw, i + i / 2));
                    if c & 1 == 1 { w >> 4 } else { w & 0xFFF }
                }
                FsKind::Fat16 => u32::from(u16_at(&raw, i * 2)),
                _ => u32_at(&raw, i * 4) & 0x0FFF_FFFF,
            };
            match v {
                _ if c < 2 => INVALID,
                0 => FREE,
                v if v >= eoc => END,
                v if v == bad => BAD,
                v if v >= 2 && v < end => v,
                _ => INVALID,
            }
        })
        .collect()
}

/// A parsed short directory entry with its long name, if any.
#[derive(Debug, Clone)]
struct Raw {
    name: String,
    name_exact: bool,
    short: [u8; 11],
    /// The first byte of the short name was lost (deleted entry without a long name).
    first_lost: bool,
    lowercase: u8,
    attr: u8,
    deleted: bool,
    first_cluster: u32,
    size: u32,
    created: Option<Timestamp>,
    modified: Option<Timestamp>,
    accessed: Option<Timestamp>,
}

/// Parses the entries of one directory. Returns the entries (without `.` and `..`) and
/// the volume label entry, if any.
///
/// Reference: fatgen103 "FAT Directory Structure" (DIR_Name[0] = E5h: free; 00h: free and
/// no allocated entries follow; 05h stands for E5h; field offsets) and "FAT Long Directory
/// Entries" (long-name entries precede their short entry in reverse order, each carrying the
/// short name's checksum; LDIR_Ord has 40h on the last one).
fn parse_dir(bytes: &[u8], kind: FsKind) -> (Vec<Raw>, Option<String>) {
    let mut out = Vec::new();
    let mut label = None;
    let mut pending: Vec<&[u8]> = Vec::new();
    for e in bytes.chunks_exact(DIR_ENTRY) {
        let b0 = u8_at(e, 0);
        if b0 == 0x00 {
            break;
        }
        let attr = u8_at(e, 11);
        if attr & ATTR_LONG_NAME_MASK == ATTR_LONG_NAME {
            pending.push(e);
            continue;
        }
        let long = std::mem::take(&mut pending);
        let deleted = b0 == DELETED;
        let Some(mut short) = e.get(0..11).and_then(|s| <[u8; 11]>::try_from(s).ok()) else {
            continue;
        };
        if attr & ATTR_VOLUME_ID != 0 && attr & ATTR_DIRECTORY == 0 {
            if !deleted {
                label = Some(String::from_utf8_lossy(&short).trim_end().to_owned());
            }
            continue;
        }
        if short == *b".          " || short == *b"..         " {
            continue;
        }
        if short[0] == 0x05 {
            short[0] = 0xE5;
        }
        let lname = long_name(&long, &mut short, deleted);
        let first_lost = deleted && lname.is_none();
        let lowercase = u8_at(e, 12);
        let (name, name_exact) = match lname {
            Some(n) => sanitize_name(n),
            None => short_name(&short, lowercase, first_lost),
        };
        let hi = if kind == FsKind::Fat32 {
            u32::from(u16_at(e, 20)) << 16
        } else {
            0
        };
        out.push(Raw {
            name,
            name_exact,
            short,
            first_lost,
            lowercase,
            attr,
            deleted,
            first_cluster: hi | u32::from(u16_at(e, 26)),
            size: u32_at(e, 28),
            created: Timestamp::from_dos(u16_at(e, 16), u16_at(e, 14))
                .map(|t| t.with_10ms(u8_at(e, 13))),
            modified: Timestamp::from_dos(u16_at(e, 24), u16_at(e, 22)),
            accessed: Timestamp::from_dos(u16_at(e, 18), 0),
        });
    }
    guess_lost_first_bytes(&mut out);
    (out, label)
}

/// Reference: fatgen103 "FAT Long Directory Entries", ChkSum().
fn short_checksum(short: &[u8; 11]) -> u8 {
    short.iter().fold(0u8, |sum, &b| {
        ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(b)
    })
}

/// Assembles the long name stored in `long` (in on-disk order) for the short entry `short`.
/// For a deleted entry the short name's first byte is lost; it is recovered from the
/// checksum and written back into `short`.
fn long_name(long: &[&[u8]], short: &mut [u8; 11], deleted: bool) -> Option<String> {
    if long.is_empty() || long.len() > 20 {
        return None;
    }
    let sum = u8_at(long.first()?, 13);
    if long
        .iter()
        .any(|l| u8_at(l, 13) != sum || u8_at(l, 12) != 0)
    {
        return None;
    }
    if deleted {
        if long.iter().any(|l| u8_at(l, 0) != DELETED) {
            return None;
        }
    } else {
        // Ordinals run N|40h, N-1, ..., 1.
        let n = long.len();
        for (i, l) in long.iter().enumerate() {
            let want = u8::try_from(n - i).unwrap_or(0) | if i == 0 { 0x40 } else { 0 };
            if u8_at(l, 0) != want {
                return None;
            }
        }
    }
    let mut units: Vec<u16> = Vec::new();
    for l in long.iter().rev() {
        for at in (1..11)
            .step_by(2)
            .chain((14..26).step_by(2))
            .chain((28..32).step_by(2))
        {
            units.push(u16_at(l, at));
        }
    }
    // The name ends at 0000h (unless it fills the last entry exactly); FFFFh pads the rest.
    let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    let tail = units.get(len..).unwrap_or_default();
    if len == 0
        || units.len() - len > 13
        || tail.iter().skip(1).any(|&u| u != 0xFFFF)
        || units.get(..len)?.contains(&0xFFFF)
    {
        return None;
    }
    let name = crate::le::utf16(units.get(..len)?);
    if !deleted {
        return (short_checksum(short) == sum).then_some(name);
    }
    // Try the likeliest first byte (the long name's first character, upper-cased), then
    // any byte.
    let preferred = name
        .chars()
        .next()
        .filter(char::is_ascii)
        .map(|c| c.to_ascii_uppercase() as u8);
    for b in preferred.into_iter().chain(0x20..=0xFF) {
        short[0] = b;
        if short_checksum(short) == sum {
            return Some(name);
        }
    }
    short[0] = DELETED;
    None
}

/// Formats an 8.3 name. Bit 3/4 of DIR_NTRes (lower-case base/extension) follow the
/// Windows NT convention; fatgen103 only marks the byte as reserved for Windows NT.
/// Bytes above 7Fh are in an unknown OEM code page and are mapped as Latin-1.
fn short_name(short: &[u8; 11], lowercase: u8, first_lost: bool) -> (String, bool) {
    let conv = |bytes: &[u8], lower: bool| -> (String, bool) {
        let s = String::from_utf8_lossy(bytes);
        let trimmed = bytes.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
        let exact = bytes.iter().all(u8::is_ascii) && s.len() == bytes.len();
        let mut out: String = bytes
            .get(..trimmed)
            .unwrap_or_default()
            .iter()
            .map(|&b| char::from(b))
            .collect();
        if lower {
            out = out.to_ascii_lowercase();
        }
        (out, exact)
    };
    let (base, e1) = conv(short.get(..8).unwrap_or_default(), lowercase & 0x08 != 0);
    let (ext, e2) = conv(short.get(8..).unwrap_or_default(), lowercase & 0x10 != 0);
    let name = if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    };
    let (name, clean) = sanitize_name(name);
    (name, e1 && e2 && clean && !first_lost)
}

/// Guesses the lost first byte of deleted short names from live siblings with the same
/// pattern (cameras name files like `IMG_0001.JPG`, so `?MG_0002.JPG` was most likely
/// `IMG_0002.JPG`). Without a match the byte becomes `_`.
fn guess_lost_first_bytes(entries: &mut [Raw]) {
    let mut votes: HashMap<([u8; 3], [u8; 3]), HashMap<u8, usize>> = HashMap::new();
    let key = |s: &[u8; 11]| ([s[1], s[2], s[3]], [s[8], s[9], s[10]]);
    for e in entries.iter().filter(|e| !e.first_lost) {
        *votes
            .entry(key(&e.short))
            .or_default()
            .entry(e.short[0])
            .or_default() += 1;
    }
    for e in entries.iter_mut().filter(|e| e.first_lost) {
        let guess = votes
            .get(&key(&e.short))
            .and_then(|v| v.iter().max_by_key(|&(b, n)| (*n, std::cmp::Reverse(*b))))
            .map_or(b'_', |(&b, _)| b);
        e.short[0] = guess;
        e.name = short_name(&e.short, e.lowercase, true).0;
    }
}

enum DirLoc {
    /// FAT12/16 root directory region.
    FixedRoot,
    Cluster(u32),
}

struct Job {
    loc: DirLoc,
    path: String,
    deleted: bool,
}

pub(crate) fn scan(disk: &Disk, bpb: &Bpb) -> Volume {
    let mut warnings = Vec::new();
    if bpb.total_sectors * bpb.bytes_per_sector > disk.len() {
        warnings.push(format!(
            "the volume claims {} bytes but only {} are available",
            bpb.total_sectors * bpb.bytes_per_sector,
            disk.len()
        ));
    }
    // Clusters past the end of the source cannot be read; leaving them out also bounds the
    // memory a corrupt BPB can make us allocate.
    let mut bpb = bpb.clone();
    bpb.cluster_count = available_clusters(
        disk,
        bpb.first_data_sector() * bpb.bytes_per_sector,
        bpb.cluster_size,
        bpb.cluster_count,
    );
    let bpb = &bpb;
    let next = read_fat(disk, bpb, &mut warnings);
    let mut heap = Heap::new(
        disk,
        bpb.first_data_sector() * bpb.bytes_per_sector,
        bpb.cluster_size,
        bpb.cluster_count,
        next,
    );
    heap.allocation_from_fat();

    let mut entries = Vec::new();
    let mut label = bpb.label.clone();
    let mut visited: HashSet<u32> = HashSet::new();
    let mut live: VecDeque<Job> = VecDeque::new();
    // Deleted directories are walked after all live ones, so that a stale directory
    // overlapping live clusters cannot hide them.
    let mut deleted_dirs: VecDeque<Job> = VecDeque::new();
    live.push_back(Job {
        loc: if bpb.kind == FsKind::Fat32 {
            DirLoc::Cluster(bpb.root_cluster)
        } else {
            DirLoc::FixedRoot
        },
        path: String::new(),
        deleted: false,
    });
    let max_dir_clusters = MAX_DIR_BYTES.div_ceil(heap.cluster_size).max(1);
    let mut is_root = true;
    while let Some(job) = live.pop_front().or_else(|| deleted_dirs.pop_front()) {
        let bytes = match job.loc {
            DirLoc::FixedRoot => {
                let at =
                    (bpb.reserved_sectors + bpb.num_fats * bpb.fat_sectors) * bpb.bytes_per_sector;
                let len = usize::try_from(bpb.root_entries * 32).unwrap_or(0);
                disk.read_lossy(at, len, &mut warnings)
            }
            DirLoc::Cluster(c) if !job.deleted => {
                if visited.contains(&c) {
                    continue;
                }
                let (runs, _, stop) = heap.follow(c, max_dir_clusters);
                if stop == Stop::Broken {
                    warnings.push(format!(
                        "directory /{} has a broken cluster chain",
                        job.path
                    ));
                }
                let runs = unvisited_prefix(&runs, &mut visited);
                heap.read_runs(disk, &runs, MAX_DIR_BYTES, &mut warnings)
            }
            DirLoc::Cluster(c) => read_deleted_dir(disk, &heap, c, &mut visited, &mut warnings),
        };
        let (raws, root_label) = parse_dir(&bytes, bpb.kind);
        if is_root {
            label = root_label.filter(|l| !l.is_empty()).or(label);
            is_root = false;
        }
        for r in raws {
            let deleted = job.deleted || r.deleted;
            let path = if job.path.is_empty() {
                r.name.clone()
            } else {
                format!("{}/{}", job.path, r.name)
            };
            let is_dir = r.attr & ATTR_DIRECTORY != 0;
            let (extents, layout, confidence) = if is_dir {
                if heap.in_heap(r.first_cluster) {
                    let q = if deleted {
                        &mut deleted_dirs
                    } else {
                        &mut live
                    };
                    q.push_back(Job {
                        loc: DirLoc::Cluster(r.first_cluster),
                        path: path.clone(),
                        deleted,
                    });
                }
                // FAT records no size for directories.
                let layout = if deleted {
                    Layout::Assumed
                } else {
                    Layout::Chain
                };
                (Vec::new(), layout, Confidence::High)
            } else {
                let p = heap.place(r.first_cluster, u64::from(r.size), deleted, false, false);
                (p.extents, p.layout, p.confidence)
            };
            entries.push(Entry {
                path,
                name: r.name,
                name_exact: r.name_exact,
                is_dir,
                deleted,
                size: if is_dir { 0 } else { u64::from(r.size) },
                valid_size: if is_dir { 0 } else { u64::from(r.size) },
                first_cluster: r.first_cluster,
                extents,
                holes: Vec::new(),
                resident: None,
                record: None,
                layout,
                confidence,
                created: r.created,
                modified: r.modified,
                accessed: r.accessed,
            });
        }
    }

    Volume {
        kind: bpb.kind,
        offset: disk.start(),
        len: disk.len(),
        cluster_size: heap.cluster_size,
        label,
        entries,
        free: heap.free_extents(),
        warnings,
    }
}

/// Reads a deleted directory. Its chain is gone, so it is read cluster by cluster from the
/// first one while the clusters stay free and the end-of-directory mark has not been seen.
/// The first cluster must start with the `.` and `..` entries every subdirectory has
/// (fatgen103 "FAT Directory Structure"), with `.` pointing at the directory itself;
/// otherwise the cluster has been reused and nothing is returned.
fn read_deleted_dir(
    disk: &Disk,
    heap: &Heap,
    first: u32,
    visited: &mut HashSet<u32>,
    warnings: &mut Vec<String>,
) -> Vec<u8> {
    if visited.contains(&first) || !heap.in_heap(first) {
        return Vec::new();
    }
    let cs = heap.cluster_size;
    let Ok(len) = usize::try_from(cs) else {
        return Vec::new();
    };
    let head = disk.read_lossy(heap.rel_offset(first), len, warnings);
    let dot = head.get(..32).unwrap_or_default();
    let dotdot = head.get(32..64).unwrap_or_default();
    let dot_cluster = u32::from(u16_at(dot, 26)) | (u32::from(u16_at(dot, 20)) << 16);
    let dot_ok = dot.get(..11) == Some(b".          ".as_slice())
        && u8_at(dot, 11) & ATTR_DIRECTORY != 0
        && (dot_cluster == first || dot_cluster & 0xFFFF == first & 0xFFFF)
        && dotdot.get(..11) == Some(b"..         ".as_slice());
    if !dot_ok {
        return Vec::new();
    }
    visited.insert(first);
    let ends = |b: &[u8]| b.chunks_exact(DIR_ENTRY).any(|e| u8_at(e, 0) == 0);
    let mut bytes = head;
    let mut c = first;
    while !ends(&bytes) && (bytes.len() as u64) < MAX_DIR_BYTES {
        c = c.saturating_add(1);
        if !heap.in_heap(c) || heap.is_allocated(c) || !visited.insert(c) {
            break;
        }
        bytes.extend(disk.read_lossy(heap.rel_offset(c), len, warnings));
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_matches_spec_example() {
        // Any fixed value works as a regression check; this one is computed by hand from
        // the ChkSum() definition.
        let mut sum = 0u8;
        for &b in b"FOO     BAR" {
            sum = (if sum & 1 != 0 { 0x80u8 } else { 0 })
                .wrapping_add(sum >> 1)
                .wrapping_add(b);
        }
        assert_eq!(short_checksum(b"FOO     BAR"), sum);
    }

    #[test]
    fn short_names() {
        assert_eq!(
            short_name(b"IMG_0001JPG", 0, false),
            ("IMG_0001.JPG".into(), true)
        );
        assert_eq!(
            short_name(b"README     ", 0, false),
            ("README".into(), true)
        );
        assert_eq!(
            short_name(b"IMG_0001JPG", 0x18, false),
            ("img_0001.jpg".into(), true)
        );
    }
}
