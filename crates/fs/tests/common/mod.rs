//! Builders for small FAT and exFAT images with hand-placed (and hand-deleted) entries.
//! Written from the specifications independently of the parser under test.

#![allow(
    dead_code,
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::unwrap_used
)]

pub mod ntfs;

pub const SECTOR: usize = 512;

// ---------------------------------------------------------------------------------------
// FAT12/16/32
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FatKind {
    Fat12,
    Fat16,
    Fat32,
}

pub struct FatImage {
    pub data: Vec<u8>,
    pub kind: FatKind,
    reserved: usize,
    fat_sectors: usize,
    root_entries: usize,
    pub first_data: usize,
    pub count: u32,
}

#[derive(Clone, Copy)]
pub enum Dir {
    Root,
    Cluster(u32),
}

impl FatImage {
    /// One sector per cluster, two FATs.
    pub fn new(kind: FatKind) -> Self {
        let (total, reserved, root_entries, bits) = match kind {
            FatKind::Fat12 => (4000usize, 1usize, 512usize, 12usize),
            FatKind::Fat16 => (20000, 1, 512, 16),
            FatKind::Fat32 => (70000, 32, 0, 32),
        };
        let fat_sectors = ((total + 2) * bits).div_ceil(8 * SECTOR);
        let root_sectors = (root_entries * 32).div_ceil(SECTOR);
        let first_data = reserved + 2 * fat_sectors + root_sectors;
        let count = u32::try_from(total - first_data).unwrap();
        let mut img = Self {
            data: vec![0; total * SECTOR],
            kind,
            reserved,
            fat_sectors,
            root_entries,
            first_data,
            count,
        };
        let b = &mut img.data;
        b[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        b[3..11].copy_from_slice(b"MSWIN4.1");
        put16(b, 11, SECTOR as u16);
        b[13] = 1;
        put16(b, 14, reserved as u16);
        b[16] = 2;
        put16(b, 17, root_entries as u16);
        if total < 0x10000 {
            put16(b, 19, total as u16);
        } else {
            put32(b, 32, total as u32);
        }
        b[21] = 0xF8;
        let label_at = if kind == FatKind::Fat32 {
            put32(b, 36, fat_sectors as u32);
            put32(b, 44, 2);
            71
        } else {
            put16(b, 22, fat_sectors as u16);
            43
        };
        b[label_at - 5] = 0x29;
        b[label_at..label_at + 11].copy_from_slice(b"BOOTLABEL  ");
        b[510] = 0x55;
        b[511] = 0xAA;
        let eoc = img.eoc();
        img.set_fat(0, eoc & 0xFFFF_FFF8);
        img.set_fat(1, eoc);
        if kind == FatKind::Fat32 {
            img.set_fat(2, eoc);
        }
        img
    }

    pub fn eoc(&self) -> u32 {
        match self.kind {
            FatKind::Fat12 => 0xFFF,
            FatKind::Fat16 => 0xFFFF,
            FatKind::Fat32 => 0x0FFF_FFFF,
        }
    }

    pub fn set_fat(&mut self, c: u32, v: u32) {
        for copy in 0..2 {
            let base = (self.reserved + copy * self.fat_sectors) * SECTOR;
            let c = c as usize;
            match self.kind {
                FatKind::Fat12 => {
                    let at = base + c + c / 2;
                    let w = u16::from_le_bytes([self.data[at], self.data[at + 1]]);
                    let v = v as u16 & 0xFFF;
                    let w = if c % 2 == 1 {
                        (w & 0x000F) | (v << 4)
                    } else {
                        (w & 0xF000) | v
                    };
                    put16(&mut self.data, at, w);
                }
                FatKind::Fat16 => put16(&mut self.data, base + c * 2, v as u16),
                FatKind::Fat32 => put32(&mut self.data, base + c * 4, v),
            }
        }
    }

    /// Links `clusters` into a chain ending in EOC.
    pub fn chain(&mut self, clusters: &[u32]) {
        for w in clusters.windows(2) {
            self.set_fat(w[0], w[1]);
        }
        if let Some(&last) = clusters.last() {
            let eoc = self.eoc();
            self.set_fat(last, eoc);
        }
    }

    pub fn cluster_offset(&self, c: u32) -> usize {
        (self.first_data + c as usize - 2) * SECTOR
    }

    fn entry_offset(&self, dir: Dir, index: usize) -> usize {
        match dir {
            Dir::Root if self.kind != FatKind::Fat32 => {
                assert!(index < self.root_entries);
                (self.reserved + 2 * self.fat_sectors) * SECTOR + index * 32
            }
            Dir::Root => self.cluster_offset(2) + index * 32,
            Dir::Cluster(c) => self.cluster_offset(c) + index * 32,
        }
    }

    /// Writes a short entry. `name` is the raw 11-byte name.
    pub fn short(
        &mut self,
        dir: Dir,
        index: usize,
        name: &[u8; 11],
        attr: u8,
        cluster: u32,
        size: u32,
    ) {
        let at = self.entry_offset(dir, index);
        let e = &mut self.data[at..at + 32];
        e[..11].copy_from_slice(name);
        e[11] = attr;
        put16(e, 20, (cluster >> 16) as u16);
        put16(e, 26, cluster as u16);
        put32(e, 28, size);
        // 2009-01-02 03:04:06, created with 150 * 10 ms extra.
        put16(e, 24, ((2009 - 1980) << 9) | (1 << 5) | 2);
        put16(e, 22, (3 << 11) | (4 << 5) | 3);
        put16(e, 16, ((2009 - 1980) << 9) | (1 << 5) | 2);
        put16(e, 14, (3 << 11) | (4 << 5) | 3);
        e[13] = 150;
    }

    /// Writes the long-name entries for `long` at `index..` (followed by the short entry at
    /// the returned index). `deleted` marks them as a deleted file would be.
    pub fn long(
        &mut self,
        dir: Dir,
        index: usize,
        long: &str,
        short: &[u8; 11],
        deleted: bool,
    ) -> usize {
        let units: Vec<u16> = long.encode_utf16().collect();
        let n = units.len().div_ceil(13);
        let sum = lfn_checksum(short);
        for k in 0..n {
            // On disk the last part comes first.
            let part = n - k;
            let at = self.entry_offset(dir, index + k);
            let e = &mut self.data[at..at + 32];
            e.fill(0);
            e[0] = if deleted {
                0xE5
            } else {
                part as u8 | if k == 0 { 0x40 } else { 0 }
            };
            e[11] = 0x0F;
            e[13] = sum;
            let slots = (1..11)
                .step_by(2)
                .chain((14..26).step_by(2))
                .chain((28..32).step_by(2));
            for (i, off) in slots.enumerate() {
                let u = match units.get((part - 1) * 13 + i) {
                    Some(&u) => u,
                    None if (part - 1) * 13 + i == units.len() => 0,
                    None => 0xFFFF,
                };
                put16(e, off, u);
            }
        }
        index + n
    }

    pub fn fill(&mut self, c: u32, byte: u8) {
        let at = self.cluster_offset(c);
        self.data[at..at + SECTOR].fill(byte);
    }

    /// Writes `.` and `..` at the start of directory cluster `c`.
    pub fn dots(&mut self, c: u32, parent: u32) {
        self.short(Dir::Cluster(c), 0, b".          ", 0x10, c, 0);
        self.short(Dir::Cluster(c), 1, b"..         ", 0x10, parent, 0);
    }
}

pub fn lfn_checksum(short: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &b in short {
        sum = (if sum & 1 == 1 { 0x80u8 } else { 0 })
            .wrapping_add(sum >> 1)
            .wrapping_add(b);
    }
    sum
}

// ---------------------------------------------------------------------------------------
// exFAT
// ---------------------------------------------------------------------------------------

pub struct ExfatImage {
    pub data: Vec<u8>,
    fat_offset: usize,
    heap_offset: usize,
    pub count: u32,
    pub root: u32,
    bitmap_cluster: u32,
}

pub struct FileSpec<'a> {
    pub name: &'a str,
    pub dir: bool,
    pub first: u32,
    pub size: u64,
    pub no_fat_chain: bool,
    pub in_use: bool,
}

impl ExfatImage {
    /// 512-byte sectors and clusters; bitmap in cluster 2, root directory in clusters 3-6.
    pub fn new(count: u32) -> Self {
        let fat_offset = 24;
        let fat_len = ((count as usize + 2) * 4).div_ceil(SECTOR);
        let heap = fat_offset + fat_len;
        let total = heap + count as usize;
        let mut img = Self {
            data: vec![0; total * SECTOR],
            fat_offset: fat_offset * SECTOR,
            heap_offset: heap * SECTOR,
            count,
            root: 3,
            bitmap_cluster: 2,
        };
        let b = &mut img.data;
        b[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
        b[3..11].copy_from_slice(b"EXFAT   ");
        put64(b, 72, total as u64);
        put32(b, 80, fat_offset as u32);
        put32(b, 84, fat_len as u32);
        put32(b, 88, heap as u32);
        put32(b, 92, count);
        put32(b, 96, 3);
        put16(b, 104, 0x0100);
        b[108] = 9;
        b[109] = 0;
        b[110] = 1;
        b[510] = 0x55;
        b[511] = 0xAA;
        img.set_fat(0, 0xFFFF_FFF8);
        img.set_fat(1, 0xFFFF_FFFF);
        img.set_fat(2, 0xFFFF_FFFF);
        img.chain(&[3, 4, 5, 6]);
        img.allocate(2, 5);
        // Root: allocation bitmap and volume label entries.
        let r = img.cluster_offset(3);
        let e = &mut img.data[r..r + 32];
        e[0] = 0x81;
        put32(e, 20, 2);
        put64(e, 24, u64::from(count).div_ceil(8));
        let e = &mut img.data[r + 32..r + 64];
        e[0] = 0x83;
        e[1] = 4;
        for (i, u) in "HAKK".encode_utf16().enumerate() {
            put16(e, 2 + 2 * i, u);
        }
        img
    }

    pub fn set_fat(&mut self, c: u32, v: u32) {
        let at = self.fat_offset + c as usize * 4;
        put32(&mut self.data, at, v);
    }

    pub fn chain(&mut self, clusters: &[u32]) {
        for w in clusters.windows(2) {
            self.set_fat(w[0], w[1]);
        }
        if let Some(&last) = clusters.last() {
            self.set_fat(last, 0xFFFF_FFFF);
        }
    }

    /// Marks `n` clusters from `first` in use in the allocation bitmap.
    pub fn allocate(&mut self, first: u32, n: u32) {
        let base = self.cluster_offset(self.bitmap_cluster);
        for c in first..first + n {
            let i = (c - 2) as usize;
            self.data[base + i / 8] |= 1 << (i % 8);
        }
    }

    pub fn cluster_offset(&self, c: u32) -> usize {
        self.heap_offset + (c as usize - 2) * SECTOR
    }

    /// Writes a File entry set into directory cluster `dir` at entry `index`. Returns the
    /// index after the set.
    pub fn set(&mut self, dir: u32, index: usize, f: &FileSpec) -> usize {
        let units: Vec<u16> = f.name.encode_utf16().collect();
        let names = units.len().div_ceil(15);
        let mut entries = vec![[0u8; 32]; 2 + names];
        let e = &mut entries[0];
        e[0] = 0x85;
        e[1] = (1 + names) as u8;
        put16(e, 4, if f.dir { 0x10 } else { 0x20 });
        // 2024-05-06 07:08:10 local, UTC+09:00, +1.23 s.
        let ts = ((2024 - 1980) << 25) | (5 << 21) | (6 << 16) | (7 << 11) | (8 << 5) | 5;
        put32(e, 8, ts);
        put32(e, 12, ts);
        put32(e, 16, ts);
        e[20] = 123;
        e[21] = 123;
        e[22] = 0x80 | 36;
        e[23] = 0x80 | 36;
        e[24] = 0x80 | 36;
        let s = &mut entries[1];
        s[0] = 0xC0;
        s[1] = 0x01 | if f.no_fat_chain { 0x02 } else { 0 };
        s[3] = units.len() as u8;
        put64(s, 8, f.size);
        put32(s, 20, f.first);
        put64(s, 24, f.size);
        for k in 0..names {
            let n = &mut entries[2 + k];
            n[0] = 0xC1;
            for i in 0..15 {
                if let Some(&u) = units.get(k * 15 + i) {
                    put16(n, 2 + 2 * i, u);
                }
            }
        }
        let sum = exfat_checksum(&entries);
        put16(&mut entries[0], 2, sum);
        if !f.in_use {
            for e in &mut entries {
                e[0] &= 0x7F;
            }
        }
        let base = self.cluster_offset(dir);
        for (k, e) in entries.iter().enumerate() {
            let at = base + (index + k) * 32;
            self.data[at..at + 32].copy_from_slice(e);
        }
        index + entries.len()
    }

    pub fn raw_entry(&mut self, dir: u32, index: usize, bytes: [u8; 32]) {
        let at = self.cluster_offset(dir) + index * 32;
        self.data[at..at + 32].copy_from_slice(&bytes);
    }
}

/// exFAT specification 6.3.3, Figure 2.
pub fn exfat_checksum(entries: &[[u8; 32]]) -> u16 {
    let mut sum = 0u16;
    for (i, e) in entries.iter().enumerate() {
        for (j, &b) in e.iter().enumerate() {
            if i == 0 && (j == 2 || j == 3) {
                continue;
            }
            sum = (if sum & 1 == 1 { 0x8000u16 } else { 0 })
                .wrapping_add(sum >> 1)
                .wrapping_add(u16::from(b));
        }
    }
    sum
}

pub fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

pub fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

pub fn put64(b: &mut [u8], at: usize, v: u64) {
    b[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
