//! Partition tables: finds where volumes lie on a disk.
//!
//! Reference: UEFI Specification 2.10, 5.2.1 "Legacy Master Boot Record (MBR)" (four
//! 16-byte partition records at offset 446, signature AA55h at 510; StartingLBA at +8,
//! SizeInLBA at +12, OSType at +4) and 5.3 "GUID Partition Table (GPT) Disk Layout"
//! (header at LBA 1 with signature "EFI PART"; PartitionEntryLBA at 72,
//! NumberOfPartitionEntries at 80, SizeOfPartitionEntry at 84; each entry holds
//! PartitionTypeGUID at 0, StartingLBA at 32 and EndingLBA at 40, inclusive).
//! Extended partitions (types 05h, 0Fh, 85h) chain Extended Boot Records whose first
//! record describes a logical partition relative to the EBR and whose second points to the
//! next EBR relative to the start of the extended partition (the de-facto MBR convention).

use std::collections::HashSet;

use blocksource::BlockSource;

use crate::{
    Error,
    disk::Disk,
    exfat, fat,
    le::{u8_at, u32_at, u64_at},
};

/// A region of the disk that may hold a volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub offset: u64,
    pub len: u64,
    pub origin: Origin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// No partition table: the whole disk is one volume (common on small SD cards).
    WholeDisk,
    /// An MBR partition (`index` 0-3 for primary partitions, 4 and up for logical ones).
    Mbr {
        index: u32,
        os_type: u8,
    },
    Gpt {
        index: u32,
        type_guid: [u8; 16],
    },
}

const MAX_EBRS: usize = 128;
const MAX_GPT_ENTRIES: u32 = 1024;

/// Lists the partitions on `src`. Without a recognizable partition table, the whole disk
/// is returned as one partition so the file system probe can decide.
pub fn find_partitions(src: &dyn BlockSource) -> Result<Vec<Partition>, Error> {
    let whole = vec![Partition {
        offset: 0,
        len: src.len(),
        origin: Origin::WholeDisk,
    }];
    let disk = Disk::new(src, 0, src.len());
    let Ok(sector0) = disk.read(0, 512) else {
        return Ok(whole);
    };
    // A volume boot record also ends in AA55h, so check for one first.
    if exfat::is_boot_sector(&sector0) || fat::is_boot_sector(&sector0) {
        return Ok(whole);
    }
    if sector0.get(510..512) != Some([0x55, 0xAA].as_slice()) {
        return Ok(whole);
    }
    let lba = u64::from(src.sector_size().max(512));
    let records = mbr_records(&sector0);
    let mut parts = Vec::new();
    if records.iter().any(|r| r.os_type == 0xEE) {
        // Protective MBR: the GPT is authoritative. Try the source's sector size, then the
        // common ones.
        for size in [lba, 512, 4096] {
            if let Some(p) = gpt(&disk, size) {
                parts = p;
                break;
            }
        }
    } else {
        for (i, r) in records.iter().enumerate() {
            if r.os_type == 0 || r.size == 0 {
                continue;
            }
            if matches!(r.os_type, 0x05 | 0x0F | 0x85) {
                logical(&disk, lba, r.start, &mut parts);
            } else {
                push(
                    &mut parts,
                    &disk,
                    lba,
                    r.start,
                    r.size,
                    Origin::Mbr {
                        index: u32::try_from(i).unwrap_or(u32::MAX),
                        os_type: r.os_type,
                    },
                );
            }
        }
    }
    if parts.is_empty() {
        return Ok(whole);
    }
    Ok(parts)
}

struct Record {
    os_type: u8,
    start: u64,
    size: u64,
}

fn mbr_records(sector: &[u8]) -> Vec<Record> {
    (0..4)
        .map(|i| {
            let at = 446 + 16 * i;
            Record {
                os_type: u8_at(sector, at + 4),
                start: u64::from(u32_at(sector, at + 8)),
                size: u64::from(u32_at(sector, at + 12)),
            }
        })
        .collect()
}

fn push(parts: &mut Vec<Partition>, disk: &Disk, lba: u64, start: u64, size: u64, origin: Origin) {
    let offset = start.saturating_mul(lba);
    if start == 0 || offset >= disk.len() {
        return;
    }
    let len = size.saturating_mul(lba).min(disk.len() - offset);
    parts.push(Partition {
        offset,
        len,
        origin,
    });
}

/// Walks the chain of Extended Boot Records.
fn logical(disk: &Disk, lba: u64, ext_start: u64, parts: &mut Vec<Partition>) {
    let mut seen = HashSet::new();
    let mut ebr = ext_start;
    let mut index = 4u32;
    while seen.len() < MAX_EBRS && seen.insert(ebr) {
        let Ok(sector) = disk.read(ebr.saturating_mul(lba), 512) else {
            return;
        };
        if sector.get(510..512) != Some([0x55, 0xAA].as_slice()) {
            return;
        }
        let r = mbr_records(&sector);
        if let Some(l) = r.first().filter(|l| l.os_type != 0 && l.size != 0) {
            push(
                parts,
                disk,
                lba,
                ebr.saturating_add(l.start),
                l.size,
                Origin::Mbr {
                    index,
                    os_type: l.os_type,
                },
            );
            index += 1;
        }
        match r.get(1) {
            Some(n) if n.os_type != 0 && n.start != 0 => ebr = ext_start.saturating_add(n.start),
            _ => return,
        }
    }
}

fn gpt(disk: &Disk, lba: u64) -> Option<Vec<Partition>> {
    let header = disk.read(lba, 92).ok()?;
    if header.get(0..8)? != b"EFI PART" {
        return None;
    }
    let entries_lba = u64_at(&header, 72);
    let count = u32_at(&header, 80).min(MAX_GPT_ENTRIES);
    let entry_size = u32_at(&header, 84);
    if !(128..=4096).contains(&entry_size) || !entry_size.is_multiple_of(8) {
        return None;
    }
    let table_len = usize::try_from(u64::from(count) * u64::from(entry_size)).ok()?;
    let table = disk.read(entries_lba.checked_mul(lba)?, table_len).ok()?;
    let mut parts = Vec::new();
    let step = usize::try_from(entry_size).ok()?;
    for (i, e) in table.chunks_exact(step).enumerate() {
        let type_guid = <[u8; 16]>::try_from(e.get(0..16)?).ok()?;
        if type_guid == [0; 16] {
            continue;
        }
        let first = u64_at(e, 32);
        let last = u64_at(e, 40);
        if last < first {
            continue;
        }
        push(
            &mut parts,
            disk,
            lba,
            first,
            last - first + 1,
            Origin::Gpt {
                index: u32::try_from(i).unwrap_or(u32::MAX),
                type_guid,
            },
        );
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    fn mbr(entries: &[(u8, u32, u32)]) -> Vec<u8> {
        let mut s = vec![0u8; 512];
        for (i, &(t, start, size)) in entries.iter().enumerate() {
            let at = 446 + 16 * i;
            s[at + 4] = t;
            s[at + 8..at + 12].copy_from_slice(&start.to_le_bytes());
            s[at + 12..at + 16].copy_from_slice(&size.to_le_bytes());
        }
        s[510] = 0x55;
        s[511] = 0xAA;
        s
    }

    #[test]
    fn primary_and_logical() {
        let mut d = vec![0u8; 512 * 400];
        d[..512].copy_from_slice(&mbr(&[(0x0C, 51, 100), (0x0F, 200, 150)]));
        // EBR at 200: logical at 200+10, next EBR at 200+100.
        d[200 * 512..201 * 512].copy_from_slice(&mbr(&[(0x07, 10, 20), (0x05, 100, 40)]));
        d[300 * 512..301 * 512].copy_from_slice(&mbr(&[(0x07, 5, 30)]));
        let p = find_partitions(&MemSource::new(d)).unwrap();
        let offs: Vec<_> = p.iter().map(|p| (p.offset / 512, p.len / 512)).collect();
        assert_eq!(offs, [(51, 100), (210, 20), (305, 30)]);
    }

    #[test]
    fn ebr_loop_terminates() {
        let mut d = vec![0u8; 512 * 400];
        d[..512].copy_from_slice(&mbr(&[(0x05, 200, 150)]));
        d[200 * 512..201 * 512].copy_from_slice(&mbr(&[(0x07, 1, 2), (0x05, 100, 1)]));
        // The second EBR points back to itself.
        d[300 * 512..301 * 512].copy_from_slice(&mbr(&[(0x07, 1, 2), (0x05, 100, 1)]));
        let p = find_partitions(&MemSource::new(d)).unwrap();
        assert_eq!(p.len(), 2);
    }

    #[test]
    fn no_table_means_whole_disk() {
        let p = find_partitions(&MemSource::new(vec![0u8; 4096])).unwrap();
        assert_eq!(p[0].origin, Origin::WholeDisk);
        let p = find_partitions(&MemSource::new(vec![0u8; 100])).unwrap();
        assert_eq!(p[0].len, 100);
    }

    #[test]
    fn gpt_partitions() {
        let mut d = vec![0u8; 512 * 200];
        d[..512].copy_from_slice(&mbr(&[(0xEE, 1, 199)]));
        let h = 512;
        d[h..h + 8].copy_from_slice(b"EFI PART");
        d[h + 72..h + 80].copy_from_slice(&2u64.to_le_bytes());
        d[h + 80..h + 84].copy_from_slice(&4u32.to_le_bytes());
        d[h + 84..h + 88].copy_from_slice(&128u32.to_le_bytes());
        let e = 1024 + 128;
        d[e] = 0xAF;
        d[e + 32..e + 40].copy_from_slice(&40u64.to_le_bytes());
        d[e + 40..e + 48].copy_from_slice(&99u64.to_le_bytes());
        let p = find_partitions(&MemSource::new(d)).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].offset, p[0].len), (40 * 512, 60 * 512));
        assert!(matches!(p[0].origin, Origin::Gpt { index: 1, .. }));
    }
}
