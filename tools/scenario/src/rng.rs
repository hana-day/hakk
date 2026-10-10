/// A deterministic PRNG seeded per scenario (SplitMix64).
///
/// Kept in-house so that dependency upgrades never change the sequence and break
/// scenario reproducibility.
///
/// Reference: Steele, Lea, Flood, "Fast Splittable Pseudorandom Number Generators", OOPSLA 2014.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform pick from `lo..=hi` (`lo` if `lo > hi`).
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if lo >= hi {
            return lo;
        }
        let span = hi - lo;
        match span.checked_add(1) {
            Some(n) => lo + self.next_u64() % n,
            None => self.next_u64(),
        }
    }

    /// Picks an index in `0..n` (0 if `n == 0`).
    pub fn index(&mut self, n: usize) -> usize {
        let hi = (n as u64).saturating_sub(1);
        usize::try_from(self.range(0, hi)).unwrap_or(0)
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            for (d, s) in chunk.iter_mut().zip(bytes) {
                *d = s;
            }
        }
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut v = vec![0; len];
        self.fill(&mut v);
        v
    }

    /// Fisher-Yates shuffle.
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.index(i + 1);
            v.swap(i, j);
        }
    }

    /// Derives a child generator (e.g. per file) without disturbing the parent sequence much.
    pub fn fork(&mut self) -> Self {
        Self(self.next_u64())
    }
}
