//! Builds a minimal EXIF (APP1) segment.
//!
//! Reference: CIPA DC-008 (Exif 2.3), TIFF Revision 6.0.

use serde::Serialize;

use crate::rng::Rng;

/// Capture info: the expected answer for naming recovered files
/// (e.g. `2025-08-14_153022_ILCE-7M4.jpg`).
#[derive(Debug, Clone, Serialize)]
pub struct Exif {
    pub make: String,
    pub model: String,
    /// "YYYY:MM:DD HH:MM:SS"
    pub date_time_original: String,
}

const CAMERAS: &[(&str, &str)] = &[
    ("Canon", "Canon EOS R6"),
    ("SONY", "ILCE-7M4"),
    ("NIKON CORPORATION", "NIKON Z 6_2"),
    ("FUJIFILM", "X-T5"),
    ("Apple", "iPhone 15 Pro"),
];

impl Exif {
    pub fn random(rng: &mut Rng) -> Self {
        let (make, model) = CAMERAS
            .get(rng.index(CAMERAS.len()))
            .copied()
            .unwrap_or(("Canon", "Canon EOS R6"));
        let date_time_original = format!(
            "{:04}:{:02}:{:02} {:02}:{:02}:{:02}",
            rng.range(2018, 2026),
            rng.range(1, 12),
            rng.range(1, 28),
            rng.range(0, 23),
            rng.range(0, 59),
            rng.range(0, 59),
        );
        Self {
            make: make.to_owned(),
            model: model.to_owned(),
            date_time_original,
        }
    }

    /// Capture info for a given camera with a random capture time.
    pub fn for_camera(make: &str, model: &str, rng: &mut Rng) -> Self {
        let mut e = Self::random(rng);
        e.make = make.to_owned();
        e.model = model.to_owned();
        e
    }

    /// The whole APP1 segment (starting at the `FF E1` marker) with no thumbnail or padding.
    pub fn app1(&self) -> Vec<u8> {
        self.app1_with(None, 0, &mut Rng::new(0))
    }

    /// The whole APP1 segment (starting at the `FF E1` marker).
    ///
    /// `thumbnail` is embedded as a JPEG in IFD1, as cameras do. If the segment would be
    /// shorter than `target_len` (the APP1 length field), a MakerNote of random bytes pads it.
    pub fn app1_with(&self, thumbnail: Option<&[u8]>, target_len: usize, rng: &mut Rng) -> Vec<u8> {
        // TIFF header (little endian). All offsets count from the start of the TIFF header.
        let make = ascii(&self.make);
        let model = ascii(&self.model);
        let date = ascii(&self.date_time_original);
        let thumb = thumbnail.unwrap_or_default();

        let ifd0: u32 = 8;
        let ifd0_entries: u16 = 3;
        let exif_ifd = ifd0 + ifd_len(ifd0_entries);
        let exif_entries: u16 = 2; // DateTimeOriginal, MakerNote
        let ifd1 = exif_ifd + ifd_len(exif_entries);
        let ifd1_entries: u16 = if thumb.is_empty() { 0 } else { 3 };
        let data = if thumb.is_empty() {
            ifd1
        } else {
            ifd1 + ifd_len(ifd1_entries)
        };
        let make_at = data;
        let model_at = make_at + len32(&make);
        let date_at = model_at + len32(&model);
        let note_at = date_at + len32(&date);
        // APP1 length field = 2 (itself) + 6 ("Exif\0\0") + TIFF.
        let fixed = 2 + 6 + note_at as usize + thumb.len();
        let note = super::jpeg::no_markers(rng.bytes(target_len.saturating_sub(fixed).max(4)));
        let thumb_at = note_at + len32(&note);

        let mut t = Vec::new();
        t.extend_from_slice(b"II\x2A\x00");
        t.extend_from_slice(&ifd0.to_le_bytes());

        // IFD0, tags in ascending order. The next-IFD link points at IFD1 (the thumbnail).
        t.extend_from_slice(&ifd0_entries.to_le_bytes());
        entry(&mut t, 0x010F, ASCII, len32(&make), make_at); // Make
        entry(&mut t, 0x0110, ASCII, len32(&model), model_at); // Model
        entry(&mut t, 0x8769, LONG, 1, exif_ifd); // ExifIFDPointer
        let next = if thumb.is_empty() { 0 } else { ifd1 };
        t.extend_from_slice(&next.to_le_bytes());

        // Exif IFD
        t.extend_from_slice(&exif_entries.to_le_bytes());
        entry(&mut t, 0x9003, ASCII, len32(&date), date_at); // DateTimeOriginal
        entry(&mut t, 0x927C, UNDEFINED, len32(&note), note_at); // MakerNote
        t.extend_from_slice(&0u32.to_le_bytes());

        // IFD1: the thumbnail (Compression = 6, JPEG).
        if !thumb.is_empty() {
            t.extend_from_slice(&ifd1_entries.to_le_bytes());
            entry(&mut t, 0x0103, SHORT, 1, 6); // Compression
            entry(&mut t, 0x0201, LONG, 1, thumb_at); // JPEGInterchangeFormat
            entry(&mut t, 0x0202, LONG, 1, len32(thumb)); // JPEGInterchangeFormatLength
            t.extend_from_slice(&0u32.to_le_bytes());
        }

        t.extend_from_slice(&make);
        t.extend_from_slice(&model);
        t.extend_from_slice(&date);
        t.extend_from_slice(&note);
        t.extend_from_slice(thumb);

        let mut seg = vec![0xFF, 0xE1];
        let len = u16::try_from(2 + 6 + t.len()).unwrap_or(u16::MAX);
        seg.extend_from_slice(&len.to_be_bytes());
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend_from_slice(&t);
        seg
    }
}

const ASCII: u16 = 2;
const SHORT: u16 = 3;
const LONG: u16 = 4;
const UNDEFINED: u16 = 7;

fn ascii(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

fn len32(v: &[u8]) -> u32 {
    u32::try_from(v.len()).unwrap_or(u32::MAX)
}

/// Size of an IFD with `n` entries (2 for the count + 12n + 4 for the next IFD).
const fn ifd_len(n: u16) -> u32 {
    2 + 12 * n as u32 + 4
}

/// Every value here is longer than 4 bytes, a single LONG or a single SHORT, so the value
/// field holds an offset or the value itself (a SHORT sits in the low bytes, little endian).
fn entry(t: &mut Vec<u8>, tag: u16, ty: u16, count: u32, value: u32) {
    t.extend_from_slice(&tag.to_le_bytes());
    t.extend_from_slice(&ty.to_le_bytes());
    t.extend_from_slice(&count.to_le_bytes());
    t.extend_from_slice(&value.to_le_bytes());
}
