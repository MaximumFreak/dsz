//! Bluetooth haptics report `0x32`: 3 kHz signed 8-bit stereo PCM for the
//! two actuators.
//!
//! Layout as published by SAxense (Sdore, MPL-2.0, apps.sdore.me/SAxense)
//! and in godot-dualsense-native's `docs/PROTOCOL.md` (Lelisvaldo, MIT):
//!
//! ```text
//! [0]       0x32           report id
//! [1]       seq << 4       4-bit sequence, low nibble 0
//! [2..11]   packet 0x11    91 07 FE 00 00 00 00 FF, then a counter (+1 per report)
//! [11..77]  packet 0x12    92 40, then 32 interleaved stereo frames, s8
//! [77..138] zero
//! [138..]   CRC-32, prefix 0xA2
//! ```
//!
//! A packet's first byte is its id in the low six bits with bit 7 set
//! ("sized"); the next byte is the payload length.

use crate::crc;

pub const REPORT_ID: u8 = 0x32;
/// Report length including the id.
pub const REPORT_LEN: usize = 142;
pub const MAX_REPORT: usize = REPORT_LEN;
/// Actuator bytes per report: 32 stereo frames.
pub const HAPTIC_FRAME: usize = 64;
/// Frames per report per channel (3 kHz, 10.667 ms).
pub const HAPTIC_SAMPLES_PER_CHANNEL: usize = 32;
/// Report period in nanoseconds: 32 / 3000 s.
pub const PERIOD_NS: u64 = 10_666_667;

const CONFIG_PACKET: [u8; 8] = [0x91, 0x07, 0xFE, 0x00, 0x00, 0x00, 0x00, 0xFF];
const COUNTER_AT: usize = 10;
const HAPTICS_AT: usize = 11;

/// Sequence and packet counters for one controller's haptics stream.
#[derive(Clone, Copy, Debug, Default)]
pub struct StreamCounters {
    seq: u8,
    counter: u8,
}

impl StreamCounters {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Build one haptics report into `out` (at least `REPORT_LEN` bytes) and
/// return it. Advances the counters.
pub fn build<'a>(
    counters: &mut StreamCounters,
    haptics: &[u8; HAPTIC_FRAME],
    out: &'a mut [u8],
) -> &'a [u8] {
    let r = &mut out[..REPORT_LEN];
    r.fill(0);
    r[0] = REPORT_ID;
    r[1] = counters.seq << 4;
    r[2..COUNTER_AT].copy_from_slice(&CONFIG_PACKET);
    r[COUNTER_AT] = counters.counter;
    r[HAPTICS_AT] = 0x92;
    r[HAPTICS_AT + 1] = HAPTIC_FRAME as u8;
    r[HAPTICS_AT + 2..HAPTICS_AT + 2 + HAPTIC_FRAME].copy_from_slice(haptics);
    crc::seal(crc::PREFIX_OUTPUT, r);

    counters.seq = (counters.seq + 1) & 0x0F;
    counters.counter = counters.counter.wrapping_add(1);
    &out[..REPORT_LEN]
}

/// A -1..=1 sample as the actuators' signed byte, symmetric about zero
/// (full scale is ±127); louder input is clipped.
#[inline]
pub fn haptic_sample(x: f32) -> u8 {
    (x.clamp(-1.0, 1.0) * 127.0).round() as i8 as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern() -> [u8; HAPTIC_FRAME] {
        let mut f = [0u8; HAPTIC_FRAME];
        for (i, b) in f.iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        f
    }

    #[test]
    fn layout() {
        let mut buf = [0u8; MAX_REPORT];
        let mut c = StreamCounters::new();
        let r = build(&mut c, &pattern(), &mut buf);
        assert_eq!(r.len(), 142);
        assert_eq!(
            &r[..11],
            &[0x32, 0x00, 0x91, 0x07, 0xFE, 0x00, 0x00, 0x00, 0x00, 0xFF, 0x00]
        );
        assert_eq!(&r[11..13], &[0x92, 0x40]);
        assert_eq!(&r[13..77], &pattern());
        assert!(r[77..138].iter().all(|&b| b == 0));
        assert!(crc::verify(crc::PREFIX_OUTPUT, r));
    }

    #[test]
    fn counters_advance() {
        let f = pattern();
        let mut buf = [0u8; MAX_REPORT];
        let mut c = StreamCounters::new();
        for k in 0..20u8 {
            let r = build(&mut c, &f, &mut buf);
            assert_eq!(r[1], (k & 0x0F) << 4);
            assert_eq!(r[10], k);
            assert!(crc::verify(crc::PREFIX_OUTPUT, r));
        }
    }

    #[test]
    fn int8_conversion() {
        assert_eq!(haptic_sample(1.0), 127);
        assert_eq!(haptic_sample(2.0), 127);
        assert_eq!(haptic_sample(-1.0) as i8, -127);
        assert_eq!(haptic_sample(-3.0) as i8, -127);
        assert_eq!(haptic_sample(f32::NAN), 0);
        assert_eq!(haptic_sample(0.0), 0);
        assert_eq!(haptic_sample(0.5), 64);
        assert_eq!(haptic_sample(-0.5) as i8, -64);
    }
}
