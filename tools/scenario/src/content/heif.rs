//! Photo (HEIC) generation, shaped like the files iPhones and iPads write.
//!
//! `ftyp` (heic), then `meta` (handler, primary item, item infos, references, properties,
//! the grid descriptor in `idat`, item locations), then `mdat` with the EXIF item, the
//! thumbnail and the image tiles. The picture is a grid of 512x512 HEVC tiles; tile and
//! thumbnail data are length-prefixed NAL units filled with random bytes, so the structure
//! is valid but nothing decodes.
//!
//! Reference: ISO/IEC 23008-12 (HEIF: `grid` 6.6.2.3, item properties `ispe` 6.5.3,
//! `hvcC` from ISO/IEC 14496-15 8.3.3), ISO/IEC 14496-12 (`meta` 8.11, `iloc` 8.11.3,
//! `iinf`/`infe` 8.11.6, `iref` 8.11.12, `iprp`/`ipco`/`ipma` 8.11.14).

use super::{Generated, Meta, exif::Exif, profile::HeifProfile};
use crate::rng::Rng;

/// Approximate bytes of HEVC data per pixel in a phone photo.
const BYTES_PER_PIXEL: f64 = 0.08;

pub fn generate_profiled(p: &HeifProfile, target: u64, rng: &mut Rng) -> Generated {
    let tile = p.tile.max(64);
    let [aw, ah] = p.aspect;
    let pixels = target as f64 / BYTES_PER_PIXEL;
    let height = ((pixels * f64::from(ah) / f64::from(aw)).sqrt() as u32).max(tile);
    let width = (height * aw / ah).max(tile);
    let (cols, rows) = (width.div_ceil(tile), height.div_ceil(tile));
    let tiles = cols * rows;

    // Item ids: tiles 1..=n, then the grid (primary), the thumbnail, the EXIF item.
    let grid_id = tiles + 1;
    let thumb_id = tiles + 2;
    let exif_id = tiles + 3;

    let exif = Exif::for_camera(&p.make, &p.model, rng);
    let exif_len = rng.range(p.exif_len[0], p.exif_len[1]) as usize;
    let app1 = exif.app1_with(None, exif_len, rng);
    // A HEIF EXIF item: a 4-byte offset to the TIFF header, then "Exif\0\0" and TIFF.
    let mut exif_item = 6u32.to_be_bytes().to_vec();
    exif_item.extend_from_slice(app1.get(4..).unwrap_or_default());

    let thumb = nal_units(rng.range(p.thumbnail[0], p.thumbnail[1]), rng);
    let per_tile = (target.saturating_sub(thumb.len() as u64 + exif_item.len() as u64)
        / u64::from(tiles).max(1))
    .max(512);
    let tile_data: Vec<Vec<u8>> = (0..tiles)
        .map(|_| nal_units(rng.range(per_tile * 3 / 4, per_tile * 5 / 4), rng))
        .collect();

    // mdat payload order, as on the device: EXIF, thumbnail, tiles.
    let mut payload: Vec<(u32, &[u8])> = vec![(exif_id, &exif_item), (thumb_id, &thumb)];
    payload.extend((1..=tiles).zip(tile_data.iter().map(Vec::as_slice)));

    let ftyp = bx(
        b"ftyp",
        &cat(&[b"heic", &0u32.to_be_bytes(), b"mif1", b"heic"]),
    );
    // `iloc` entries have fixed widths, so meta's size does not depend on the offsets.
    let meta_len = meta(
        &Layout::sizes(&payload),
        0,
        width,
        height,
        cols,
        rows,
        grid_id,
        thumb_id,
        exif_id,
        tiles,
        rng.fork(),
    )
    .len();
    let data_start = (ftyp.len() + meta_len + 8) as u64;
    let layout = Layout::place(&payload, data_start);
    let meta = meta(
        &layout,
        0,
        width,
        height,
        cols,
        rows,
        grid_id,
        thumb_id,
        exif_id,
        tiles,
        rng.fork(),
    );

    let mut mdat = Vec::new();
    for (_, data) in &payload {
        mdat.extend_from_slice(data);
    }
    let mut out = ftyp;
    out.extend(meta);
    out.extend(bx(b"mdat", &mdat));
    Generated {
        bytes: out,
        meta: Meta {
            exif: Some(exif),
            width: Some(width),
            height: Some(height),
            ..Meta::default()
        },
    }
}

/// Where each item's data lies: (item id, absolute offset, length).
struct Layout(Vec<(u32, u64, u64)>);

impl Layout {
    fn sizes(payload: &[(u32, &[u8])]) -> Self {
        Self(
            payload
                .iter()
                .map(|(id, d)| (*id, 0, d.len() as u64))
                .collect(),
        )
    }

    fn place(payload: &[(u32, &[u8])], mut at: u64) -> Self {
        let mut v = Vec::new();
        for (id, d) in payload {
            v.push((*id, at, d.len() as u64));
            at += d.len() as u64;
        }
        Self(v)
    }
}

#[allow(clippy::too_many_arguments)]
fn meta(
    layout: &Layout,
    _base: u64,
    width: u32,
    height: u32,
    cols: u32,
    rows: u32,
    grid_id: u32,
    thumb_id: u32,
    exif_id: u32,
    tiles: u32,
    mut rng: Rng,
) -> Vec<u8> {
    let id16 = |id: u32| u16::try_from(id).unwrap_or(u16::MAX).to_be_bytes();
    let hdlr = full(b"hdlr", 0, 0, &cat(&[&[0; 4], b"pict", &[0; 12], b"\0"]));
    let dref = full(
        b"dref",
        0,
        0,
        &cat(&[&1u32.to_be_bytes(), &full(b"url ", 0, 1, &[])]),
    );
    let dinf = bx(b"dinf", &dref);
    let pitm = full(b"pitm", 0, 0, &id16(grid_id));

    // Item infos (infe version 2): tiles are hidden (flags = 1).
    let mut infes = Vec::new();
    for id in 1..=tiles {
        infes.push(full(
            b"infe",
            2,
            1,
            &cat(&[&id16(id), &[0, 0], b"hvc1", b"\0"]),
        ));
    }
    infes.push(full(
        b"infe",
        2,
        0,
        &cat(&[&id16(grid_id), &[0, 0], b"grid", b"\0"]),
    ));
    infes.push(full(
        b"infe",
        2,
        1,
        &cat(&[&id16(thumb_id), &[0, 0], b"hvc1", b"\0"]),
    ));
    infes.push(full(
        b"infe",
        2,
        1,
        &cat(&[&id16(exif_id), &[0, 0], b"Exif", b"\0"]),
    ));
    let count = u16::try_from(infes.len()).unwrap_or(u16::MAX).to_be_bytes();
    let iinf = full(b"iinf", 0, 0, &cat(&[&count, &infes.concat()]));

    // References: the grid is made of the tiles; the thumbnail and EXIF describe the grid.
    let mut dimg = cat(&[
        &id16(grid_id),
        &u16::try_from(tiles).unwrap_or(0).to_be_bytes(),
    ]);
    for id in 1..=tiles {
        dimg.extend(id16(id));
    }
    let iref = full(
        b"iref",
        0,
        0,
        &cat(&[
            &bx(b"dimg", &dimg),
            &bx(
                b"thmb",
                &cat(&[&id16(thumb_id), &1u16.to_be_bytes(), &id16(grid_id)]),
            ),
            &bx(
                b"cdsc",
                &cat(&[&id16(exif_id), &1u16.to_be_bytes(), &id16(grid_id)]),
            ),
        ]),
    );

    // Properties: 1 hvcC (tiles), 2 ispe (tile), 3 ispe (whole), 4 hvcC (thumb), 5 ispe (thumb).
    let tile = width.div_ceil(cols);
    let ipco = bx(
        b"ipco",
        &cat(&[
            &hvcc(&mut rng),
            &ispe(tile, height.div_ceil(rows)),
            &ispe(width, height),
            &hvcc(&mut rng),
            &ispe(320, 240),
        ]),
    );
    // Associations: index | 0x80 marks a property as essential.
    let mut ipma = (tiles + 2).to_be_bytes().to_vec();
    for id in 1..=tiles {
        ipma.extend(cat(&[&id16(id), &[2, 0x81, 2]]));
    }
    ipma.extend(cat(&[&id16(grid_id), &[1, 3]]));
    ipma.extend(cat(&[&id16(thumb_id), &[2, 0x84, 5]]));
    let iprp = bx(b"iprp", &cat(&[&ipco, &full(b"ipma", 0, 0, &ipma)]));

    // The grid descriptor (version 0, 16-bit output size) lives in idat.
    let grid = cat(&[
        &[0, 0],
        &[
            u8::try_from(rows - 1).unwrap_or(0),
            u8::try_from(cols - 1).unwrap_or(0),
        ],
        &u16::try_from(width).unwrap_or(u16::MAX).to_be_bytes(),
        &u16::try_from(height).unwrap_or(u16::MAX).to_be_bytes(),
    ]);
    let idat = bx(b"idat", &grid);

    // Item locations (version 1, 4-byte offsets and lengths, no base offset).
    let mut iloc = vec![0x44, 0x00];
    iloc.extend(u16::try_from(layout.0.len() + 1).unwrap_or(0).to_be_bytes());
    let entry = |id: u32, method: u16, offset: u64, len: u64| {
        cat(&[
            &id16(id),
            &method.to_be_bytes(),
            &0u16.to_be_bytes(), // data reference index
            &1u16.to_be_bytes(), // extent count
            &u32::try_from(offset).unwrap_or(u32::MAX).to_be_bytes(),
            &u32::try_from(len).unwrap_or(u32::MAX).to_be_bytes(),
        ])
    };
    iloc.extend(entry(grid_id, 1, 0, grid.len() as u64)); // construction method 1: idat
    for &(id, offset, len) in &layout.0 {
        iloc.extend(entry(id, 0, offset, len));
    }
    let iloc = full(b"iloc", 1, 0, &iloc);

    full(
        b"meta",
        0,
        0,
        &cat(&[&hdlr, &dinf, &pitm, &iinf, &iref, &iprp, &idat, &iloc]),
    )
}

/// An `hvcC` box: a decoder configuration record with placeholder parameter sets.
fn hvcc(rng: &mut Rng) -> Vec<u8> {
    let mut v = vec![
        1, 0x01, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 0x5A, 0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0,
        0x0F, 3,
    ];
    // VPS, SPS, PPS arrays with one small NAL unit each.
    for nal_type in [32u8, 33, 34] {
        let mut nal = vec![nal_type << 1, 1];
        nal.extend(rng.bytes(12));
        v.push(0x80 | nal_type);
        v.extend(1u16.to_be_bytes());
        v.extend(u16::try_from(nal.len()).unwrap_or(0).to_be_bytes());
        v.extend(nal);
    }
    bx(b"hvcC", &v)
}

fn ispe(width: u32, height: u32) -> Vec<u8> {
    full(
        b"ispe",
        0,
        0,
        &cat(&[&width.to_be_bytes(), &height.to_be_bytes()]),
    )
}

/// About `len` bytes of length-prefixed HEVC NAL units (an IDR slice) of random content.
fn nal_units(len: u64, rng: &mut Rng) -> Vec<u8> {
    let body = usize::try_from(len.max(16)).unwrap_or(16) - 4;
    let mut v = u32::try_from(body).unwrap_or(0).to_be_bytes().to_vec();
    v.extend([0x26, 0x01]); // NAL header: IDR_W_RADL, layer 0, temporal id 1
    v.extend(rng.bytes(body - 2));
    v
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn bx(ty: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(8 + payload.len()).unwrap_or(u32::MAX);
    cat(&[&size.to_be_bytes(), ty, payload])
}

fn full(ty: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let vf = (u32::from(version) << 24) | (flags & 0x00FF_FFFF);
    bx(ty, &cat(&[&vf.to_be_bytes(), payload]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipad() -> HeifProfile {
        HeifProfile {
            make: "Apple".into(),
            model: "iPad (7th generation)".into(),
            dir: "DCIM/100APPLE".into(),
            name: "IMG_{n}.HEIC".into(),
            aspect: [4, 3],
            tile: 512,
            exif_len: [2100, 2200],
            thumbnail: [7000, 8000],
        }
    }

    #[test]
    fn layout_and_item_locations() {
        let g = generate_profiled(&ipad(), 650_000, &mut Rng::new(1));
        let b = &g.bytes;
        // Top level: ftyp, meta, mdat, covering the whole file.
        let mut pos = 0usize;
        let mut types = Vec::new();
        while pos < b.len() {
            let size = u32::from_be_bytes(b[pos..pos + 4].try_into().unwrap()) as usize;
            types.push(String::from_utf8_lossy(&b[pos + 4..pos + 8]).into_owned());
            pos += size;
        }
        assert_eq!(pos, b.len());
        assert_eq!(types, ["ftyp", "meta", "mdat"]);
        // The first tile's iloc entry points at a NAL unit inside mdat.
        let iloc = b.windows(4).position(|w| w == b"iloc").unwrap() - 4;
        let first = iloc + 8 + 4 + 2 + 2 + 14; // header, v/f, sizes, count, idat entry
        let offset = u32::from_be_bytes(b[first + 8..first + 12].try_into().unwrap()) as usize;
        assert!(offset < b.len());
        assert!(g.bytes.len() > 400_000);
    }
}
