//! Scenarios without a file system (`fs = "none"`): files are placed directly on the disk.
//!
//! Like the NIST CFReDS carving images, files are separated by filler and split into
//! fragments in different layouts (in order, shuffled, with a missing piece, nested,
//! braided). The placement is chosen here, so the ground truth is exact and the result is
//! the same on every OS.
//!
//! Reference: NIST CFReDS "Forensic Images for File Carving" (levels L0-L5).

use anyhow::{Result, bail};

use crate::{
    content::{self, Generated},
    locate::Extent,
    rng::Rng,
    scenario::{Fill, Placement, Scenario, Step},
};

/// A file placed on the disk.
pub struct Placed {
    pub step: usize,
    pub path: String,
    pub kind: crate::scenario::ContentKind,
    pub profile: Option<String>,
    pub generated: Generated,
    pub extents: Vec<Extent>,
}

/// A piece of a file: `(file index, offset in the file, length)`.
type Piece = (usize, u64, u64);

pub fn run(sc: &Scenario, rng: &mut Rng) -> Result<(Vec<u8>, Vec<Placed>)> {
    let size = usize::try_from(sc.size.0)?;
    let mut disk = vec![0u8; size];
    let mut cursor = 0u64;
    let mut placed: Vec<Placed> = Vec::new();

    for (step_no, step) in sc.steps.iter().enumerate() {
        let Step::Place {
            count,
            kind,
            min_size,
            max_size,
            profile,
            layout,
            fragments,
            gap,
            fill,
            align,
        } = step
        else {
            bail!("steps[{step_no}]: only `place` steps are allowed without a file system");
        };

        let first = placed.len();
        for _ in 0..*count {
            let n = u32::try_from(placed.len() + 1)?;
            let target = rng.range(min_size.0, max_size.0);
            let p = profile.as_deref();
            let generated = content::generate(*kind, p, &sc.profiles, target, &mut rng.fork())?;
            placed.push(Placed {
                step: step_no,
                path: content::path(*kind, p, &sc.profiles, None, n),
                kind: *kind,
                profile: profile.clone(),
                generated,
                extents: Vec::new(),
            });
        }

        let files: Vec<usize> = (first..placed.len()).collect();
        let len_of = |i: usize| placed.get(i).map_or(0, |p| p.generated.bytes.len() as u64);
        let order = arrange(&files, &len_of, *layout, *fragments, align.0, rng);

        for (file, offset, len) in order {
            let g = rng.range(gap[0].0, gap[1].0);
            filler(&mut disk, cursor, g, *fill, rng)?;
            cursor = (cursor + g).next_multiple_of(align.0);
            let Some(p) = placed.get_mut(file) else {
                continue;
            };
            let (Ok(src_start), Ok(dst_start), Ok(n)) = (
                usize::try_from(offset),
                usize::try_from(cursor),
                usize::try_from(len),
            ) else {
                bail!("placement overflow");
            };
            let (Some(src), Some(dst)) = (
                p.generated.bytes.get(src_start..src_start + n),
                disk.get_mut(dst_start..dst_start + n),
            ) else {
                bail!("the disk is full: increase `size` or reduce the files");
            };
            dst.copy_from_slice(src);
            p.extents.push(Extent {
                file_offset: offset,
                image_offset: cursor,
                len,
            });
            cursor += len;
        }
        let g = rng.range(gap[0].0, gap[1].0);
        filler(&mut disk, cursor, g, *fill, rng)?;
        cursor += g;
    }

    for p in &mut placed {
        p.extents.sort_by_key(|e| e.file_offset);
    }
    Ok((disk, placed))
}

/// Decides the order in which pieces of `files` go onto the disk.
fn arrange(
    files: &[usize],
    len_of: &dyn Fn(usize) -> u64,
    layout: Placement,
    fragments: [u32; 2],
    align: u64,
    rng: &mut Rng,
) -> Vec<Piece> {
    let split = |f: usize, rng: &mut Rng| {
        let k =
            u32::try_from(rng.range(u64::from(fragments[0]), u64::from(fragments[1]))).unwrap_or(2);
        cut(f, len_of(f), k, align, rng)
    };
    let mut order = Vec::new();
    match layout {
        Placement::Contiguous => {
            order.extend(files.iter().map(|&f| (f, 0, len_of(f))));
        }
        Placement::Sequential => {
            for &f in files {
                order.extend(split(f, rng));
            }
        }
        Placement::Shuffled => {
            for &f in files {
                let mut pieces = split(f, rng);
                // Any order but the original one.
                let original = pieces.clone();
                while pieces.len() > 1 && pieces == original {
                    rng.shuffle(&mut pieces);
                }
                order.extend(pieces);
            }
        }
        Placement::Missing => {
            for &f in files {
                let mut pieces = split(f, rng);
                let drop = rng.index(pieces.len());
                pieces.remove(drop);
                order.extend(pieces);
            }
        }
        Placement::Nested => {
            // Pairs (outer, inner): outer's first half, inner whole, outer's second half.
            for pair in files.chunks(2) {
                match pair {
                    [outer, inner] => {
                        let halves = cut(*outer, len_of(*outer), 2, align, rng);
                        let mut halves = halves.into_iter();
                        order.extend(halves.next());
                        order.push((*inner, 0, len_of(*inner)));
                        order.extend(halves);
                    }
                    [single] => order.push((*single, 0, len_of(*single))),
                    _ => {}
                }
            }
        }
        Placement::Braided => {
            // Pairs whose fragments alternate: a1 b1 a2 b2 ...
            for pair in files.chunks(2) {
                match pair {
                    [a, b] => {
                        let (pa, pb) = (split(*a, rng), split(*b, rng));
                        for i in 0..pa.len().max(pb.len()) {
                            order.extend(pa.get(i).copied());
                            order.extend(pb.get(i).copied());
                        }
                    }
                    [single] => order.push((*single, 0, len_of(*single))),
                    _ => {}
                }
            }
        }
    }
    order
}

/// Splits a file into `k` pieces cut at multiples of `align` (fewer if it is too small).
fn cut(file: usize, len: u64, k: u32, align: u64, rng: &mut Rng) -> Vec<Piece> {
    let units = len / align;
    let k = u64::from(k).min(units.max(1));
    let mut cuts: Vec<u64> = Vec::new();
    while (cuts.len() as u64) < k - 1 {
        let c = rng.range(1, units.saturating_sub(1).max(1)) * align;
        if c < len && !cuts.contains(&c) {
            cuts.push(c);
        }
    }
    cuts.sort_unstable();
    let mut pieces = Vec::new();
    let mut start = 0;
    for c in cuts.into_iter().chain([len]) {
        pieces.push((file, start, c - start));
        start = c;
    }
    pieces
}

/// Writes `len` bytes of filler at `at` (clipped to the disk).
fn filler(disk: &mut [u8], at: u64, len: u64, fill: Fill, rng: &mut Rng) -> Result<()> {
    let start = usize::try_from(at)?.min(disk.len());
    let end = usize::try_from(at.saturating_add(len))?.min(disk.len());
    let Some(region) = disk.get_mut(start..end) else {
        return Ok(());
    };
    match fill {
        Fill::Random => {
            rng.fill(region);
            // Random filler must not invent file headers or end markers.
            let scrubbed = content::no_markers(region.to_vec());
            region.copy_from_slice(&scrubbed);
        }
        Fill::Byte(b) => region.fill(b),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_covers_the_file_at_aligned_points() {
        let mut rng = Rng::new(1);
        let pieces = cut(0, 100_000, 4, 4096, &mut rng);
        assert_eq!(pieces.len(), 4);
        assert_eq!(pieces.first().map(|p| p.1), Some(0));
        assert_eq!(pieces.iter().map(|p| p.2).sum::<u64>(), 100_000);
        assert!(pieces.iter().all(|p| p.1 % 4096 == 0));
    }

    #[test]
    fn small_files_get_fewer_pieces() {
        let pieces = cut(0, 5000, 4, 4096, &mut Rng::new(2));
        assert_eq!(pieces.len(), 1);
    }

    #[test]
    fn layouts_place_every_piece_once() {
        let len_of = |f: usize| (f as u64 + 1) * 50_000;
        let files = [0, 1, 2, 3];
        for layout in [
            Placement::Contiguous,
            Placement::Sequential,
            Placement::Shuffled,
            Placement::Nested,
            Placement::Braided,
        ] {
            let order = arrange(&files, &len_of, layout, [2, 3], 512, &mut Rng::new(3));
            for f in files {
                let total: u64 = order.iter().filter(|p| p.0 == f).map(|p| p.2).sum();
                assert_eq!(total, len_of(f), "{layout:?} file {f}");
            }
        }
        let missing = arrange(
            &files,
            &len_of,
            Placement::Missing,
            [2, 3],
            512,
            &mut Rng::new(4),
        );
        for f in files {
            let total: u64 = missing.iter().filter(|p| p.0 == f).map(|p| p.2).sum();
            assert!(total < len_of(f));
        }
    }
}
