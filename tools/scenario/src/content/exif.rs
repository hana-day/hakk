//! 最小限の EXIF（APP1）を作る。
//!
//! 参考：CIPA DC-008（Exif 2.3）、TIFF Revision 6.0。

use serde::Serialize;

use crate::rng::Rng;

/// 撮影情報。復元時のファイル名生成（例 `2025-08-14_153022_ILCE-7M4.jpg`）の正解になる。
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

    /// APP1 セグメント全体（マーカー `FF E1` から）。
    pub fn app1(&self) -> Vec<u8> {
        // TIFF ヘッダ（リトルエンディアン）。オフセットはすべて TIFF ヘッダの先頭から数える
        let make = ascii(&self.make);
        let model = ascii(&self.model);
        let date = ascii(&self.date_time_original);

        const IFD0: u32 = 8;
        const IFD0_ENTRIES: u16 = 3;
        let exif_ifd = IFD0 + ifd_len(IFD0_ENTRIES);
        const EXIF_ENTRIES: u16 = 1;
        let data = exif_ifd + ifd_len(EXIF_ENTRIES);
        let make_at = data;
        let model_at = make_at + len32(&make);
        let date_at = model_at + len32(&model);

        let mut t = Vec::new();
        t.extend_from_slice(b"II\x2A\x00");
        t.extend_from_slice(&IFD0.to_le_bytes());

        // IFD0：タグ番号の昇順
        t.extend_from_slice(&IFD0_ENTRIES.to_le_bytes());
        entry(&mut t, 0x010F, ASCII, len32(&make), make_at); // Make
        entry(&mut t, 0x0110, ASCII, len32(&model), model_at); // Model
        entry(&mut t, 0x8769, LONG, 1, exif_ifd); // ExifIFDPointer
        t.extend_from_slice(&0u32.to_le_bytes());

        // Exif IFD
        t.extend_from_slice(&EXIF_ENTRIES.to_le_bytes());
        entry(&mut t, 0x9003, ASCII, len32(&date), date_at); // DateTimeOriginal
        t.extend_from_slice(&0u32.to_le_bytes());

        t.extend_from_slice(&make);
        t.extend_from_slice(&model);
        t.extend_from_slice(&date);

        let mut seg = vec![0xFF, 0xE1];
        let len = u16::try_from(2 + 6 + t.len()).unwrap_or(u16::MAX);
        seg.extend_from_slice(&len.to_be_bytes());
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend_from_slice(&t);
        seg
    }
}

const ASCII: u16 = 2;
const LONG: u16 = 4;

fn ascii(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}

fn len32(v: &[u8]) -> u32 {
    u32::try_from(v.len()).unwrap_or(u32::MAX)
}

/// エントリ数 `n` の IFD の大きさ（件数2 + 12n + 次IFD4）。
const fn ifd_len(n: u16) -> u32 {
    2 + 12 * n as u32 + 4
}

/// 値はすべて4バイトを超えるか LONG 1個なので、値欄にはオフセット（または値）を入れる。
fn entry(t: &mut Vec<u8>, tag: u16, ty: u16, count: u32, value: u32) {
    t.extend_from_slice(&tag.to_le_bytes());
    t.extend_from_slice(&ty.to_le_bytes());
    t.extend_from_slice(&count.to_le_bytes());
    t.extend_from_slice(&value.to_le_bytes());
}
