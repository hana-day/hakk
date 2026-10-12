//! MFT records (file record segments) and the attributes in them.
//!
//! Reference: Microsoft "FILE_RECORD_SEGMENT_HEADER structure", "MULTI_SECTOR_HEADER
//! structure", "ATTRIBUTE_RECORD_HEADER structure", "ATTRIBUTE_LIST_ENTRY structure",
//! "FILE_NAME structure", "STANDARD_INFORMATION structure", "MFT_SEGMENT_REFERENCE
//! structure" and "Attribute types" (learn.microsoft.com, Windows file system
//! documentation); Brian Carrier, "File System Forensic Analysis", chapters 12-13 ("MFT
//! Entries", "Fixup values", "Attribute header", "$STANDARD_INFORMATION", "$FILE_NAME",
//! "$ATTRIBUTE_LIST", "$DATA").
//!
//! A record starts with "FILE". Before it is written, the last two bytes of each 512-byte
//! stride are saved in the update sequence array and replaced by the update sequence
//! number, so a torn write shows up as a stride whose last two bytes do not match. Reading
//! a record means checking those bytes and putting the saved ones back ("fixup").

use crate::{
    le::{u8_at, u16_at, u32_at, u64_at, utf16},
    ntfs::runs::{self, Run, RunError},
};

pub(crate) const STRIDE: usize = 512;

pub(crate) const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub(crate) const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub(crate) const ATTR_FILE_NAME: u32 = 0x30;
pub(crate) const ATTR_VOLUME_NAME: u32 = 0x60;
pub(crate) const ATTR_DATA: u32 = 0x80;
const ATTR_END: u32 = 0xFFFF_FFFF;

/// FILE_RECORD_SEGMENT_HEADER flags.
const FLAG_IN_USE: u16 = 0x0001;
const FLAG_DIRECTORY: u16 = 0x0002;

/// ATTRIBUTE_RECORD_HEADER flags.
pub(crate) const ATTR_COMPRESSED: u16 = 0x00FF;
pub(crate) const ATTR_ENCRYPTED: u16 = 0x4000;

/// $FILE_NAME namespace of a DOS 8.3 alias (the others: 0 POSIX, 1 Win32, 3 a Win32 name
/// that is also a valid DOS name).
pub(crate) const NS_DOS: u8 = 2;

/// A reference to an MFT record: the record number (48 bits) and the sequence number it
/// had when the reference was made (16 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FileRef {
    pub(crate) record: u64,
    pub(crate) seq: u16,
}

impl FileRef {
    pub(crate) fn from_u64(v: u64) -> Self {
        Self {
            record: v & 0x0000_FFFF_FFFF_FFFF,
            #[allow(clippy::cast_possible_truncation)]
            seq: (v >> 48) as u16,
        }
    }
}

/// A $FILE_NAME attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Name {
    pub(crate) parent: FileRef,
    pub(crate) namespace: u8,
    pub(crate) name: String,
    /// The file size as of the last name change (Windows does not keep it current).
    pub(crate) size: u64,
    /// Created, modified and accessed times (FILETIME).
    pub(crate) times: [u64; 3],
}

/// One piece of an attribute's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Resident(Vec<u8>),
    NonResident(NonResident),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NonResident {
    pub(crate) start_vcn: u64,
    /// Clusters this piece spans (from its lowest to its highest VCN).
    pub(crate) vcns: u64,
    /// Only meaningful in the piece starting at VCN 0.
    pub(crate) data_size: u64,
    pub(crate) init_size: u64,
    pub(crate) compression_unit: u16,
    pub(crate) runs: Vec<Run>,
    pub(crate) run_error: Option<RunError>,
}

/// An unnamed $DATA attribute (or a piece of one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Data {
    pub(crate) flags: u16,
    pub(crate) value: Value,
}

/// What the scan needs from one record. Attributes not listed here are dropped right away
/// to keep the memory per record small.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Record {
    pub(crate) seq: u16,
    pub(crate) in_use: bool,
    pub(crate) is_dir: bool,
    /// For an extension record, the base record it belongs to.
    pub(crate) base: Option<FileRef>,
    /// Every stride's check bytes matched the update sequence number.
    pub(crate) fixup_ok: bool,
    /// $STANDARD_INFORMATION created, modified and accessed times (FILETIME).
    pub(crate) times: Option<[u64; 3]>,
    pub(crate) names: Vec<Name>,
    /// Pieces of the unnamed $DATA attribute.
    pub(crate) data: Vec<Data>,
    /// The $ATTRIBUTE_LIST value (resident), or where it is stored.
    pub(crate) attr_list: Option<Value>,
    pub(crate) label: Option<String>,
}

/// Applies the update sequence array to `rec` in place. Returns `false` if the array is
/// malformed or a stride's check bytes do not match (a torn or damaged record); strides
/// that match are still restored.
pub(crate) fn fixup(rec: &mut [u8]) -> bool {
    let usa_off = usize::from(u16_at(rec, 4));
    let count = usize::from(u16_at(rec, 6));
    let strides = rec.len() / STRIDE;
    if count < 2 || count - 1 != strides || usa_off < 0x28 || usa_off + 2 * count > rec.len() {
        return false;
    }
    let usn = u16_at(rec, usa_off);
    let mut ok = true;
    for i in 1..count {
        let saved = u16_at(rec, usa_off + 2 * i);
        let at = i * STRIDE - 2;
        if u16_at(rec, at) != usn {
            ok = false;
            continue;
        }
        if let Some(slot) = rec.get_mut(at..at + 2) {
            slot.copy_from_slice(&saved.to_le_bytes());
        }
    }
    ok
}

/// Parses a record read from the MFT. Returns `None` if it does not start with "FILE"
/// (never used, or damaged beyond use).
pub(crate) fn parse(mut rec: Vec<u8>) -> Option<Record> {
    if rec.get(0..4)? != b"FILE" {
        return None;
    }
    let fixup_ok = fixup(&mut rec);
    let flags = u16_at(&rec, 0x16);
    let base = u64_at(&rec, 0x20);
    let mut r = Record {
        seq: u16_at(&rec, 0x10),
        in_use: flags & FLAG_IN_USE != 0,
        is_dir: flags & FLAG_DIRECTORY != 0,
        base: (base != 0).then(|| FileRef::from_u64(base)),
        fixup_ok,
        ..Record::default()
    };
    let used = usize::try_from(u32_at(&rec, 0x18))
        .unwrap_or(usize::MAX)
        .min(rec.len());
    let mut at = usize::from(u16_at(&rec, 0x14));
    // Each attribute is at least 24 bytes long, so this ends within the record.
    while at.saturating_add(0x18) <= used {
        let ty = u32_at(&rec, at);
        if ty == ATTR_END {
            break;
        }
        let len = usize::try_from(u32_at(&rec, at + 4)).unwrap_or(0);
        let Some(attr) = at
            .checked_add(len)
            .filter(|&end| len >= 0x18 && end <= used)
            .and_then(|end| rec.get(at..end))
        else {
            break;
        };
        attribute(&mut r, ty, attr);
        at += len;
    }
    Some(r)
}

fn attribute(r: &mut Record, ty: u32, a: &[u8]) {
    let named = u8_at(a, 9) != 0;
    let flags = u16_at(a, 0x0C);
    let Some(value) = value(a) else {
        return;
    };
    match (ty, value) {
        (ATTR_STANDARD_INFORMATION, Value::Resident(v)) if v.len() >= 0x20 => {
            r.times = Some([u64_at(&v, 0), u64_at(&v, 8), u64_at(&v, 0x18)]);
        }
        (ATTR_FILE_NAME, Value::Resident(v)) => {
            if let Some(n) = file_name(&v) {
                r.names.push(n);
            }
        }
        (ATTR_DATA, value) if !named => r.data.push(Data { flags, value }),
        (ATTR_ATTRIBUTE_LIST, value) => r.attr_list = Some(value),
        (ATTR_VOLUME_NAME, Value::Resident(v)) => {
            let units: Vec<u16> = v
                .chunks_exact(2)
                .map(|c| u16_at(c, 0))
                .take_while(|&u| u != 0)
                .collect();
            r.label = Some(utf16(&units)).filter(|l| !l.is_empty());
        }
        _ => {}
    }
}

/// The value of an attribute (resident) or its run list (non-resident).
fn value(a: &[u8]) -> Option<Value> {
    if u8_at(a, 8) == 0 {
        let len = usize::try_from(u32_at(a, 0x10)).ok()?;
        let off = usize::from(u16_at(a, 0x14));
        return Some(Value::Resident(a.get(off..off.checked_add(len)?)?.to_vec()));
    }
    if a.len() < 0x40 {
        return None;
    }
    let start_vcn = u64_at(a, 0x10);
    let last_vcn = u64_at(a, 0x18);
    // An empty attribute has a highest VCN of -1.
    let vcns = last_vcn
        .checked_sub(start_vcn)
        .and_then(|d| d.checked_add(1))
        .unwrap_or(0);
    let pairs = a.get(usize::from(u16_at(a, 0x20))..).unwrap_or_default();
    let (runs, run_error) = runs::decode(pairs, start_vcn, vcns);
    Some(Value::NonResident(NonResident {
        start_vcn,
        vcns,
        data_size: u64_at(a, 0x30),
        init_size: u64_at(a, 0x38),
        compression_unit: u16_at(a, 0x22),
        runs,
        run_error,
    }))
}

fn file_name(v: &[u8]) -> Option<Name> {
    let len = usize::from(u8_at(v, 0x40));
    let bytes = v.get(0x42..0x42 + 2 * len)?;
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16_at(c, 0)).collect();
    Some(Name {
        parent: FileRef::from_u64(u64_at(v, 0)),
        namespace: u8_at(v, 0x41),
        name: utf16(&units),
        size: u64_at(v, 0x30),
        times: [u64_at(v, 8), u64_at(v, 0x10), u64_at(v, 0x20)],
    })
}

/// The records an $ATTRIBUTE_LIST value points to.
pub(crate) fn list_records(list: &[u8]) -> Vec<FileRef> {
    let mut out = Vec::new();
    let mut at = 0usize;
    // Each entry is at least 26 bytes long, so this ends.
    while at.saturating_add(0x1A) <= list.len() {
        let len = usize::from(u16_at(list, at + 4));
        if len < 0x1A {
            break;
        }
        let r = FileRef::from_u64(u64_at(list, at + 0x10));
        if !out.contains(&r) {
            out.push(r);
        }
        at += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1024-byte record with the given attributes, protected by an update sequence.
    pub(crate) fn record(flags: u16, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes());
        r[0x10..0x12].copy_from_slice(&7u16.to_le_bytes());
        r[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
        let mut at = 0x38;
        for a in attrs {
            r[at..at + a.len()].copy_from_slice(a);
            at += a.len();
        }
        r[at..at + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        r[0x18..0x1C].copy_from_slice(&u32::try_from(at + 8).unwrap().to_le_bytes());
        r[0x1C..0x20].copy_from_slice(&1024u32.to_le_bytes());
        // Update sequence number 0x0042; save the stride ends.
        r[0x30..0x32].copy_from_slice(&0x42u16.to_le_bytes());
        for i in 1..3 {
            let end = i * STRIDE - 2;
            let saved = [r[end], r[end + 1]];
            r[0x30 + 2 * i..0x32 + 2 * i].copy_from_slice(&saved);
            r[end..end + 2].copy_from_slice(&0x42u16.to_le_bytes());
        }
        r
    }

    fn resident(ty: u32, value: &[u8]) -> Vec<u8> {
        let len = (0x18 + value.len()).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&ty.to_le_bytes());
        a[4..8].copy_from_slice(&u32::try_from(len).unwrap().to_le_bytes());
        a[0x10..0x14].copy_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
        a[0x14..0x16].copy_from_slice(&0x18u16.to_le_bytes());
        a[0x18..0x18 + value.len()].copy_from_slice(value);
        a
    }

    #[test]
    fn parses_names_and_resident_data() {
        let mut fname = vec![0u8; 0x42];
        fname[0..8].copy_from_slice(&(5u64 | (5 << 48)).to_le_bytes());
        fname[0x40] = 3;
        fname[0x41] = 1; // Win32
        fname.extend("a.b".encode_utf16().flat_map(u16::to_le_bytes));
        // Resident data long enough to cross the first stride end.
        let data: Vec<u8> = (0..600u32)
            .map(|i| u8::try_from(i % 256).unwrap())
            .collect();
        let rec = record(
            FLAG_IN_USE,
            &[resident(ATTR_FILE_NAME, &fname), resident(ATTR_DATA, &data)],
        );
        let r = parse(rec).unwrap();
        assert!(r.fixup_ok && r.in_use && !r.is_dir);
        assert_eq!(r.seq, 7);
        assert_eq!(r.names.len(), 1);
        assert_eq!(r.names[0].name, "a.b");
        assert_eq!(r.names[0].parent, FileRef { record: 5, seq: 5 });
        assert_eq!(r.data[0].value, Value::Resident(data));
    }

    #[test]
    fn detects_torn_records() {
        let mut rec = record(FLAG_IN_USE, &[]);
        rec[1022] ^= 1;
        assert!(!parse(rec).unwrap().fixup_ok);
        let mut rec = record(0, &[]);
        rec[6] = 9; // array longer than the record has strides
        assert!(!parse(rec).unwrap().fixup_ok);
        assert!(parse(b"BAAD".to_vec()).is_none());
        assert!(parse(b"FILE".to_vec()).is_some());
    }

    #[test]
    fn lists_records() {
        let mut list = vec![0u8; 0x20 * 3];
        for (i, rec) in [9u64, 30, 9].iter().enumerate() {
            let e = &mut list[i * 0x20..];
            e[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
            e[4..6].copy_from_slice(&0x20u16.to_le_bytes());
            e[0x10..0x18].copy_from_slice(&(rec | (2 << 48)).to_le_bytes());
        }
        let refs = list_records(&list);
        assert_eq!(
            refs,
            [
                FileRef { record: 9, seq: 2 },
                FileRef { record: 30, seq: 2 }
            ]
        );
        assert!(list_records(&[0u8; 0x40]).is_empty());
    }
}
