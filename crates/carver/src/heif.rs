//! Structure-aware HEIF (HEIC) carving. AVIF uses the same container but is not
//! recognized yet.
//!
//! A HEIF file starts with an `ftyp` whose brands name an HEVC image
//! (`heic`, `mif1`, ...); its top-level boxes (`meta`, `mdat`, ...) are walked by their sizes. The
//! item locations in `meta`'s `iloc` are then checked against the data: every item stored
//! in the file must lie inside it, and HEVC items must start with a plausible
//! length-prefixed NAL unit. The first item that fails is reported as a likely fragment
//! boundary.
//!
//! Reference: ISO/IEC 23008-12 (HEIF, brands 10.2, items and `grid`), ISO/IEC 14496-12
//! (`ftyp` 4.3, `meta` 8.11.1, `iloc` 8.11.3, `iinf`/`infe` 8.11.6), ISO/IEC 23008-2 7.3.1.2
//! (HEVC NAL unit header).

use blocksource::BlockSource;

use crate::reader::Reader;

/// A HEIF file found at some offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heif {
    pub len: u64,
    pub status: Status,
    /// `*b"heic"`, `*b"mif1"`, ...: the brand that identified the file.
    pub brand: [u8; 4],
    /// Items stored in the file whose placement was checked.
    pub items_checked: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `meta` and `mdat` present and every stored item checked out.
    Complete,
    /// An item does not fit or does not look like its data, at this offset.
    Corrupt { at: u64 },
    /// No `meta`/`mdat`, or the data ran out.
    Truncated,
}

/// Brands of HEVC-coded HEIF images (HEIC). `mif1` alone could also be AVIF, so files
/// whose major brand is `avif` are left out.
const IMAGE_BRANDS: &[&[u8; 4]] = &[b"heic", b"heix", b"heim", b"heis", b"mif1"];

/// Top-level boxes of a HEIF file.
const TOP_LEVEL: &[&[u8; 4]] = &[
    b"ftyp", b"meta", b"mdat", b"free", b"skip", b"idat", b"moov", b"uuid",
];

/// Whether the `ftyp` at `start` names a HEIF image (major or compatible brand).
pub fn is_heif<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64) -> Option<[u8; 4]> {
    let size = u64::from(r.u32_be(start)?);
    if r.slice(start + 4, 4)? != b"ftyp" || !(16..=512).contains(&size) {
        return None;
    }
    let major: [u8; 4] = r.slice(start + 8, 4)?.try_into().ok()?;
    if matches!(&major, b"avif" | b"avis") {
        return None;
    }
    if IMAGE_BRANDS.contains(&&major) {
        return Some(major);
    }
    // Compatible brands follow the 4-byte minor version.
    let compat = r.slice(start + 16, usize::try_from(size - 16).ok()?)?;
    compat
        .chunks_exact(4)
        .filter_map(|c| <[u8; 4]>::try_from(c).ok())
        .find(|b| IMAGE_BRANDS.contains(&b))
}

/// Tries to carve a HEIF file at `start`. `None` if there is no HEIF `ftyp` there.
pub fn carve<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Heif> {
    let brand = is_heif(r, start)?;
    let limit = start.saturating_add(max_len).min(r.len());
    let mut pos = start;
    let mut meta = None;
    let mut mdat = false;
    let mut truncated = false;
    while pos + 8 <= limit {
        let (Some(size32), Some(ty)) = (r.u32_be(pos), r.slice(pos + 4, 4)) else {
            break;
        };
        let Ok(ty) = <[u8; 4]>::try_from(ty) else {
            break;
        };
        if !TOP_LEVEL.contains(&&ty) || (&ty == b"ftyp" && pos != start) {
            break;
        }
        let (size, header) = match size32 {
            1 => match r.u64_be(pos + 8) {
                Some(s) => (s, 16),
                None => break,
            },
            0 => {
                truncated = true;
                (limit - pos, 8)
            }
            s => (u64::from(s), 8),
        };
        if size < header {
            break;
        }
        let end = pos.saturating_add(size);
        if end > limit {
            truncated = true;
        }
        match &ty {
            // `meta` is a full box: 4 bytes of version and flags before its children.
            b"meta" => meta = Some((pos + header + 4, end.min(limit))),
            b"mdat" => mdat = true,
            _ => {}
        }
        pos = end.min(limit);
        if truncated {
            break;
        }
    }

    let len = pos - start;
    let mut out = Heif {
        len,
        status: Status::Truncated,
        brand,
        items_checked: 0,
    };
    let Some(meta) = meta else { return Some(out) };
    if !mdat && !truncated {
        // Everything may live in `idat`; without `mdat` there is nothing else to check.
    }
    let types = item_types(r, meta.0, meta.1);
    let Some(locations) = item_locations(r, meta.0, meta.1) else {
        return Some(out);
    };
    let mut first_bad: Option<u64> = None;
    for loc in &locations {
        if loc.method != 0 {
            continue;
        }
        let hevc = types
            .iter()
            .any(|(id, t)| *id == loc.id && (t == b"hvc1" || t == b"hev1"));
        for &(offset, length) in &loc.extents {
            out.items_checked += 1;
            // Offsets in `iloc` are from the start of the file.
            let fits = length > 0 && offset.saturating_add(length) <= len;
            let abs = start.saturating_add(offset);
            if !fits || (hevc && !nal_ok(r, abs, length)) {
                first_bad = Some(first_bad.map_or(abs, |b| b.min(abs)));
            }
        }
    }
    out.status = match (first_bad, truncated, mdat) {
        (Some(at), _, _) => Status::Corrupt { at },
        (None, true, _) | (None, _, false) => Status::Truncated,
        (None, false, true) => Status::Complete,
    };
    Some(out)
}

/// One item's location: id, construction method (0 = file offset, 1 = `idat`, 2 = item),
/// and its extents as (offset, length).
struct Location {
    id: u32,
    method: u8,
    extents: Vec<(u64, u64)>,
}

/// Parses `iloc` (versions 0-2) among the children of `meta` (payload range `[from, to)`).
fn item_locations<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    from: u64,
    to: u64,
) -> Option<Vec<Location>> {
    let (b, e) = child(r, from, to, b"iloc")?;
    let version = r.byte(b)?;
    let mut p = b + 4;
    let sizes = r.byte(p)?;
    let sizes2 = r.byte(p + 1)?;
    p += 2;
    let (offset_size, length_size) = (u32::from(sizes >> 4), u32::from(sizes & 0x0F));
    let base_size = u32::from(sizes2 >> 4);
    let index_size = if version >= 1 {
        u32::from(sizes2 & 0x0F)
    } else {
        0
    };
    let count = if version < 2 {
        let c = u32::from(r.u16_be(p)?);
        p += 2;
        c
    } else {
        let c = r.u32_be(p)?;
        p += 4;
        c
    };
    let mut read = |p: &mut u64, n: u32| -> Option<u64> {
        let mut v = 0u64;
        for _ in 0..n {
            v = (v << 8) | u64::from(r.byte(*p)?);
            *p += 1;
        }
        Some(v)
    };
    let mut out = Vec::new();
    for _ in 0..count.min(65_535) {
        if p >= e {
            break;
        }
        let id = u32::try_from(read(&mut p, if version < 2 { 2 } else { 4 })?).ok()?;
        let method = if version >= 1 {
            u8::try_from(read(&mut p, 2)? & 0x0F).ok()?
        } else {
            0
        };
        read(&mut p, 2)?; // data reference index
        let base = read(&mut p, base_size)?;
        let extents = read(&mut p, 2)?;
        let mut list = Vec::new();
        for _ in 0..extents.min(1024) {
            read(&mut p, index_size)?;
            let offset = read(&mut p, offset_size)?;
            let length = read(&mut p, length_size)?;
            list.push((base.saturating_add(offset), length));
        }
        out.push(Location {
            id,
            method,
            extents: list,
        });
    }
    Some(out)
}

/// Item types from `iinf` (`infe` versions 2 and 3).
fn item_types<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    from: u64,
    to: u64,
) -> Vec<(u32, [u8; 4])> {
    let Some((b, e)) = child(r, from, to, b"iinf") else {
        return Vec::new();
    };
    let version = r.byte(b).unwrap_or(0);
    let first = b + 4 + if version == 0 { 2 } else { 4 };
    let mut out = Vec::new();
    let mut pos = first;
    while pos + 8 <= e {
        let (Some(size), Some(ty)) = (r.u32_be(pos), r.slice(pos + 4, 4)) else {
            break;
        };
        let size = u64::from(size);
        if size < 8 || pos + size > e {
            break;
        }
        if ty == b"infe" {
            let v = r.byte(pos + 8).unwrap_or(0);
            let body = pos + 12;
            let (id, ty_at) = if v >= 3 {
                (r.u32_be(body), body + 6)
            } else {
                (r.u16_be(body).map(u32::from), body + 4)
            };
            if let (Some(id), Some(t)) = (
                id,
                r.slice(ty_at, 4).and_then(|s| <[u8; 4]>::try_from(s).ok()),
            ) {
                out.push((id, t));
            }
        }
        pos += size;
    }
    out
}

/// The payload range of the first child box of type `want` in `[from, to)`.
fn child<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    from: u64,
    to: u64,
    want: &[u8; 4],
) -> Option<(u64, u64)> {
    let mut pos = from;
    while pos + 8 <= to {
        let size = u64::from(r.u32_be(pos)?);
        let ty = r.slice(pos + 4, 4)?;
        if size < 8 || pos + size > to {
            return None;
        }
        if ty == want {
            return Some((pos + 8, pos + size));
        }
        pos += size;
    }
    None
}

/// An HEVC item starts with a 4-byte NAL length that fits the item, followed by a NAL
/// header whose forbidden_zero_bit is 0.
fn nal_ok<S: BlockSource + ?Sized>(r: &mut Reader<S>, at: u64, len: u64) -> bool {
    match (r.u32_be(at), r.byte(at + 4)) {
        (Some(n), Some(header)) => n > 0 && u64::from(n) + 4 <= len.max(5) && header & 0x80 == 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use blocksource::MemSource;

    use super::*;

    fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = u32::try_from(8 + payload.len())
            .unwrap()
            .to_be_bytes()
            .to_vec();
        v.extend_from_slice(ty);
        v.extend_from_slice(payload);
        v
    }

    /// ftyp, meta (iinf with one hvc1 item, iloc v0 pointing into mdat), mdat.
    fn heic(break_item: bool) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        let mut item = 60u32.to_be_bytes().to_vec();
        item.extend([0x26, 0x01]);
        item.extend([0x33; 58]);
        if break_item {
            item[0] = 0xFF;
        }
        let infe = bx(
            b"infe",
            &[
                &[2, 0, 0, 0][..],
                &1u16.to_be_bytes(),
                &[0, 0],
                b"hvc1",
                b"\0",
            ]
            .concat(),
        );
        let iinf = bx(
            b"iinf",
            &[&[0, 0, 0, 0][..], &1u16.to_be_bytes(), &infe].concat(),
        );
        // iloc v0: 4-byte offset and length, no base; one item, one extent.
        let iloc_len = 8 + 4 + 2 + 2 + 2 + 2 + 2 + 8;
        let meta_len = 8 + 4 + iinf.len() + iloc_len;
        let data_at = u32::try_from(ftyp.len() + meta_len + 8).unwrap();
        let iloc = bx(
            b"iloc",
            &[
                &[0, 0, 0, 0, 0x44, 0x00][..],
                &1u16.to_be_bytes(),
                &1u16.to_be_bytes(),
                &0u16.to_be_bytes(),
                &1u16.to_be_bytes(),
                &data_at.to_be_bytes(),
                &64u32.to_be_bytes(),
            ]
            .concat(),
        );
        let meta = bx(b"meta", &[&[0, 0, 0, 0][..], &iinf, &iloc].concat());
        [ftyp, meta, bx(b"mdat", &item)].concat()
    }

    fn run(data: Vec<u8>) -> Option<Heif> {
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        carve(&mut r, 0, 1 << 30)
    }

    #[test]
    fn complete_heic() {
        let h = heic(false);
        let got = run([h.clone(), vec![0u8; 100]].concat()).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(got.len, h.len() as u64);
        assert_eq!(got.items_checked, 1);
        assert_eq!(&got.brand, b"heic");
    }

    #[test]
    fn broken_item_is_reported() {
        assert!(matches!(
            run(heic(true)).unwrap().status,
            Status::Corrupt { .. }
        ));
    }

    #[test]
    fn mp4_is_not_heif() {
        let mut v = bx(b"ftyp", b"isom\0\0\0\0isommp42");
        v.extend(bx(b"mdat", &[0; 32]));
        assert!(run(v).is_none());
    }
}
