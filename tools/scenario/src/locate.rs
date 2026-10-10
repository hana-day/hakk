//! ファイルの中身をイメージと照合して、ディスク上の配置を特定する。
//!
//! OS やファイルシステム解析に頼らず、バイト列だけから正解の配置を求める。
//! イメージを 512 バイト単位で索引化し、ファイルを 512 バイトごとに区切って探す。
//! 同じ中身のブロックが複数ある場合は、前後のブロックとの連続性で決める。

use std::collections::HashMap;

use serde::Serialize;

pub const BLOCK: usize = 512;

/// ファイルの一部がイメージのどこにあるか。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Extent {
    pub file_offset: u64,
    pub image_offset: u64,
    pub len: u64,
}

/// イメージの 512 バイト境界のブロックの索引。
pub struct Index<'a> {
    image: &'a [u8],
    map: HashMap<u64, Vec<u64>>,
}

impl<'a> Index<'a> {
    pub fn new(image: &'a [u8]) -> Self {
        let mut map: HashMap<u64, Vec<u64>> = HashMap::new();
        for (i, block) in image.chunks_exact(BLOCK).enumerate() {
            // ゼロ埋めのブロックは大量にあり、生成するファイルには現れないので省く
            if block.iter().all(|&b| b == 0) {
                continue;
            }
            map.entry(hash(block)).or_default().push((i * BLOCK) as u64);
        }
        Self { image, map }
    }

    fn candidates(&self, block: &[u8]) -> Vec<u64> {
        self.map
            .get(&hash(block))
            .into_iter()
            .flatten()
            .copied()
            .filter(|&off| self.block_at(off, block.len()) == Some(block))
            .collect()
    }

    fn block_at(&self, off: u64, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(off).ok()?;
        self.image.get(start..start.checked_add(len)?)
    }

    /// ファイル `data` の配置を求める。一部でも特定できなければ `None`。
    pub fn locate(&self, data: &[u8]) -> Option<Vec<Extent>> {
        let full_blocks = data.len() / BLOCK;
        // 各ブロックの候補位置
        let cands: Vec<Vec<u64>> = data
            .chunks_exact(BLOCK)
            .map(|b| self.candidates(b))
            .collect();
        let mut pos: Vec<Option<u64>> = cands
            .iter()
            .map(|c| {
                if c.len() == 1 {
                    c.first().copied()
                } else {
                    None
                }
            })
            .collect();

        // 一意に決まらないブロックは、隣が決まっていれば連続性で決める。前後から繰り返す
        loop {
            let mut changed = false;
            for i in 0..full_blocks {
                if pos.get(i).copied().flatten().is_some() {
                    continue;
                }
                let prev = i.checked_sub(1).and_then(|p| pos.get(p).copied().flatten());
                let next = pos.get(i + 1).copied().flatten();
                let want = [
                    prev.map(|p| p + BLOCK as u64),
                    next.and_then(|n| n.checked_sub(BLOCK as u64)),
                ];
                let c = cands.get(i).map(Vec::as_slice).unwrap_or_default();
                if let Some(found) = want.into_iter().flatten().find(|w| c.contains(w))
                    && let Some(slot) = pos.get_mut(i)
                {
                    *slot = Some(found);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        let mut extents: Vec<Extent> = Vec::new();
        for (i, p) in pos.iter().enumerate() {
            push(&mut extents, (i * BLOCK) as u64, (*p)?, BLOCK as u64);
        }

        // 端数：直前のブロックの続きにあるとみなして確かめる（クラスタは 512 より大きい）
        let tail = data.get(full_blocks * BLOCK..).unwrap_or_default();
        if !tail.is_empty() {
            // 512 バイト未満のファイルは扱わない（生成側で 4KiB 以上にしている）
            let at = extents.last().map(|e| e.image_offset + e.len)?;
            if self.block_at(at, tail.len()) != Some(tail) {
                return None;
            }
            push(
                &mut extents,
                (full_blocks * BLOCK) as u64,
                at,
                tail.len() as u64,
            );
        }
        Some(extents)
    }
}

/// 連続していれば直前の範囲を伸ばし、そうでなければ新しい範囲を足す。
fn push(extents: &mut Vec<Extent>, file_offset: u64, image_offset: u64, len: u64) {
    if let Some(last) = extents.last_mut()
        && last.file_offset + last.len == file_offset
        && last.image_offset + last.len == image_offset
    {
        last.len += len;
        return;
    }
    extents.push(Extent {
        file_offset,
        image_offset,
        len,
    });
}

/// 配置 `extents` のうち、最終イメージ上で元の中身のまま残っているバイト数（512 バイト単位で判定）。
pub fn intact_bytes(image: &[u8], data: &[u8], extents: &[Extent]) -> u64 {
    let mut intact = 0;
    for e in extents {
        let (Ok(fo), Ok(io), Ok(len)) = (
            usize::try_from(e.file_offset),
            usize::try_from(e.image_offset),
            usize::try_from(e.len),
        ) else {
            continue;
        };
        let (Some(want), Some(got)) = (data.get(fo..fo + len), image.get(io..io + len)) else {
            continue;
        };
        for (w, g) in want.chunks(BLOCK).zip(got.chunks(BLOCK)) {
            if w == g {
                intact += w.len() as u64;
            }
        }
    }
    intact
}

/// FNV-1a を 8 バイト単位にしたもの。索引用なので衝突は中身の比較で弾く。
fn hash(block: &[u8]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for chunk in block.chunks(8) {
        let mut w = [0u8; 8];
        for (d, s) in w.iter_mut().zip(chunk) {
            *d = *s;
        }
        h = (h ^ u64::from_le_bytes(w)).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::Rng;

    fn image_with(files: &[(&[u8], &[u64])], len: usize) -> Vec<u8> {
        let mut img = vec![0u8; len];
        for (data, places) in files {
            // places[i] は i 番目の 4KiB 断片の置き場所
            for (chunk, &at) in data.chunks(4096).zip(places.iter()) {
                let at = at as usize;
                img[at..at + chunk.len()].copy_from_slice(chunk);
            }
        }
        img
    }

    #[test]
    fn contiguous_file() {
        let data = Rng::new(1).bytes(10_000);
        let img = image_with(&[(&data, &[8192, 12288, 16384])], 64 * 1024);
        let ext = Index::new(&img).locate(&data).unwrap();
        assert_eq!(
            ext,
            [Extent {
                file_offset: 0,
                image_offset: 8192,
                len: 10_000
            }]
        );
    }

    #[test]
    fn fragmented_file() {
        let data = Rng::new(2).bytes(12_288);
        let img = image_with(&[(&data, &[40960, 4096, 20480])], 64 * 1024);
        let ext = Index::new(&img).locate(&data).unwrap();
        assert_eq!(ext.len(), 3);
        assert_eq!(
            ext[1],
            Extent {
                file_offset: 4096,
                image_offset: 4096,
                len: 4096
            }
        );
    }

    /// 2つのファイルの先頭が同じ（例：同じ量子化表）でも、続きとの連続性で区別できる
    #[test]
    fn duplicate_blocks_resolved_by_neighbors() {
        let mut a = vec![0x55u8; 1024];
        a.extend(Rng::new(3).bytes(7168));
        let mut b = vec![0x55u8; 1024];
        b.extend(Rng::new(4).bytes(7168));
        let img = image_with(&[(&a, &[4096, 8192]), (&b, &[20480, 24576])], 64 * 1024);
        let idx = Index::new(&img);
        assert_eq!(idx.locate(&a).unwrap()[0].image_offset, 4096);
        assert_eq!(idx.locate(&b).unwrap()[0].image_offset, 20480);
    }

    #[test]
    fn missing_file_is_none() {
        let data = Rng::new(5).bytes(8192);
        let img = vec![0u8; 64 * 1024];
        assert!(Index::new(&img).locate(&data).is_none());
    }

    #[test]
    fn intact_counts_overwritten_blocks() {
        let data = Rng::new(6).bytes(8192);
        let mut img = image_with(&[(&data, &[4096, 8192])], 32 * 1024);
        let ext = Index::new(&img).locate(&data).unwrap();
        assert_eq!(intact_bytes(&img, &data, &ext), 8192);
        img[5000] ^= 0xFF; // 1ブロックだけ壊す
        assert_eq!(intact_bytes(&img, &data, &ext), 8192 - 512);
    }
}
