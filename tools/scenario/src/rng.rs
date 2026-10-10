/// シードから決定的に動く擬似乱数（SplitMix64）。
///
/// 外部クレートの版上げで乱数列が変わり、シナリオの再現性が崩れるのを避けるため自前で持つ。
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

    /// `lo..=hi` から一様に選ぶ（`lo > hi` なら `lo`）。
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

    /// `0..n` の添字を1つ選ぶ（`n == 0` なら 0）。
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

    /// Fisher–Yates でシャッフルする。
    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.index(i + 1);
            v.swap(i, j);
        }
    }

    /// 子の乱数列を派生させる（ファイルごとの内容生成などに使い、親の列を乱さない）。
    pub fn fork(&mut self) -> Self {
        Self(self.next_u64())
    }
}
