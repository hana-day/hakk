//! Sets of byte ranges.

use std::ops::Range;

/// Disjoint, sorted byte ranges, each carrying a value. Adjacent ranges with equal values
/// are coalesced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeMap<V> {
    entries: Vec<(Range<u64>, V)>,
}

/// A set of byte ranges.
pub type RangeSet = RangeMap<()>;

impl<V> Default for RangeMap<V> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<V: Copy + Eq> RangeMap<V> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of stored ranges.
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// Total number of bytes covered.
    pub fn total_len(&self) -> u64 {
        self.entries.iter().map(|(r, _)| r.end - r.start).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Range<u64>, V)> + '_ {
        self.entries.iter().map(|(r, v)| (r.clone(), *v))
    }

    pub fn ranges(&self) -> impl Iterator<Item = Range<u64>> + '_ {
        self.entries.iter().map(|(r, _)| r.clone())
    }

    pub fn first(&self) -> Option<(Range<u64>, V)> {
        self.entries.first().map(|(r, v)| (r.clone(), *v))
    }

    /// The value at `pos`, if covered.
    pub fn get(&self, pos: u64) -> Option<V> {
        let i = self.entries.partition_point(|(r, _)| r.end <= pos);
        self.entries
            .get(i)
            .filter(|(r, _)| r.start <= pos)
            .map(|(_, v)| *v)
    }

    /// The parts of `range` that are covered, clipped to `range`.
    pub fn overlapping(&self, range: Range<u64>) -> impl Iterator<Item = (Range<u64>, V)> + '_ {
        let first = self.entries.partition_point(|(r, _)| r.end <= range.start);
        self.entries
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take_while(move |(r, _)| r.start < range.end)
            .map(move |(r, v)| (r.start.max(range.start)..r.end.min(range.end), *v))
    }

    /// Number of bytes of `range` that are covered.
    pub fn overlap_len(&self, range: Range<u64>) -> u64 {
        self.overlapping(range).map(|(r, _)| r.end - r.start).sum()
    }

    /// Sets `range` to `value`, overwriting whatever was there.
    pub fn insert(&mut self, range: Range<u64>, value: V) {
        if range.is_empty() {
            return;
        }
        self.remove(range.clone());
        let i = self.entries.partition_point(|(r, _)| r.end <= range.start);
        self.entries.insert(i, (range, value));
        // Coalesce with neighbours holding the same value.
        if let Some(next) = i.checked_add(1) {
            self.try_join(i, next);
        }
        if let Some(prev) = i.checked_sub(1) {
            self.try_join(prev, i);
        }
    }

    /// Removes `range`, splitting stored ranges that straddle its ends.
    pub fn remove(&mut self, range: Range<u64>) {
        if range.is_empty() {
            return;
        }
        let i = self.entries.partition_point(|(r, _)| r.end <= range.start);
        let j = self.entries.partition_point(|(r, _)| r.start < range.end);
        if i >= j {
            return;
        }
        let mut keep = Vec::with_capacity(2);
        if let Some((r, v)) = self.entries.get(i)
            && r.start < range.start
        {
            keep.push((r.start..range.start, *v));
        }
        if let Some((r, v)) = j.checked_sub(1).and_then(|last| self.entries.get(last))
            && r.end > range.end
        {
            keep.push((range.end..r.end, *v));
        }
        self.entries.splice(i..j, keep);
    }

    /// Joins entries `a` and `b = a + 1` if they touch and hold the same value.
    fn try_join(&mut self, a: usize, b: usize) {
        let joinable = match (self.entries.get(a), self.entries.get(b)) {
            (Some((ra, va)), Some((rb, vb))) => ra.end == rb.start && va == vb,
            _ => false,
        };
        if joinable {
            let (rb, _) = self.entries.remove(b);
            if let Some((ra, _)) = self.entries.get_mut(a) {
                ra.end = rb.end;
            }
        }
    }
}

impl RangeSet {
    pub fn add(&mut self, range: Range<u64>) {
        self.insert(range, ());
    }

    pub fn contains(&self, pos: u64) -> bool {
        self.get(pos).is_some()
    }

    pub fn to_vec(&self) -> Vec<Range<u64>> {
        self.ranges().collect()
    }
}

impl FromIterator<Range<u64>> for RangeSet {
    fn from_iter<I: IntoIterator<Item = Range<u64>>>(iter: I) -> Self {
        let mut set = RangeSet::new();
        for r in iter {
            set.add(r);
        }
        set
    }
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_and_splits() {
        let mut s: RangeSet = [10..20, 30..40, 20..25].into_iter().collect();
        assert_eq!(s.to_vec(), [10..25, 30..40]);
        s.add(24..31);
        assert_eq!(s.to_vec(), [10..40]);
        s.remove(15..17);
        assert_eq!(s.to_vec(), [10..15, 17..40]);
        s.remove(0..12);
        s.remove(38..100);
        assert_eq!(s.to_vec(), [12..15, 17..38]);
        assert_eq!(s.total_len(), 3 + 21);
        assert!(s.contains(12) && !s.contains(15) && s.contains(37) && !s.contains(38));
        assert_eq!(s.overlap_len(14..18), 2);
        s.remove(0..100);
        assert!(s.is_empty());
    }

    #[test]
    fn values_overwrite_and_only_equal_values_merge() {
        let mut m = RangeMap::new();
        m.insert(0..10, 1u32);
        m.insert(10..20, 1);
        m.insert(5..15, 2);
        assert_eq!(
            m.iter().collect::<Vec<_>>(),
            [(0..5, 1), (5..15, 2), (15..20, 1)]
        );
        m.insert(5..15, 1);
        assert_eq!(m.iter().collect::<Vec<_>>(), [(0..20, 1)]);
        assert_eq!(
            m.overlapping(3..7).collect::<Vec<_>>(),
            [(3..7, 1)],
            "clipped to the query"
        );
    }

    #[test]
    fn empty_ranges_are_ignored() {
        let mut s = RangeSet::new();
        s.add(5..5);
        s.remove(0..0);
        assert!(s.is_empty());
    }
}
