//! The NTFS boot sector (BPB and extended BPB).
//!
//! Reference: Microsoft "NTFS overview" / Windows Server documentation, "Boot Sector"
//! (learn.microsoft.com: BPB and extended BPB field table: bytes per sector at 0x0B,
//! sectors per cluster at 0x0D, total sectors at 0x28, $MFT cluster at 0x30, $MFTMirr
//! cluster at 0x38, clusters per file record segment at 0x40, clusters per index buffer
//! at 0x44, end-of-sector marker 0xAA55 at 0x1FE); Brian Carrier, "File System Forensic
//! Analysis", chapter 13, "Boot Sector" (a size field below zero means 2^-n bytes, and the
//! backup boot sector lies in the last sector of the volume).

use crate::{
    Error,
    disk::Disk,
    le::{u8_at, u16_at, u64_at},
};

#[derive(Debug, Clone)]
pub(crate) struct Boot {
    pub(crate) cluster_size: u64,
    /// Bytes in the volume according to the boot sector.
    pub(crate) volume_len: u64,
    /// Clusters in the volume.
    pub(crate) clusters: u64,
    pub(crate) mft_lcn: u64,
    pub(crate) mftmirr_lcn: u64,
    /// Size of an MFT record (file record segment).
    pub(crate) record_size: u64,
}

/// Largest cluster size Windows formats (2 MiB, encoded as a negative power of two).
const MAX_CLUSTER: u64 = 2 << 20;

/// A size in clusters, or (if negative) 2^-n bytes.
fn clusters_or_shift(v: u8, cluster_size: u64) -> Option<u64> {
    #[allow(clippy::cast_possible_wrap)]
    let v = v as i8;
    if v > 0 {
        cluster_size.checked_mul(u64::try_from(v).ok()?)
    } else if v < 0 {
        1u64.checked_shl(u32::from(v.unsigned_abs()))
    } else {
        None
    }
}

/// Parses and validates an NTFS boot sector.
pub(crate) fn parse(b: &[u8]) -> Option<Boot> {
    if b.len() < 512 || b.get(3..11)? != b"NTFS    " || u16_at(b, 510) != 0xAA55 {
        return None;
    }
    let bps = u64::from(u16_at(b, 11));
    if !bps.is_power_of_two() || !(256..=4096).contains(&bps) {
        return None;
    }
    // Sectors per cluster: 1..=128 as is; larger clusters are written as 2^-n (F4h..).
    let spc_raw = u8_at(b, 13);
    let spc = if spc_raw <= 0x80 {
        u64::from(spc_raw)
    } else {
        1u64.checked_shl(256 - u32::from(spc_raw))?
    };
    if spc == 0 || !spc.is_power_of_two() {
        return None;
    }
    let cluster_size = bps.checked_mul(spc).filter(|&c| c <= MAX_CLUSTER)?;
    let total_sectors = u64_at(b, 0x28);
    let clusters = total_sectors / spc;
    let mft_lcn = u64_at(b, 0x30);
    let mftmirr_lcn = u64_at(b, 0x38);
    let record_size = clusters_or_shift(u8_at(b, 0x40), cluster_size)?;
    // Records are protected in 512-byte strides, so they are a whole number of them.
    if clusters == 0
        || mft_lcn >= clusters
        || mftmirr_lcn >= clusters
        || !record_size.is_power_of_two()
        || !(512..=65_536).contains(&record_size)
    {
        return None;
    }
    Some(Boot {
        cluster_size,
        volume_len: total_sectors.saturating_mul(bps),
        clusters,
        mft_lcn,
        mftmirr_lcn,
        record_size,
    })
}

pub(crate) fn is_boot_sector(sector: &[u8]) -> bool {
    parse(sector).is_some()
}

/// Looks for an NTFS boot sector. If the first sector names NTFS but does not validate,
/// tries the backup copy in the last sector of the volume.
pub(crate) fn probe(disk: &Disk) -> Result<Option<Boot>, Error> {
    let main = disk.read(0, 512)?;
    if let Some(b) = parse(&main) {
        return Ok(Some(b));
    }
    if main.get(3..11) != Some(b"NTFS    ".as_slice()) {
        return Ok(None);
    }
    for sector in [512u64, 4096] {
        let last = (disk.len() / sector).saturating_sub(1) * sector;
        if last == 0 {
            continue;
        }
        if let Some(b) = disk.read(last, 512).ok().and_then(|b| parse(&b)) {
            return Ok(Some(b));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector() -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[3..11].copy_from_slice(b"NTFS    ");
        b[11..13].copy_from_slice(&512u16.to_le_bytes());
        b[13] = 8;
        b[0x28..0x30].copy_from_slice(&131_071u64.to_le_bytes());
        b[0x30..0x38].copy_from_slice(&4u64.to_le_bytes());
        b[0x38..0x40].copy_from_slice(&2u64.to_le_bytes());
        b[0x40] = 0xF6; // 2^10 = 1024-byte records
        b[510] = 0x55;
        b[511] = 0xAA;
        b
    }

    #[test]
    fn parses_boot_sector() {
        let b = parse(&sector()).unwrap();
        assert_eq!(b.cluster_size, 4096);
        assert_eq!(b.record_size, 1024);
        assert_eq!(b.clusters, 131_071 / 8);
        assert_eq!(b.mft_lcn, 4);
    }

    #[test]
    fn large_clusters_and_record_in_clusters() {
        let mut s = sector();
        s[13] = 0xF4; // 2^12 sectors = 2 MiB clusters
        s[0x28..0x30].copy_from_slice(&(1u64 << 30).to_le_bytes());
        s[0x40] = 0xF6;
        assert_eq!(parse(&s).unwrap().cluster_size, 2 << 20);
        s[13] = 0xF3; // 4 MiB: too large
        assert!(parse(&s).is_none());
        let mut s = sector();
        s[13] = 1;
        s[0x40] = 2; // two 512-byte clusters
        assert_eq!(parse(&s).unwrap().record_size, 1024);
    }

    #[test]
    fn rejects_bad_fields() {
        for (at, v) in [(13, 3u8), (0x40, 0), (0x40, 0xF8), (12, 0), (510, 0)] {
            let mut s = sector();
            s[at] = v;
            assert!(parse(&s).is_none(), "{at:#x}={v:#x}");
        }
        let mut s = sector();
        s[0x30..0x38].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse(&s).is_none());
    }
}
