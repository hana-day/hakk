//! Synthetic photo content: a gradient background with colored shapes and mild noise.
//!
//! Pure noise makes every photo statistically identical, so nothing could tell one photo's
//! data from another's. Real photos have structure that changes smoothly; these pictures
//! do too (and differ from file to file), while the noise keeps every compressed 512-byte
//! block unique for locating files on disk.

use crate::rng::Rng;

/// An RGB image of `width` x `height` pixels.
pub fn picture(width: u32, height: u32, rng: &mut Rng) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let color = |rng: &mut Rng| {
        [
            rng.range(0, 255) as f32,
            rng.range(0, 255) as f32,
            rng.range(0, 255) as f32,
        ]
    };
    let (c0, c1) = (color(rng), color(rng));
    let angle = rng.range(0, 359) as f32 * std::f32::consts::PI / 180.0;
    let (dx, dy) = (angle.cos(), angle.sin());
    let norm = (w as f32 * dx.abs() + h as f32 * dy.abs()).max(1.0);

    let mut img = vec![0u8; w * h * 3];
    // Background gradient.
    for y in 0..h {
        for x in 0..w {
            let t = ((x as f32 * dx + y as f32 * dy) / norm).rem_euclid(1.0);
            if let Some(px) = img.get_mut((y * w + x) * 3..(y * w + x) * 3 + 3) {
                for (i, p) in px.iter_mut().enumerate() {
                    let (a, b) = (
                        c0.get(i).copied().unwrap_or(0.0),
                        c1.get(i).copied().unwrap_or(0.0),
                    );
                    *p = (a + (b - a) * t) as u8;
                }
            }
        }
    }
    // Shapes: discs and rectangles.
    for _ in 0..rng.range(6, 16) {
        let c = color(rng);
        let cx = rng.range(0, w as u64) as i64;
        let cy = rng.range(0, h as u64) as i64;
        let r = rng.range(w as u64 / 20 + 1, w as u64 / 5 + 2) as i64;
        let disc = rng.range(0, 1) == 0;
        let (x0, x1) = ((cx - r).max(0), (cx + r).min(w as i64 - 1));
        let (y0, y1) = ((cy - r).max(0), (cy + r).min(h as i64 - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                if disc && (x - cx).pow(2) + (y - cy).pow(2) > r * r {
                    continue;
                }
                let at = (y as usize * w + x as usize) * 3;
                if let Some(px) = img.get_mut(at..at + 3) {
                    for (p, v) in px.iter_mut().zip(c) {
                        *p = v as u8;
                    }
                }
            }
        }
    }
    // Mild noise.
    let mut noise = vec![0u8; img.len()];
    rng.fill(&mut noise);
    for (p, n) in img.iter_mut().zip(noise) {
        *p = p.saturating_add(n & 0x0F).saturating_sub(8);
    }
    img
}
