//! A builder for small NTFS images with hand-made MFT records, written from the published
//! structure descriptions (Microsoft's NTFS documentation and Carrier, "File System
//! Forensic Analysis", ch. 13) independently of the parser under test.
//!
//! Layout: 4 KiB clusters, 1 KiB records, 8 MiB. The MFT is itself fragmented (records
//! 0-31 at clusters 4-11, records 32-63 at clusters 200-207), $MFTMirr is at cluster 2 and
//! the $Bitmap data at cluster 20.

#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

use super::{put16, put32, put64};

pub const CLUSTER: usize = 4096;
pub const RECORD: usize = 1024;
pub const CLUSTERS: u64 = 2048;
pub const MFT_RUNS: [(u64, u64); 2] = [(4, 8), (200, 8)];
pub const MFT_RECORDS: u64 = 64;
pub const MIRR_LCN: u64 = 2;
pub const BITMAP_LCN: u64 = 20;
pub const ROOT: u64 = 5;

pub const SI: u32 = 0x10;
pub const ATTR_LIST: u32 = 0x20;
pub const FILE_NAME: u32 = 0x30;
pub const VOLUME_NAME: u32 = 0x60;
pub const DATA: u32 = 0x80;

pub const WIN32: u8 = 1;
pub const DOS: u8 = 2;

/// 2024-05-06 07:08:09 UTC as a FILETIME.
pub const TIME: u64 = (1_714_979_289 + 11_644_473_600) * 10_000_000;

pub struct NtfsImage {
    pub data: Vec<u8>,
    bitmap: Vec<u8>,
}

/// A data run: `len` clusters at `lcn`, or sparse.
pub type Run = (u64, Option<u64>);

pub fn reference(record: u64, seq: u16) -> u64 {
    record | (u64::from(seq) << 48)
}

impl NtfsImage {
    pub fn new() -> Self {
        let mut img = Self {
            data: vec![0; CLUSTERS as usize * CLUSTER],
            bitmap: vec![0; (CLUSTERS / 8) as usize],
        };
        let b = &mut img.data;
        b[0..3].copy_from_slice(&[0xEB, 0x52, 0x90]);
        b[3..11].copy_from_slice(b"NTFS    ");
        put16(b, 11, 512);
        b[13] = 8;
        b[21] = 0xF8;
        // The last sector holds the backup boot sector and is not part of the count.
        put64(b, 0x28, CLUSTERS * 8 - 1);
        put64(b, 0x30, MFT_RUNS[0].0);
        put64(b, 0x38, MIRR_LCN);
        b[0x40] = 0xF6; // 2^10-byte records
        b[0x44] = 1;
        b[510] = 0x55;
        b[511] = 0xAA;
        img.allocate(0, 4);
        for (lcn, len) in MFT_RUNS {
            img.allocate(lcn, len);
        }
        img.allocate(BITMAP_LCN, 1);
        img.allocate(CLUSTERS - 1, 1);

        let mft_size = MFT_RECORDS * RECORD as u64;
        img.record(
            0,
            1,
            true,
            false,
            None,
            &[
                std_info(TIME),
                file_name(ROOT, 5, "$MFT", WIN32),
                non_resident(DATA, 0, &MFT_RUNS.map(|(l, n)| (n, Some(l))), mft_size, 0),
            ],
        );
        img.record(
            1,
            1,
            true,
            false,
            None,
            &[file_name(ROOT, 5, "$MFTMirr", WIN32)],
        );
        let label: Vec<u8> = "HAKK".encode_utf16().flat_map(u16::to_le_bytes).collect();
        img.record(
            3,
            3,
            true,
            false,
            None,
            &[
                file_name(ROOT, 5, "$Volume", WIN32),
                resident(VOLUME_NAME, &label),
            ],
        );
        img.record(5, 5, true, true, None, &[file_name(ROOT, 5, ".", WIN32)]);
        img.record(
            6,
            6,
            true,
            false,
            None,
            &[
                file_name(ROOT, 5, "$Bitmap", WIN32),
                non_resident(DATA, 0, &[(1, Some(BITMAP_LCN))], CLUSTERS / 8, 0),
            ],
        );
        img.record(
            11,
            11,
            true,
            true,
            None,
            &[file_name(ROOT, 5, "$Extend", WIN32)],
        );
        img
    }

    /// Byte offset of record `n` in the image.
    pub fn record_offset(n: u64) -> usize {
        let per_cluster = (CLUSTER / RECORD) as u64;
        let mut first = 0;
        for (lcn, len) in MFT_RUNS {
            let count = len * per_cluster;
            if n < first + count {
                let k = n - first;
                return (lcn as usize) * CLUSTER + k as usize * RECORD;
            }
            first += count;
        }
        panic!("record {n} is outside the MFT");
    }

    /// Writes record `n`, protected by an update sequence array.
    pub fn record(
        &mut self,
        n: u64,
        seq: u16,
        in_use: bool,
        dir: bool,
        base: Option<u64>,
        attrs: &[Vec<u8>],
    ) {
        let mut r = vec![0u8; RECORD];
        r[0..4].copy_from_slice(b"FILE");
        put16(&mut r, 4, 0x30);
        put16(&mut r, 6, (RECORD / 512 + 1) as u16);
        put16(&mut r, 0x10, seq);
        put16(&mut r, 0x12, 1);
        put16(&mut r, 0x14, 0x38);
        put16(&mut r, 0x16, u16::from(in_use) | if dir { 2 } else { 0 });
        put64(&mut r, 0x20, base.unwrap_or(0));
        put32(&mut r, 0x2C, n as u32);
        let mut at = 0x38;
        for a in attrs {
            r[at..at + a.len()].copy_from_slice(a);
            at += a.len();
        }
        put32(&mut r, at, 0xFFFF_FFFF);
        put32(&mut r, 0x18, (at + 8) as u32);
        put32(&mut r, 0x1C, RECORD as u32);
        let usn = seq.wrapping_mul(3).wrapping_add(1);
        put16(&mut r, 0x30, usn);
        for i in 1..=RECORD / 512 {
            let end = i * 512 - 2;
            let saved = [r[end], r[end + 1]];
            r[0x30 + 2 * i..0x32 + 2 * i].copy_from_slice(&saved);
            put16(&mut r, end, usn);
        }
        let at = Self::record_offset(n);
        self.data[at..at + RECORD].copy_from_slice(&r);
    }

    pub fn allocate(&mut self, lcn: u64, len: u64) {
        for c in lcn..lcn + len {
            self.bitmap[(c / 8) as usize] |= 1 << (c % 8);
        }
    }

    pub fn write(&mut self, lcn: u64, bytes: &[u8]) {
        let at = lcn as usize * CLUSTER;
        self.data[at..at + bytes.len()].copy_from_slice(bytes);
    }

    pub fn cluster(lcn: u64) -> u64 {
        lcn * CLUSTER as u64
    }

    /// Writes the bitmap, $MFTMirr and the backup boot sector.
    pub fn finish(mut self) -> Vec<u8> {
        let at = BITMAP_LCN as usize * CLUSTER;
        self.data[at..at + self.bitmap.len()].copy_from_slice(&self.bitmap);
        let mirr = MIRR_LCN as usize * CLUSTER;
        let mft = Self::record_offset(0);
        let copy = self.data[mft..mft + 4 * RECORD].to_vec();
        self.data[mirr..mirr + 4 * RECORD].copy_from_slice(&copy);
        let boot = self.data[..512].to_vec();
        let last = self.data.len() - 512;
        self.data[last..].copy_from_slice(&boot);
        self.data
    }
}

impl Default for NtfsImage {
    fn default() -> Self {
        Self::new()
    }
}

fn header(ty: u32, len: usize, non_resident: bool) -> Vec<u8> {
    let mut a = vec![0u8; len];
    put32(&mut a, 0, ty);
    put32(&mut a, 4, len as u32);
    a[8] = u8::from(non_resident);
    a
}

pub fn resident(ty: u32, value: &[u8]) -> Vec<u8> {
    let len = (0x18 + value.len()).next_multiple_of(8);
    let mut a = header(ty, len, false);
    put32(&mut a, 0x10, value.len() as u32);
    put16(&mut a, 0x14, 0x18);
    a[0x18..0x18 + value.len()].copy_from_slice(value);
    a
}

pub fn std_info(time: u64) -> Vec<u8> {
    let mut v = vec![0u8; 0x48];
    for at in [0, 8, 0x10, 0x18] {
        put64(&mut v, at, time);
    }
    resident(SI, &v)
}

pub fn file_name(parent: u64, parent_seq: u16, name: &str, namespace: u8) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut v = vec![0u8; 0x42 + 2 * units.len()];
    put64(&mut v, 0, reference(parent, parent_seq));
    for at in [8, 0x10, 0x18, 0x20] {
        put64(&mut v, at, TIME);
    }
    v[0x40] = units.len() as u8;
    v[0x41] = namespace;
    for (i, u) in units.iter().enumerate() {
        put16(&mut v, 0x42 + 2 * i, *u);
    }
    resident(FILE_NAME, &v)
}

/// Encodes a run list (Carrier ch. 13): header nibbles give the sizes of the length and of
/// the signed offset relative to the previous run.
pub fn encode_runs(runs: &[Run]) -> Vec<u8> {
    fn signed_size(v: i64) -> usize {
        (1..=8)
            .find(|&n| {
                let bits = 8 * n as u32 - 1;
                n == 8 || (v >= -(1i64 << bits) && v < (1i64 << bits))
            })
            .unwrap()
    }
    fn unsigned_size(v: u64) -> usize {
        (1..=8).find(|&n| n == 8 || v < (1u64 << (8 * n))).unwrap()
    }
    let mut out = Vec::new();
    let mut prev = 0i64;
    for &(len, lcn) in runs {
        let ls = unsigned_size(len);
        let (os, delta) = match lcn {
            Some(l) => {
                let d = l as i64 - prev;
                prev = l as i64;
                (signed_size(d), d)
            }
            None => (0, 0),
        };
        out.push(((os as u8) << 4) | ls as u8);
        out.extend_from_slice(&len.to_le_bytes()[..ls]);
        out.extend_from_slice(&delta.to_le_bytes()[..os]);
    }
    out.push(0);
    out
}

/// A non-resident attribute piece covering `runs` from `start_vcn`. `size` is written as
/// the data, allocated and initialized size.
pub fn non_resident(ty: u32, start_vcn: u64, runs: &[Run], size: u64, flags: u16) -> Vec<u8> {
    let pairs = encode_runs(runs);
    let len = (0x40 + pairs.len()).next_multiple_of(8);
    let mut a = header(ty, len, true);
    put16(&mut a, 0x0C, flags);
    let clusters: u64 = runs.iter().map(|r| r.0).sum();
    put64(&mut a, 0x10, start_vcn);
    put64(&mut a, 0x18, (start_vcn + clusters).wrapping_sub(1));
    put16(&mut a, 0x20, 0x40);
    if start_vcn == 0 {
        put64(&mut a, 0x28, size.next_multiple_of(CLUSTER as u64));
        put64(&mut a, 0x30, size);
        put64(&mut a, 0x38, size);
    }
    a[0x40..0x40 + pairs.len()].copy_from_slice(&pairs);
    a
}

/// An $ATTRIBUTE_LIST value: `(type, start VCN, record, seq)` per entry.
pub fn attr_list(entries: &[(u32, u64, u64, u16)]) -> Vec<u8> {
    let mut v = Vec::new();
    for (i, &(ty, vcn, rec, seq)) in entries.iter().enumerate() {
        let mut e = vec![0u8; 0x20];
        put32(&mut e, 0, ty);
        put16(&mut e, 4, 0x20);
        put64(&mut e, 8, vcn);
        put64(&mut e, 0x10, reference(rec, seq));
        put16(&mut e, 0x18, i as u16);
        v.extend(e);
    }
    resident(ATTR_LIST, &v)
}
