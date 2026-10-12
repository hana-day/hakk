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
//! Reference: ISO/IEC 13818-1 2.4.3 (transport packet: sync byte, PID, payload unit start,
//! adaptation field control, continuity counter); the BDAV source packet layout used by
//! AVCHD (a 4-byte TP_extra_header with copy permission and a 30-bit arrival time stamp
//! in front of each 188-byte packet; aligned units of 32 source packets).

use std::collections::HashMap;

use blocksource::BlockSource;

use crate::reader::Reader;

/// Bytes per BDAV source packet.
pub const PACKET: u64 = 192;
/// Packets per aligned unit.
const UNIT: u64 = 32;
/// The arrival clock runs at 27 MHz; a forward jump beyond this many ticks (about two
/// seconds) between consecutive packets means another file.
const MAX_ATS_STEP: u32 = 27_000_000 * 2;

/// A transport stream found at some offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mts {
    pub len: u64,
    pub status: Status,
    pub packets: u64,
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

/// Follows the stream at `start`. `None` if it does not look like a BDAV stream.
pub fn carve<S: BlockSource + ?Sized>(r: &mut Reader<S>, start: u64, max_len: u64) -> Option<Mts> {
    if !looks_like_mts(r.slice(start, 3 * 192)?) {
        return None;
    }
    let limit = start.saturating_add(max_len).min(r.len());
    let mut counters: HashMap<u16, u8> = HashMap::new();
    let mut last_ats: Option<u32> = None;
    let mut pos = start;
    let mut packets = 0u64;
    let mut ran_out = false;
    loop {
        if pos + PACKET > limit {
            ran_out = pos < limit || limit < r.len();
            break;
        }
        let Some(p) = r.slice(pos, 192) else {
            ran_out = true;
            break;
        };
        let (Some(&sync), Some(header)) = (p.get(4), p.get(..4)) else {
            break;
        };
        if sync != 0x47 {
            break;
        }
        // Arrival time stamp: the low 30 bits of the 4-byte header.
        let ats = header.try_into().map(u32::from_be_bytes).unwrap_or(0) & 0x3FFF_FFFF;
        if let Some(prev) = last_ats {
            let step = ats.wrapping_sub(prev) & 0x3FFF_FFFF;
            if step > MAX_ATS_STEP {
                break;
            }
        }
        // Continuity counter of PIDs that carry payload (adaptation_field_control 01/11).
        let (b1, b2, b3) = (p.get(5).copied()?, p.get(6).copied()?, p.get(7).copied()?);
        let pid = (u16::from(b1 & 0x1F) << 8) | u16::from(b2);
        let has_payload = b3 & 0x10 != 0;
        let cc = b3 & 0x0F;
        if has_payload && pid != 0x1FFF {
            if let Some(&prev) = counters.get(&pid)
                && cc != (prev + 1) & 0x0F
                && cc != prev
            {
                break;
            }
            counters.insert(pid, cc);
        }
        last_ats = Some(ats);
        packets += 1;
        pos += PACKET;
    }
    let len = pos - start;
    let status = if packets == 0 {
        return None;
    } else if packets.is_multiple_of(UNIT) {
        Status::Complete
    } else if ran_out {
        Status::Truncated
    } else {
        Status::Corrupt { at: pos }
    };
    Some(Mts {
        len,
        status,
        packets,
    })
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
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
    fn not_a_stream() {
        assert!(run(vec![0u8; 2000]).is_none());
        // Sync bytes but the first packet is not PAT/SIT.
        let mut s = stream(10, 0);
        s[5] = 0x10;
        s[6] = 0x11;
        assert!(run(s).is_none());
    }
}
