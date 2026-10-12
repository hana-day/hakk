//! AVCHD video (`.MTS`) generation: a BDAV MPEG-2 transport stream, as camcorders write.
//!
//! 192-byte source packets (a 4-byte header with a 30-bit arrival time stamp, then a
//! 188-byte transport packet), starting with the program association table and
//! following the PIDs AVCHD uses: PAT 0x0000, SIT 0x001F, PMT 0x0100, PCR 0x1001, video
//! 0x1011 (H.264), audio 0x1100 (AC-3). Continuity counters count per PID and the arrival
//! clock advances at about 24 Mbit/s. Payloads are random bytes (PES headers on the first
//! packet of each unit), so the structure is valid but nothing plays. The file is a whole
//! number of aligned units (32 packets, 6144 bytes).
//!
//! Reference: ISO/IEC 13818-1 2.4.3 (transport packets), 2.4.3.6 (PES packets); the BDAV
//! source packet format used by AVCHD (TP_extra_header, aligned units).

use std::collections::HashMap;

use super::{Generated, Meta};
use crate::rng::Rng;

const PACKET: usize = 192;
const UNIT: usize = 32 * PACKET;
/// Arrival clock ticks (27 MHz) per packet at about 24 Mbit/s.
const ATS_STEP: u32 = 1728;

const PAT: u16 = 0x0000;
const SIT: u16 = 0x001F;
const PMT: u16 = 0x0100;
const VIDEO: u16 = 0x1011;
const AUDIO: u16 = 0x1100;

pub fn generate(target: u64, rng: &mut Rng) -> Generated {
    let units = usize::try_from(target).unwrap_or(0).div_ceil(UNIT).max(1);
    let packets = units * UNIT / PACKET;
    let mut out = Vec::with_capacity(packets * PACKET);
    let mut cc: HashMap<u16, u8> = HashMap::new();
    let mut ats = u32::try_from(rng.range(0, 0x3FFF_FFFF)).unwrap_or(0);
    for i in 0..packets {
        // Tables at the start and every 100 packets; otherwise mostly video, some audio.
        let (pid, start) = match i % 100 {
            0 => (PAT, true),
            1 => (PMT, true),
            2 if i == 2 => (SIT, true),
            k if k % 12 == 5 => (AUDIO, k == 5),
            k => (VIDEO, k == 3),
        };
        let counter = cc.entry(pid).or_insert(0x0F);
        *counter = (*counter + 1) & 0x0F;
        out.extend((ats & 0x3FFF_FFFF).to_be_bytes());
        let pusi = if start { 0x40 } else { 0x00 };
        out.extend([
            0x47,
            pusi | (pid >> 8) as u8 & 0x1F,
            (pid & 0xFF) as u8,
            0x10 | *counter,
        ]);
        let mut payload = rng.bytes(184);
        if start && matches!(pid, VIDEO | AUDIO) {
            // A PES header: start code, stream id, length 0 (unbounded video).
            let stream = if pid == VIDEO { 0xE0 } else { 0xBD };
            payload.splice(..6, [0x00, 0x00, 0x01, stream, 0x00, 0x00]);
        }
        out.extend(payload);
        ats = ats.wrapping_add(ATS_STEP);
    }
    Generated {
        bytes: out,
        meta: Meta {
            width: Some(1920),
            height: Some(1080),
            ..Meta::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_units_with_sync_bytes() {
        let g = generate(100_000, &mut Rng::new(1));
        assert_eq!(g.bytes.len() % UNIT, 0);
        assert!(g.bytes.chunks(PACKET).all(|p| p[4] == 0x47));
        // The first packet is the PAT with a payload start.
        assert_eq!(g.bytes[5] & 0x40, 0x40);
        assert_eq!(
            (u16::from(g.bytes[5] & 0x1F) << 8) | u16::from(g.bytes[6]),
            PAT
        );
    }
}
