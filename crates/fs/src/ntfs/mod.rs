//! NTFS.
//!
//! Reference: Microsoft NTFS documentation on learn.microsoft.com ("NTFS overview", the
//! "Master File Table" pages, and the on-disk structure references named in `boot`,
//! `record` and `runs`); Brian Carrier, "File System Forensic Analysis" (Addison-Wesley,
//! 2005), chapters 11-13 (NTFS concepts, analysis and data structures).
//!
//! Every file and directory has a record in the Master File Table ($MFT, record 0), whose
//! own placement comes from the data runs of record 0 (the MFT may itself be fragmented).
//! Deleting a file clears the record's in-use flag and frees its clusters in $Bitmap
//! (record 6), but leaves the record's attributes alone until the record is reused, so a
//! deleted file keeps its name, its parent directory reference and the data runs that say
//! exactly where each fragment lies. The parent reference carries the parent's sequence
//! number, which changes whenever a record is freed or reused, so a parent that has since
//! become another file is detected rather than mistaken for the original folder.
//!
//! Records 0-15 hold the file system's own metadata files and are not listed.

mod boot;
mod record;
mod runs;

use std::collections::{HashMap, HashSet};

pub(crate) use boot::{Boot, is_boot_sector, probe};
use record::{
    ATTR_COMPRESSED, ATTR_ENCRYPTED, Data, FileRef, NS_DOS, Name, NonResident, Record, Value,
};
use runs::Run;

use crate::{
    Confidence, Entry, Extent, FsKind, Hole, Layout, Volume, disk::Disk, le::sanitize_name,
    time::Timestamp,
};

const MFT: u64 = 0;
const VOLUME: u64 = 3;
const ROOT: u64 = 5;
const BITMAP: u64 = 6;
const EXTEND: u64 = 11;
/// Records below this hold metadata files.
const FIRST_USER_RECORD: u64 = 16;
/// Folder for files whose parent directory is gone (reused, or not a directory).
pub(crate) const ORPHANS: &str = "$OrphanFiles";
/// Deepest folder nesting followed when building a path (also ends reference cycles).
const MAX_DEPTH: usize = 1024;
/// The MFT is read in chunks of this many bytes.
const CHUNK: u64 = 4 << 20;
/// Largest $ATTRIBUTE_LIST read (Carrier ch. 13: lists are small; 256 KiB is generous).
const MAX_ATTR_LIST: u64 = 256 << 10;

/// A stream assembled from the pieces of a non-resident attribute.
#[derive(Debug, Default)]
struct Stream {
    runs: Vec<Run>,
    size: u64,
    init: u64,
    /// The pieces cover every cluster up to `size`, and every run list decoded cleanly.
    complete: bool,
}

/// Puts the pieces of a non-resident attribute in VCN order and joins their runs. Stops
/// at the first gap (the following data cannot be placed).
fn combine<'a>(pieces: impl IntoIterator<Item = &'a NonResident>, cluster_size: u64) -> Stream {
    let mut pieces: Vec<&NonResident> = pieces.into_iter().collect();
    pieces.sort_by_key(|p| p.start_vcn);
    pieces.dedup_by_key(|p| p.start_vcn);
    let mut s = Stream {
        complete: true,
        ..Stream::default()
    };
    match pieces.first() {
        Some(first) if first.start_vcn == 0 => {
            s.size = first.data_size;
            s.init = first.init_size.min(first.data_size);
        }
        _ => {
            s.complete = false;
            return s;
        }
    }
    let mut next = 0u64;
    for p in pieces {
        if p.start_vcn != next {
            s.complete = false;
            break;
        }
        let covered = runs::clusters(&p.runs);
        s.runs.extend_from_slice(&p.runs);
        next = p.start_vcn.saturating_add(covered);
        if p.run_error.is_some() || covered != p.vcns {
            // A damaged run list: what follows it cannot be placed.
            s.complete = s.complete && next.saturating_mul(cluster_size) >= s.size;
            break;
        }
    }
    if next.saturating_mul(cluster_size) < s.size {
        s.complete = false;
    }
    s
}

/// The allocation state of every cluster, from $Bitmap.
struct Alloc {
    words: Vec<u64>,
    /// Allocated clusters before each block of `BLOCK_WORDS` words.
    ranks: Vec<u64>,
    clusters: u64,
}

const BLOCK_WORDS: usize = 64;

impl Alloc {
    fn new(bitmap: &[u8], clusters: u64) -> Self {
        let mut words: Vec<u64> = bitmap
            .chunks(8)
            .map(|c| {
                let mut w = [0u8; 8];
                for (d, s) in w.iter_mut().zip(c) {
                    *d = *s;
                }
                u64::from_le_bytes(w)
            })
            .collect();
        words.truncate(usize::try_from(clusters.div_ceil(64)).unwrap_or(usize::MAX));
        let mut ranks = Vec::with_capacity(words.len() / BLOCK_WORDS + 1);
        let mut total = 0u64;
        for block in words.chunks(BLOCK_WORDS) {
            ranks.push(total);
            total += block.iter().map(|w| u64::from(w.count_ones())).sum::<u64>();
        }
        Self {
            words,
            ranks,
            clusters,
        }
    }

    /// Clusters the bitmap covers (any beyond count as allocated).
    fn covered(&self) -> u64 {
        (self.words.len() as u64 * 64).min(self.clusters)
    }

    /// Allocated clusters among `0..c` (`c` at most `covered()`).
    fn rank(&self, c: u64) -> u64 {
        let Ok(word) = usize::try_from(c / 64) else {
            return 0;
        };
        let block = word / BLOCK_WORDS;
        let mut n = self.ranks.get(block).copied().unwrap_or(0);
        for w in self
            .words
            .get(block * BLOCK_WORDS..word)
            .unwrap_or_default()
        {
            n += u64::from(w.count_ones());
        }
        let bits = c % 64;
        if bits > 0 {
            let w = self.words.get(word).copied().unwrap_or(0);
            n += u64::from((w & ((1u64 << bits) - 1)).count_ones());
        }
        n
    }

    fn any_allocated(&self, lcn: u64, len: u64) -> bool {
        let end = lcn.saturating_add(len);
        if end > self.covered() {
            return true;
        }
        self.rank(end) > self.rank(lcn)
    }

    /// Unallocated cluster runs as `(first, count)`.
    fn free_runs(&self) -> Vec<(u64, u64)> {
        let mut out: Vec<(u64, u64)> = Vec::new();
        let limit = self.covered();
        let mut push = |c: u64, n: u64| match out.last_mut() {
            Some(last) if last.0 + last.1 == c => last.1 += n,
            _ => out.push((c, n)),
        };
        for (i, &w) in self.words.iter().enumerate() {
            let base = i as u64 * 64;
            if w == u64::MAX {
                continue;
            }
            if w == 0 && base + 64 <= limit {
                push(base, 64);
                continue;
            }
            for bit in 0..64 {
                let c = base + bit;
                if c >= limit {
                    break;
                }
                if w & (1 << bit) == 0 {
                    push(c, 1);
                }
            }
        }
        out
    }
}

struct Scanner<'a> {
    disk: &'a Disk<'a>,
    boot: &'a Boot,
    /// Clusters that lie within the source.
    clusters: u64,
    warnings: Vec<String>,
}

impl Scanner<'_> {
    fn read_record_at(&mut self, offset: u64) -> Option<Record> {
        let len = usize::try_from(self.boot.record_size).ok()?;
        record::parse(self.disk.read_lossy(offset, len, &mut self.warnings))
    }

    /// Reads record `n` through the given MFT runs.
    fn read_record(&mut self, mft: &[Run], n: u64) -> Option<Record> {
        let rs = self.boot.record_size;
        let len = usize::try_from(rs).ok()?;
        let at = n.checked_mul(rs)?;
        record::parse(runs::read(
            self.disk,
            mft,
            self.boot.cluster_size,
            at,
            len,
            &mut self.warnings,
        ))
    }

    /// Reads an attribute list value, wherever it is stored.
    fn attr_list_bytes(&mut self, v: &Value) -> Vec<u8> {
        match v {
            Value::Resident(b) => b.clone(),
            Value::NonResident(nr) => {
                let s = combine([nr], self.boot.cluster_size);
                let len = usize::try_from(s.size.min(MAX_ATTR_LIST)).unwrap_or(0);
                runs::read(
                    self.disk,
                    &s.runs,
                    self.boot.cluster_size,
                    0,
                    len,
                    &mut self.warnings,
                )
            }
        }
    }

    /// The runs and size of $MFT, from record 0 (or its copy in $MFTMirr). Pieces of the
    /// $DATA attribute in extension records (listed in $ATTRIBUTE_LIST) are read through
    /// the runs known so far, since they lie early in the MFT.
    fn locate_mft(&mut self) -> Option<Stream> {
        let cs = self.boot.cluster_size;
        let mut best: Option<Stream> = None;
        for lcn in [self.boot.mft_lcn, self.boot.mftmirr_lcn] {
            let Some(rec0) = self.read_record_at(lcn.saturating_mul(cs)) else {
                continue;
            };
            if rec0.base.is_some() {
                continue;
            }
            let mut pieces: Vec<NonResident> = non_resident(&rec0.data).cloned().collect();
            if let Some(list) = &rec0.attr_list {
                let list = self.attr_list_bytes(list);
                let mut pending: Vec<FileRef> = record::list_records(&list)
                    .into_iter()
                    .filter(|r| r.record != MFT)
                    .collect();
                // Each pass reads the records reachable through the runs found so far.
                while !pending.is_empty() {
                    let known = combine(pieces.iter(), cs).runs;
                    let before = pending.len();
                    pending.retain(|r| {
                        let Some(ext) = self.read_record(&known, r.record) else {
                            return true;
                        };
                        if ext.base
                            == Some(FileRef {
                                record: MFT,
                                seq: rec0.seq,
                            })
                        {
                            pieces.extend(non_resident(&ext.data).cloned());
                        }
                        false
                    });
                    if pending.len() == before {
                        break;
                    }
                }
            }
            let s = combine(pieces.iter(), cs);
            if s.complete && !s.runs.is_empty() {
                return Some(s);
            }
            if best.as_ref().is_none_or(|b| s.runs.len() > b.runs.len()) {
                best = Some(s);
            }
        }
        best.filter(|s| !s.runs.is_empty())
    }

    /// Reads every record of the MFT. Records that are not records (never used, or
    /// unreadable) are left out.
    fn read_mft(&mut self, mft: &Stream) -> HashMap<u64, Record> {
        let rs = self.boot.record_size;
        let cs = self.boot.cluster_size;
        let stored = runs::clusters(&mft.runs).saturating_mul(cs);
        let count = mft.size.min(stored) / rs;
        let per_chunk = (CHUNK / rs).max(1);
        let mut out = HashMap::new();
        let mut torn = 0u64;
        let mut n = 0u64;
        while n < count {
            let k = per_chunk.min(count - n);
            let Ok(len) = usize::try_from(k * rs) else {
                break;
            };
            let bytes = runs::read(self.disk, &mft.runs, cs, n * rs, len, &mut self.warnings);
            for (i, rec) in bytes
                .chunks_exact(usize::try_from(rs).unwrap_or(1024))
                .enumerate()
            {
                if let Some(r) = record::parse(rec.to_vec()) {
                    torn += u64::from(!r.fixup_ok);
                    out.insert(n + i as u64, r);
                }
            }
            n += k;
        }
        if torn > 0 {
            self.warnings.push(format!(
                "{torn} MFT record(s) failed the update sequence check (torn or damaged)"
            ));
        }
        out
    }
}

fn non_resident<'a>(
    data: impl IntoIterator<Item = &'a Data>,
) -> impl Iterator<Item = &'a NonResident> {
    data.into_iter().filter_map(|d| match &d.value {
        Value::NonResident(nr) => Some(nr),
        Value::Resident(_) => None,
    })
}

/// A base record with the attributes of its extension records merged in.
struct File<'a> {
    rec: &'a Record,
    names: Vec<&'a Name>,
    data: Vec<&'a Data>,
    /// An extension record named in the attribute list could not be found or no longer
    /// belongs to this file.
    missing_parts: bool,
}

/// Whether `rec` (record `n`) is the one a reference made with sequence number `seq`
/// meant. Freeing a record bumps its sequence number, so a deleted record may be one
/// ahead of references made while it was live.
fn same_seq(rec: &Record, seq: u16) -> bool {
    rec.seq == seq || (!rec.in_use && rec.seq == seq.wrapping_add(1))
}

/// Merges the extension records of every base record.
fn assemble<'a>(
    records: &'a HashMap<u64, Record>,
    lists: &HashMap<u64, Vec<FileRef>>,
) -> HashMap<u64, File<'a>> {
    // Extension records by the base record they name.
    let mut exts: HashMap<u64, Vec<u64>> = HashMap::new();
    for (&n, r) in records {
        if let Some(b) = r.base {
            exts.entry(b.record).or_default().push(n);
        }
    }
    let mut files = HashMap::new();
    for (&n, rec) in records {
        if rec.base.is_some() {
            continue;
        }
        let mut f = File {
            rec,
            names: rec.names.iter().collect(),
            data: rec.data.iter().collect(),
            missing_parts: false,
        };
        let mut parts: Vec<u64> = exts.get(&n).cloned().unwrap_or_default();
        parts.sort_unstable();
        for r in lists.get(&n).into_iter().flatten() {
            if r.record == n {
                continue;
            }
            if !parts.contains(&r.record) {
                // Listed, but the record no longer points back here.
                f.missing_parts = true;
            }
        }
        for p in parts {
            let Some(ext) = records.get(&p) else {
                continue;
            };
            let Some(base) = ext.base else {
                continue;
            };
            if base.record != n || !same_seq(rec, base.seq) {
                continue;
            }
            f.names.extend(ext.names.iter());
            f.data.extend(ext.data.iter());
        }
        files.insert(n, f);
    }
    files
}

/// The name a file is listed under: a Win32 or POSIX name if there is one (the DOS 8.3
/// alias is a fallback). The flag says whether the long name was found.
fn best_name<'a>(names: &[&'a Name]) -> Option<(&'a Name, bool)> {
    names
        .iter()
        .find(|n| n.namespace != NS_DOS)
        .map(|n| (*n, true))
        .or_else(|| names.first().map(|n| (*n, false)))
}

#[derive(Clone)]
struct DirPath {
    path: String,
    deleted: bool,
    exact: bool,
}

/// Builds folder paths from parent references, checking each step's sequence number.
struct Paths<'a> {
    files: &'a HashMap<u64, File<'a>>,
    memo: HashMap<u64, DirPath>,
}

impl Paths<'_> {
    /// The path of the directory `r` refers to (`""` for the root), whether it or one of
    /// its ancestors is deleted, and whether every name on the way is exact.
    fn dir(&mut self, r: FileRef) -> DirPath {
        // Walk up until the root, a known directory, or a broken link.
        let mut chain: Vec<(u64, String, bool, bool)> = Vec::new();
        let mut seen: HashSet<u64> = HashSet::new();
        let mut cur = r;
        let tail = loop {
            if cur.record == ROOT {
                break DirPath {
                    path: String::new(),
                    deleted: false,
                    exact: true,
                };
            }
            if let Some(d) = self.memo.get(&cur.record)
                && self
                    .files
                    .get(&cur.record)
                    .is_some_and(|f| same_seq(f.rec, cur.seq))
            {
                break d.clone();
            }
            let file = self.files.get(&cur.record).filter(|f| {
                f.rec.is_dir && same_seq(f.rec, cur.seq) && !seen.contains(&cur.record)
            });
            let named = file.and_then(|f| best_name(&f.names).map(|n| (f, n)));
            let Some((f, (name, long))) = named.filter(|_| chain.len() < MAX_DEPTH) else {
                break DirPath {
                    path: ORPHANS.to_owned(),
                    deleted: false,
                    exact: false,
                };
            };
            seen.insert(cur.record);
            let (clean, ok) = sanitize_name(name.name.clone());
            chain.push((cur.record, clean, !f.rec.in_use, long && ok));
            cur = name.parent;
        };
        let mut acc = tail;
        for (n, name, deleted, exact) in chain.into_iter().rev() {
            acc = DirPath {
                path: join(&acc.path, &name),
                deleted: acc.deleted || deleted,
                exact: acc.exact && exact,
            };
            self.memo.insert(n, acc.clone());
        }
        acc
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
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
    let cs = boot.cluster_size;
    let mut sc = Scanner {
        disk,
        boot,
        clusters: boot.clusters.min(disk.len() / cs),
        warnings,
    };
    let mut vol = Volume {
        kind: FsKind::Ntfs,
        offset: disk.start(),
        len: disk.len(),
        cluster_size: cs,
        label: None,
        entries: Vec::new(),
        free: Vec::new(),
        warnings: Vec::new(),
    };
    let Some(mft) = sc.locate_mft() else {
        sc.warnings
            .push("the MFT could not be located (record 0 and its mirror are unusable)".into());
        vol.warnings = sc.warnings;
        return vol;
    };
    if !mft.complete {
        sc.warnings
            .push("the MFT's own data runs are incomplete; later records are missing".into());
    }
    let records = sc.read_mft(&mft);

    // Attribute lists, read once (a non-resident one costs a read).
    let mut lists: HashMap<u64, Vec<FileRef>> = HashMap::new();
    for (&n, r) in &records {
        if let (None, Some(v)) = (r.base, &r.attr_list) {
            let bytes = sc.attr_list_bytes(v);
            lists.insert(n, record::list_records(&bytes));
        }
    }
    let files = assemble(&records, &lists);

    vol.label = files.get(&VOLUME).and_then(|f| f.rec.label.clone());
    let alloc = files
        .get(&BITMAP)
        .map(|f| {
            let s = combine(non_resident(f.data.iter().copied()), cs);
            let need = sc.clusters.div_ceil(8);
            let len = usize::try_from(s.size.min(need)).unwrap_or(0);
            let bytes = runs::read(sc.disk, &s.runs, cs, 0, len, &mut sc.warnings);
            Alloc::new(&bytes, sc.clusters)
        })
        .filter(|a| !a.words.is_empty());
    if alloc.is_none() {
        sc.warnings
            .push("no usable $Bitmap; reallocated clusters cannot be detected".into());
    }

    let mut paths = Paths {
        files: &files,
        memo: HashMap::new(),
    };
    let mut numbers: Vec<u64> = files.keys().copied().collect();
    numbers.sort_unstable();
    for n in numbers {
        let Some(f) = files.get(&n) else {
            continue;
        };
        if n < FIRST_USER_RECORD {
            continue;
        }
        let Some((name, long)) = best_name(&f.names) else {
            continue;
        };
        if name.parent.record == EXTEND {
            continue;
        }
        let dir = paths.dir(name.parent);
        let (clean, ok) = sanitize_name(name.name.clone());
        let deleted = !f.rec.in_use || dir.deleted;
        let mut e = Entry {
            path: join(&dir.path, &clean),
            name: clean,
            name_exact: long && ok,
            is_dir: f.rec.is_dir,
            deleted,
            size: 0,
            valid_size: 0,
            first_cluster: 0,
            extents: Vec::new(),
            holes: Vec::new(),
            resident: None,
            record: Some(n),
            layout: Layout::Runs,
            confidence: Confidence::High,
            created: None,
            modified: None,
            accessed: None,
        };
        let times = f.rec.times.unwrap_or(name.times);
        [e.created, e.modified, e.accessed] = times.map(Timestamp::from_filetime);
        if !f.rec.is_dir {
            place(&mut e, f, name, &sc, alloc.as_ref());
        }
        if !f.rec.fixup_ok || f.missing_parts {
            e.confidence = Confidence::Low;
        }
        vol.entries.push(e);
    }

    if let Some(a) = &alloc {
        vol.free = a
            .free_runs()
            .into_iter()
            .map(|(c, n)| Extent {
                offset: disk.start().saturating_add(c.saturating_mul(cs)),
                len: n.saturating_mul(cs),
            })
            .collect();
    }
    vol.warnings = sc.warnings;
    vol
}

/// Fills in where a file's data lies and how far that can be trusted.
fn place(e: &mut Entry, f: &File, name: &Name, sc: &Scanner, alloc: Option<&Alloc>) {
    let cs = sc.boot.cluster_size;
    if let Some(v) = f.data.iter().find_map(|d| match &d.value {
        Value::Resident(v) => Some(v),
        Value::NonResident(_) => None,
    }) {
        e.size = v.len() as u64;
        e.valid_size = e.size;
        e.resident = Some(v.clone());
        e.layout = Layout::Resident;
        return;
    }
    let Some(first) = f.data.first() else {
        // No $DATA left (its extension record was reused): only the size recorded with
        // the name is known, and Windows does not keep that one current.
        e.size = name.size;
        e.valid_size = e.size;
        e.confidence = Confidence::Low;
        return;
    };
    let s = combine(non_resident(f.data.iter().copied()), cs);
    e.size = s.size;
    e.valid_size = s.init;
    let compressed = f.data.iter().any(|d| {
        d.flags & ATTR_COMPRESSED != 0
            || matches!(&d.value, Value::NonResident(nr) if nr.compression_unit != 0)
    });
    if compressed || first.flags & ATTR_ENCRYPTED != 0 {
        // The clusters do not hold the file's bytes as is.
        e.confidence = Confidence::Low;
        return;
    }
    let mut complete = s.complete;
    let mut reallocated = false;
    // Extents on either side of a hole stay apart even when they touch on disk, or the
    // hole would land in the wrong place.
    let mut after_hole = false;
    for r in &s.runs {
        let file_off = r.vcn.saturating_mul(cs);
        if file_off >= s.size {
            break;
        }
        let len = r.len.saturating_mul(cs).min(s.size - file_off);
        let Some(lcn) = r.lcn else {
            match e.holes.last_mut() {
                Some(h) if h.file_offset.saturating_add(h.len) == file_off => h.len += len,
                _ => e.holes.push(Hole {
                    file_offset: file_off,
                    len,
                }),
            }
            after_hole = true;
            continue;
        };
        let n = len.div_ceil(cs);
        if lcn.checked_add(n).is_none_or(|end| end > sc.clusters) {
            // Points outside the volume (or past the end of the source).
            complete = false;
            break;
        }
        if e.first_cluster == 0 {
            e.first_cluster = u32::try_from(lcn).unwrap_or(u32::MAX);
        }
        if e.deleted {
            reallocated |= alloc.is_some_and(|a| a.any_allocated(lcn, n));
        }
        let offset = sc.disk.start().saturating_add(lcn.saturating_mul(cs));
        match e.extents.last_mut() {
            Some(x) if x.end() == offset && !after_hole => x.len += len,
            _ => e.extents.push(Extent { offset, len }),
        }
        after_hole = false;
    }
    e.confidence = if !complete || reallocated {
        Confidence::Low
    } else if e.deleted && alloc.is_none() {
        Confidence::Medium
    } else {
        Confidence::High
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmap_ranks() {
        let mut bm = vec![0u8; 64 * 8 * 3];
        bm[0] = 0b0000_0101; // clusters 0 and 2
        bm[600] = 0x80; // cluster 4807
        let a = Alloc::new(&bm, 64 * 64 * 3 - 10);
        assert!(a.any_allocated(0, 1));
        assert!(!a.any_allocated(1, 1));
        assert!(a.any_allocated(1, 2));
        assert!(!a.any_allocated(3, 4804));
        assert!(a.any_allocated(3, 4805));
        // Past the end counts as allocated.
        assert!(a.any_allocated(64 * 64 * 3 - 11, 2));
        let free = a.free_runs();
        assert_eq!(free.first(), Some(&(1, 1)));
        assert_eq!(free.get(1), Some(&(3, 4804)));
        assert_eq!(free.get(2), Some(&(4808, 64 * 64 * 3 - 10 - 4808)));
    }

    fn piece(start_vcn: u64, runs: &[(u64, Option<u64>)], size: u64) -> NonResident {
        let mut vcn = start_vcn;
        let runs: Vec<Run> = runs
            .iter()
            .map(|&(len, lcn)| {
                let r = Run { vcn, len, lcn };
                vcn += len;
                r
            })
            .collect();
        NonResident {
            start_vcn,
            vcns: vcn - start_vcn,
            data_size: size,
            init_size: size,
            compression_unit: 0,
            runs,
            run_error: None,
        }
    }

    #[test]
    fn combines_pieces_in_vcn_order() {
        let a = piece(0, &[(4, Some(100))], 10 * 512);
        let b = piece(4, &[(6, Some(50))], 0);
        let s = combine([&b, &a], 512);
        assert!(s.complete);
        assert_eq!(s.runs.len(), 2);
        assert_eq!(s.size, 5120);
        // A gap: the second piece is missing.
        let c = piece(6, &[(4, Some(50))], 0);
        assert!(!combine([&a, &c], 512).complete);
        // Without the first piece nothing is known.
        assert!(!combine([&b], 512).complete);
    }
}
