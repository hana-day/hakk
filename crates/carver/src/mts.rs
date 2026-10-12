//! AVCHD / BDAV MPEG-2 transport stream (`.mts`, `.m2ts`) carving.
//!
//! These files are a plain run of 192-byte packets: a 4-byte header carrying a 30-bit
//! arrival time stamp, then a 188-byte transport stream packet starting with the sync byte
//! `0x47`. There is no file header or footer, so the file is followed packet by packet:
//! it ends where the sync bytes stop, or where another file evidently begins (the arrival
//! clock jumps, or a stream's continuity counter breaks). Camcorders write these files in
//! aligned units of 32 packets (6144 bytes); a file that does not end on one is reported
//! as broken there.
//!
//! A fragmented file is joined back together: where the packets stop, the next fragment
//! is looked for on cluster boundaries nearby, and taken only if its packets carry on
//! exactly where the stream stopped (the arrival clock moves on by a fraction of a second
//! and every stream's continuity counter by one), which data from anywhere else does not.
//!
//! Reference: ISO/IEC 13818-1 2.4.3 (transport packet: sync byte, PID, payload unit start,
//! adaptation field control, continuity counter); the BDAV source packet layout used by
//! AVCHD (a 4-byte TP_extra_header with copy permission and a 30-bit arrival time stamp
//! in front of each 188-byte packet; aligned units of 32 source packets).

use blocksource::BlockSource;

use crate::{fragments, reader::Reader};

/// Bytes per BDAV source packet.
pub const PACKET: u64 = 192;
/// Packets per aligned unit.
const UNIT: u64 = 32;
/// The arrival clock runs at 27 MHz; a forward jump beyond this many ticks (about two
/// seconds) between consecutive packets means another file.
const MAX_ATS_STEP: u32 = 27_000_000 * 2;
/// Across a join the clock must move on by less than this (0.1 s; camcorders send a
/// packet every few hundredths of a millisecond).
const JOIN_ATS_STEP: u32 = 27_000_000 / 10;
/// Packets that must carry on cleanly from a candidate fragment for it to be taken (or
/// fewer, ending the file on a unit boundary).
const JOIN_PACKETS: usize = 256;
/// Packets before the failure to consider as the end of a fragment: junk passes the
/// per-packet checks now and then, so the cut may lie a little before where they fail.
const HISTORY: usize = 8;
/// How far from the end of a fragment (either way) to look for the next one. Videos are
/// large and spread far on a full disk; the check at each place is one byte.
const JOIN_REACH: u64 = 64 << 20;
/// Most fragments to join (a video of a few gigabytes on a fragmented disk has hundreds).
const MAX_FRAGMENTS: usize = 4096;
/// Fragments start and end on sector boundaries.
const SECTOR: u64 = 512;
/// Streams whose continuity counters are tracked (AVCHD uses fewer than ten).
const MAX_PIDS: usize = 16;

/// A transport stream found at some offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mts {
    pub len: u64,
    pub status: Status,
    pub packets: u64,
    /// `(source offset, length)` of each piece in file order; one piece if contiguous.
    pub fragments: Vec<(u64, u64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Ended on an aligned unit.
    Complete,
    /// The packets stop mid-unit at this offset (often where a fragment ends).
    Corrupt { at: u64 },
    /// The data ran out.
    Truncated,
}

/// Whether `head` (at least 3 packets) starts a BDAV stream: sync bytes every 192 bytes
/// and a first packet carrying the program association table (PID 0) or the selection
/// information table (PID 0x1F) with a payload start.
pub fn looks_like_mts(head: &[u8]) -> bool {
    let sync = |k: usize| head.get(4 + k * 192) == Some(&0x47);
    if !(sync(0) && sync(1) && sync(2)) {
        return false;
    }
    let (Some(&b1), Some(&b2)) = (head.get(5), head.get(6)) else {
        return false;
    };
    let pid = (u16::from(b1 & 0x1F) << 8) | u16::from(b2);
    let start = b1 & 0x40 != 0;
    start && (pid == 0x0000 || pid == 0x001F)
}

/// Follows the stream at `start` without joining fragments. `None` if it does not look
/// like a BDAV stream.
pub fn carve<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Mts> {
    follow(r, start, max_len, None)
}

/// Follows the stream at `start`, joining the fragments it finds when `join` is set (its
/// grid; the reach and fragment count are this module's own, as videos are large).
pub fn follow<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    start: u64,
    max_len: u64,
    join: Option<&fragments::Options>,
) -> Option<Mts> {
    if !looks_like_mts(r.slice(start, 3 * 192)?) {
        return None;
    }
    let mut map = Map {
        closed: Vec::new(),
        open: (start, 0),
    };
    let mut state = State::default();
    // States before each of the last packets, oldest first: (logical offset, state).
    let mut history: Vec<(u64, State)> = Vec::with_capacity(HISTORY + 1);
    let mut pos = 0u64;
    let mut ran_out = false;
    loop {
        if pos + PACKET > max_len {
            ran_out = true;
            break;
        }
        let Some(p) = map.packet(r, pos) else {
            ran_out = true;
            break;
        };
        if history.len() > HISTORY {
            history.remove(0);
        }
        history.push((pos, state));
        if state.take(&p, MAX_ATS_STEP) {
            pos += PACKET;
            continue;
        }
        let Some(o) = join.filter(|_| map.closed.len() + 2 <= MAX_FRAGMENTS) else {
            break;
        };
        let ended = state.packets.is_multiple_of(UNIT);
        let Some((next, at, before)) = find_next(r, &map, &history, pos, ended, o) else {
            break;
        };
        map = next;
        pos = at;
        state = before;
        history.clear();
    }
    if state.packets == 0 {
        return None;
    }
    let mut fragments = map.closed.clone();
    if pos > map.open.1 {
        fragments.push((map.open.0, pos - map.open.1));
    }
    let status = if state.packets.is_multiple_of(UNIT) {
        Status::Complete
    } else if ran_out {
        Status::Truncated
    } else {
        Status::Corrupt {
            at: map.physical(pos),
        }
    };
    Some(Mts {
        len: pos,
        status,
        packets: state.packets,
        fragments,
    })
}

/// Looks for where the stream goes on after it failed at logical offset `failed`: a cut on
/// the grid shortly before the failure, and a fragment on the grid within reach of it whose
/// packets carry on from the state there. Returns the extended map, the logical offset to
/// go on from, and the state before it.
///
/// Later parts of the same stream can carry on too, when the packets skipped happen to
/// leave every continuity counter where it was; but skipping packets also skips their
/// arrival times. So the candidate whose clock moves on least is taken, and one moving on
/// no more than the stream normally does is taken at once. A stream that stopped on a unit
/// boundary may simply have ended, and another clip from the same camera can carry on from
/// a similar clock; there only a candidate moving on as normal is taken.
fn find_next<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    map: &Map,
    history: &[(u64, State)],
    failed: u64,
    ended: bool,
    o: &fragments::Options,
) -> Option<(Map, u64, State)> {
    let grid = o.grid.max(SECTOR);
    let oldest = history.first()?.0.max(map.open.1);
    let normal = history
        .iter()
        .zip(history.iter().skip(1))
        .filter_map(|(a, b)| Some(b.1.last_ats?.wrapping_sub(a.1.last_ats?) & 0x3FFF_FFFF))
        .max()
        .map_or(0, |s| s.saturating_mul(2));
    // Cuts on sector boundaries from the failure back to the oldest packet remembered (the
    // stream may have been picked up mid-file, so the file's own cluster grid is unknown),
    // each with the state before the packet it falls in.
    let mut cuts = Vec::new();
    let mut end = (map.physical(failed) + 4) / SECTOR * SECTOR;
    while end > map.open.0 && end >= map.physical(oldest) {
        let cut = map.open.1 + (end - map.open.0);
        let resume = cut / PACKET * PACKET;
        if let Some(&(_, before)) = history.iter().find(|(at, _)| *at == resume) {
            cuts.push((end, cut, resume, before));
        }
        end -= SECTOR;
    }
    // The next fragment starts a whole number of clusters away from the cut. All cuts are
    // tried at each distance, so they share the data read.
    let mut best: Option<(u32, Map, u64, State)> = None;
    for k in 1..=JOIN_REACH.max(o.reach) / grid {
        for &(end, cut, resume, before) in &cuts {
            for to in [end.checked_add(k * grid), end.checked_sub(k * grid)] {
                let Some(to) = to else { continue };
                // Cheap first: the sync byte of the first packet wholly past the cut.
                let first = cut.div_ceil(PACKET) * PACKET;
                if map.overlaps(to, end) || r.slice(to + (first - cut) + 4, 1) != Some(&[0x47][..])
                {
                    continue;
                }
                let next = map.cut(cut, to);
                let Some(step) = carries_on(r, &next, resume, cut, before, ended) else {
                    continue;
                };
                if step <= normal {
                    return Some((next, resume, before));
                }
                if !ended && best.as_ref().is_none_or(|b| step < b.0) {
                    best = Some((step, next, resume, before));
                }
            }
        }
    }
    best.map(|(_, next, resume, before)| (next, resume, before))
}

/// Whether the stream goes on cleanly through `map` from logical offset `resume`, given the
/// state before it: the arrival clock's step across the cut if so.
fn carries_on<S: BlockSource + ?Sized>(
    r: &mut Reader<S>,
    map: &Map,
    resume: u64,
    cut: u64,
    mut state: State,
    ended: bool,
) -> Option<u32> {
    let first = cut.div_ceil(PACKET) * PACKET;
    let at = map.physical(first);
    // A stream that stopped on a unit boundary may simply have ended; a new file starting
    // there (another clip of the same recording, carrying the clock on) is not a fragment.
    if ended && first == cut && r.slice(at, 3 * 192).is_some_and(looks_like_mts) {
        return None;
    }
    let mut pos = resume;
    let mut across = None;
    for _ in 0..JOIN_PACKETS {
        // Packets whose header (up to the continuity counter) lies past the cut are the
        // first to say anything about the new fragment.
        let new = pos + 8 > cut;
        let limit = if new { JOIN_ATS_STEP } else { MAX_ATS_STEP };
        let prev = state.last_ats;
        let took = map.packet(r, pos).is_some_and(|p| state.take(&p, limit));
        if !took {
            // A last fragment shorter than the check, ending the file.
            let ends = pos > cut && state.packets.is_multiple_of(UNIT);
            return across.filter(|_| ends);
        }
        if across.is_none() && new {
            across = Some(match (prev, state.last_ats) {
                (Some(a), Some(b)) => b.wrapping_sub(a) & 0x3FFF_FFFF,
                _ => 0,
            });
        }
        pos += PACKET;
    }
    across
}

/// Where the file's bytes are: closed fragments, then an open one running on.
#[derive(Debug, Clone)]
struct Map {
    /// `(source offset, length)` in file order.
    closed: Vec<(u64, u64)>,
    /// `(source offset, logical offset)` of the open fragment.
    open: (u64, u64),
}

impl Map {
    /// Source offset of a logical offset in the open fragment.
    fn physical(&self, logical: u64) -> u64 {
        self.open.0 + logical.saturating_sub(self.open.1)
    }

    /// The map with the open fragment closed at logical offset `cut` and a new one from
    /// source offset `to`.
    fn cut(&self, cut: u64, to: u64) -> Self {
        let mut closed = self.closed.clone();
        closed.push((self.open.0, cut - self.open.1));
        Self {
            closed,
            open: (to, cut),
        }
    }

    /// Whether source offset `at` lies in the file's own data (up to `end` in the open
    /// fragment).
    fn overlaps(&self, at: u64, end: u64) -> bool {
        (self.open.0..end).contains(&at)
            || self
                .closed
                .iter()
                .any(|&(o, l)| (o..o.saturating_add(l)).contains(&at))
    }

    /// The packet at logical offset `pos`, which may straddle the last cut.
    fn packet<S: BlockSource + ?Sized>(&self, r: &mut Reader<S>, pos: u64) -> Option<[u8; 192]> {
        let mut p = [0u8; 192];
        if pos >= self.open.1 {
            p.copy_from_slice(r.slice(self.physical(pos), 192)?);
            return Some(p);
        }
        let &(o, l) = self.closed.last()?;
        let before = usize::try_from(self.open.1 - pos)
            .ok()
            .filter(|&b| b < 192)?;
        let from = o + l - before as u64;
        p.get_mut(..before)?.copy_from_slice(r.slice(from, before)?);
        p.get_mut(before..)?
            .copy_from_slice(r.slice(self.open.0, 192 - before)?);
        Some(p)
    }
}

/// The per-packet checks: the arrival clock and each stream's continuity counter.
#[derive(Debug, Clone, Copy, Default)]
struct State {
    /// `(PID, last counter)` of the streams seen, the first `pids` entries.
    counters: [(u16, u8); MAX_PIDS],
    pids: usize,
    last_ats: Option<u32>,
    packets: u64,
}

impl State {
    /// Takes one packet; false (leaving the state as it was) if it does not continue the
    /// stream.
    fn take(&mut self, p: &[u8; 192], max_step: u32) -> bool {
        if p[4] != 0x47 {
            return false;
        }
        // Arrival time stamp: the low 30 bits of the 4-byte header.
        let ats = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x3FFF_FFFF;
        if let Some(prev) = self.last_ats
            && ats.wrapping_sub(prev) & 0x3FFF_FFFF > max_step
        {
            return false;
        }
        // Continuity counter of PIDs that carry payload (adaptation_field_control 01/11).
        let pid = (u16::from(p[5] & 0x1F) << 8) | u16::from(p[6]);
        let has_payload = p[7] & 0x10 != 0;
        let cc = p[7] & 0x0F;
        if has_payload && pid != 0x1FFF {
            let pids = self.pids;
            let seen = self.counters.iter_mut().take(pids).find(|e| e.0 == pid);
            match seen {
                Some(e) => {
                    if cc != (e.1 + 1) & 0x0F && cc != e.1 {
                        return false;
                    }
                    e.1 = cc;
                }
                None => {
                    if let Some(e) = self.counters.get_mut(pids) {
                        *e = (pid, cc);
                        self.pids += 1;
                    }
                }
            }
        }
        self.last_ats = Some(ats);
        self.packets += 1;
        true
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use std::collections::HashMap;

    use blocksource::MemSource;

    use super::*;

    /// `n` source packets: PAT first, then video on PID 0x1011, with continuity counters
    /// and arrival times counting up from `ats0`.
    fn stream(n: u64, ats0: u32) -> Vec<u8> {
        let mut v = Vec::new();
        let mut cc: HashMap<u16, u8> = HashMap::new();
        for i in 0..n {
            let pid: u16 = if i % 50 == 0 { 0 } else { 0x1011 };
            let c = cc.entry(pid).or_insert(15);
            *c = (*c + 1) & 0x0F;
            let ats = ats0.wrapping_add(i as u32 * 1000) & 0x3FFF_FFFF;
            v.extend(ats.to_be_bytes());
            v.extend([0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10 | *c]);
            v.extend([0x5A; 184]);
        }
        v
    }

    fn run(data: Vec<u8>) -> Option<Mts> {
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        carve(&mut r, 0, 1 << 30)
    }

    fn join(data: Vec<u8>) -> Option<Mts> {
        let src = MemSource::new(data);
        let mut r = Reader::new(&src);
        follow(&mut r, 0, 1 << 30, Some(&fragments::Options::default()))
    }

    /// Bytes that are not a stream.
    fn noise(n: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2_654_435_761) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    #[test]
    fn whole_units_are_complete() {
        let s = stream(64, 100);
        let got = run([s.clone(), vec![0u8; 1000]].concat()).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(got.len, s.len() as u64);
    }

    #[test]
    fn a_following_file_is_not_merged() {
        let a = stream(64, 100);
        let b = stream(32, 900_000_000);
        let got = run([a.clone(), b].concat()).unwrap();
        assert_eq!(got.len, a.len() as u64);
        assert_eq!(got.status, Status::Complete);
    }

    #[test]
    fn a_cut_mid_unit_is_reported() {
        let mut s = stream(40, 100);
        s.extend([0x11; 192 * 4]);
        let got = run(s).unwrap();
        assert_eq!(got.packets, 40);
        assert!(matches!(got.status, Status::Corrupt { .. }));
    }

    #[test]
    fn fragments_are_joined_in_order() {
        let s = stream(416, 100);
        let cut = 8 * 4096;
        let data = [&s[..cut], &noise(3 * 4096, 1), &s[cut..], &noise(4096, 2)].concat();
        let got = join(data).unwrap();
        assert_eq!(got.status, Status::Complete);
        let rest = (s.len() - cut) as u64;
        assert_eq!(
            got.fragments,
            vec![(0, cut as u64), (cut as u64 + 3 * 4096, rest)]
        );
    }

    #[test]
    fn a_later_part_placed_before_is_found() {
        // Parts 0, 2, 1 on disk.
        let s = stream(640, 100);
        let (a, b) = (8 * 4096, 20 * 4096);
        let data = [
            &s[..a],
            &noise(4096, 1),
            &s[b..],
            &noise(2 * 4096, 2),
            &s[a..b],
            &noise(4096, 3),
        ]
        .concat();
        let got = join(data).unwrap();
        assert_eq!(got.status, Status::Complete);
        assert_eq!(got.len, s.len() as u64);
        let tail = (s.len() - b) as u64;
        assert_eq!(
            got.fragments,
            vec![
                (0, a as u64),
                (a as u64 + 4096 + tail + 2 * 4096, (b - a) as u64),
                (a as u64 + 4096, tail),
            ]
        );
    }

    #[test]
    fn the_next_clip_is_not_joined() {
        // Clip b goes on with clip a's clock right after a gap; a ended on a unit.
        let a = stream(64, 100);
        let b = stream(64, 100 + 64 * 1000);
        let data = [&a[..], &noise(4096 - a.len() % 4096, 1), &b[..]].concat();
        let got = join(data).unwrap();
        assert_eq!(got.fragments, vec![(0, a.len() as u64)]);
    }

    #[test]
    fn not_a_stream() {
        assert!(run(vec![0u8; 2000]).is_none());
        // Sync bytes but the first packet is not PAT/SIT.
        let mut s = stream(10, 0);
        s[5] = 0x10;
        s[6] = 0x11;
        assert!(run(s).is_none());
    }
}
