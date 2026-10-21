//! Bluetooth media reports: `0x32` carries 3 kHz signed 8-bit stereo PCM
//! for the two actuators; `0x39` adds two Opus frames for the speaker (or
//! the headset jack).
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
//! ("sized"); the next byte is the payload length. Bit 6 doubles the
//! payload: two blocks of that length (Nielk1's notes in SAxense issue #1).
//!
//! `0x39` is laid out as a PS5 sends it, from Nielk1's Bluetooth capture of
//! a console (`packet_samples.txt`, SAxense issue #1): a config packet,
//! two Opus frames, two haptic blocks.
//!
//! ```text
//! [0]         0x39           report id
//! [1]         seq << 4
//! [2..10]     packet 0x11    91 06 7F 51 1F 62 28, then the audio frame
//!                            counter (+1 per Opus frame, so +2 per report)
//! [10..412]   packet 0x15    D5 C8, then two 200-byte Opus frames
//! [412..542]  packet 0x12    D2 40, then two blocks of 32 stereo frames, s8
//! [542]       zero
//! [543..]     CRC-32, prefix 0xA2
//! ```
//!
//! Every Opus frame in the capture opens with TOC `0xF4` (RFC 6716: CELT
//! only, fullband, 10 ms, stereo, one frame), and 200 bytes per 10 ms is
//! 160 kbit/s constant bitrate.

use crate::crc;

pub const REPORT_ID: u8 = 0x32;
/// Report length including the id.
pub const REPORT_LEN: usize = 142;
pub const MAX_REPORT: usize = AUDIO_REPORT_LEN;
/// Actuator bytes per report: 32 stereo frames.
pub const HAPTIC_FRAME: usize = 64;
/// Frames per report per channel (3 kHz, 10.667 ms).
pub const HAPTIC_SAMPLES_PER_CHANNEL: usize = 32;
/// Report period in nanoseconds: 32 / 3000 s.
pub const PERIOD_NS: u64 = 10_666_667;

const CONFIG_PACKET: [u8; 8] = [0x91, 0x07, 0xFE, 0x00, 0x00, 0x00, 0x00, 0xFF];
const COUNTER_AT: usize = 10;
const HAPTICS_AT: usize = 11;

pub const AUDIO_REPORT_ID: u8 = 0x39;
/// Length of the speaker report including the id.
pub const AUDIO_REPORT_LEN: usize = 547;
/// One Opus frame: 10 ms at 160 kbit/s CBR.
pub const OPUS_FRAME: usize = 200;
/// Opus frames and haptic blocks per speaker report.
pub const BLOCKS: usize = 2;
/// Samples per channel in one Opus frame (10 ms at 48 kHz).
pub const OPUS_SAMPLES: usize = 480;

/// The console's config packet, counter not included.
const AUDIO_CONFIG: [u8; 7] = [0x91, 0x06, 0x7F, 0x51, 0x1F, 0x62, 0x28];
const AUDIO_COUNTER_AT: usize = 9;
const OPUS_AT: usize = 10;
const AUDIO_HAPTICS_AT: usize = OPUS_AT + 2 + BLOCKS * OPUS_FRAME;
/// Opus packet types: the console's speaker audio, and headphone audio
/// (godot-dualsense-native uses `0x16` for headphones). Tested on an Edge
/// over Bluetooth: with `0x15` the jack plays the left channel on both
/// sides and the speaker the right one, whatever the output report's path;
/// `0x16` plays stereo on the jack; `0x13` left the jack silent.
const OPUS_SPEAKER: u8 = 0x15;
const OPUS_HEADSET: u8 = 0x16;

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
    build_with_microphone(counters, haptics, None, out)
}

/// Haptics plus optional microphone admission (`None` preserves capture bytes). Every media configuration
/// carries bit0, so an ordinary haptics packet must preserve capture too.
pub fn build_with_microphone<'a>(
    counters: &mut StreamCounters,
    haptics: &[u8; HAPTIC_FRAME],
    microphone: Option<bool>,
    out: &'a mut [u8],
) -> &'a [u8] {
    let r = &mut out[..REPORT_LEN];
    r.fill(0);
    r[0] = REPORT_ID;
    r[1] = counters.seq << 4;
    r[2..COUNTER_AT].copy_from_slice(&CONFIG_PACKET);
    if let Some(microphone) = microphone {
        r[4] = (r[4] & !AUDIO_CONTROL_MIC) | u8::from(microphone);
    }
    r[COUNTER_AT] = counters.counter;
    r[HAPTICS_AT] = 0x92;
    r[HAPTICS_AT + 1] = HAPTIC_FRAME as u8;
    r[HAPTICS_AT + 2..HAPTICS_AT + 2 + HAPTIC_FRAME].copy_from_slice(haptics);
    crc::seal(crc::PREFIX_OUTPUT, r);

    counters.seq = (counters.seq + 1) & 0x0F;
    counters.counter = counters.counter.wrapping_add(1);
    &out[..REPORT_LEN]
}

/// Build one speaker report (two Opus frames, two haptic blocks) into `out`
/// (at least `AUDIO_REPORT_LEN` bytes) and return it. Advances the
/// counters: the sequence by one, the frame counter by two. `headset`
/// sends the audio to the headset jack in stereo instead of the speaker.
pub fn build_audio<'a>(
    counters: &mut StreamCounters,
    opus: &[[u8; OPUS_FRAME]; BLOCKS],
    haptics: &[[u8; HAPTIC_FRAME]; BLOCKS],
    headset: bool,
    out: &'a mut [u8],
) -> &'a [u8] {
    // Preserve the captured configuration until capture is explicitly managed.
    build_audio_with_microphone(counters, opus, haptics, headset, None, out)
}

pub fn build_audio_with_microphone<'a>(
    counters: &mut StreamCounters,
    opus: &[[u8; OPUS_FRAME]; BLOCKS],
    haptics: &[[u8; HAPTIC_FRAME]; BLOCKS],
    headset: bool,
    microphone: Option<bool>,
    out: &'a mut [u8],
) -> &'a [u8] {
    let r = &mut out[..AUDIO_REPORT_LEN];
    r.fill(0);
    r[0] = AUDIO_REPORT_ID;
    r[1] = counters.seq << 4;
    r[2..AUDIO_COUNTER_AT].copy_from_slice(&AUDIO_CONFIG);
    if let Some(microphone) = microphone {
        r[4] = (r[4] & !AUDIO_CONTROL_MIC) | u8::from(microphone);
    }
    r[AUDIO_COUNTER_AT] = counters.counter;
    r[OPUS_AT] = 0x80 | 0x40 | if headset { OPUS_HEADSET } else { OPUS_SPEAKER };
    r[OPUS_AT + 1] = OPUS_FRAME as u8;
    for (k, f) in opus.iter().enumerate() {
        let at = OPUS_AT + 2 + k * OPUS_FRAME;
        r[at..at + OPUS_FRAME].copy_from_slice(f);
    }
    r[AUDIO_HAPTICS_AT] = 0x80 | 0x40 | 0x12;
    r[AUDIO_HAPTICS_AT + 1] = HAPTIC_FRAME as u8;
    for (k, f) in haptics.iter().enumerate() {
        let at = AUDIO_HAPTICS_AT + 2 + k * HAPTIC_FRAME;
        r[at..at + HAPTIC_FRAME].copy_from_slice(f);
    }
    crc::seal(crc::PREFIX_OUTPUT, r);

    counters.seq = (counters.seq + 1) & 0x0F;
    counters.counter = counters.counter.wrapping_add(BLOCKS as u8);
    &out[..AUDIO_REPORT_LEN]
}

/// Flags of the `91 01 <flags>` config packet, a bitmask as Nielk1 notes
/// for the PS5 Media Remote (SAxense issue #1). Tested on a DualSense Edge
/// over Bluetooth: bit 0 streams the microphone back in `0x31` reports,
/// bit 1 keeps the input (state) reports coming; `00` stops both.
pub const AUDIO_CONTROL_MIC: u8 = 0x01;
pub const AUDIO_CONTROL_INPUT: u8 = 0x02;

/// Build a report that stops the microphone stream and keeps input on:
/// report `0x32` holding only the config packet `91 01 02`. While a speaker
/// stream runs the controller sends microphone audio back in `0x31`
/// reports, and it keeps doing so after the stream ends until told to
/// stop.
pub fn build_mic_off<'a>(counters: &mut StreamCounters, out: &'a mut [u8]) -> &'a [u8] {
    build_audio_control(counters, AUDIO_CONTROL_INPUT, out)
}

/// Report `0x32` holding only the config packet `91 01 <flags>`.
pub fn build_audio_control<'a>(counters: &mut StreamCounters, flags: u8, out: &'a mut [u8]) -> &'a [u8] {
    let r = &mut out[..REPORT_LEN];
    r.fill(0);
    r[0] = REPORT_ID;
    r[1] = counters.seq << 4;
    r[2..5].copy_from_slice(&[0x91, 0x01, flags]);
    crc::seal(crc::PREFIX_OUTPUT, r);
    counters.seq = (counters.seq + 1) & 0x0F;
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
    fn audio_layout() {
        let mut opus = [[0u8; OPUS_FRAME]; BLOCKS];
        opus[0][0] = 0xF4;
        opus[1][0] = 0xF4;
        opus[1][199] = 0xAB;
        let h = pattern();
        let mut buf = [0u8; MAX_REPORT];
        let mut c = StreamCounters::new();
        let r = build_audio(&mut c, &opus, &[h, h], false, &mut buf).to_vec();
        assert_eq!(r.len(), 547);
        assert_eq!(&r[..10], &[0x39, 0x00, 0x91, 0x06, 0x7F, 0x51, 0x1F, 0x62, 0x28, 0x00]);
        assert_eq!(&r[10..12], &[0xD5, 0xC8]);
        assert_eq!(r[12], 0xF4);
        assert_eq!(r[212], 0xF4);
        assert_eq!(r[411], 0xAB);
        assert_eq!(&r[412..414], &[0xD2, 0x40]);
        assert_eq!(&r[414..478], &h);
        assert_eq!(&r[478..542], &h);
        assert_eq!(r[542], 0);
        assert!(crc::verify(crc::PREFIX_OUTPUT, &r));
        // Same packet shape as the console capture: 540 bytes of packets
        // after the sequence byte, then one zero.
        assert_eq!(2 + 6 + 2 + 400 + 2 + 128, 540);

        let mut c = StreamCounters::new();
        let r = build_audio(&mut c, &opus, &[h, h], true, &mut buf);
        assert_eq!(&r[10..12], &[0xD6, 0xC8], "headset audio type");
        assert!(crc::verify(crc::PREFIX_OUTPUT, r));
    }

    #[test]
    fn audio_counter_counts_frames() {
        let mut buf = [0u8; MAX_REPORT];
        let mut c = StreamCounters::new();
        let opus = [[0u8; OPUS_FRAME]; BLOCKS];
        let h = [0u8; HAPTIC_FRAME];
        for k in 0..20u8 {
            let r = build_audio(&mut c, &opus, &[h, h], false, &mut buf);
            assert_eq!(r[1], (k & 0x0F) << 4);
            assert_eq!(r[9], k.wrapping_mul(2));
        }
    }

    #[test]
    fn mic_off() {
        let mut buf = [0u8; MAX_REPORT];
        let mut c = StreamCounters::new();
        let r = build_mic_off(&mut c, &mut buf).to_vec();
        assert_eq!(r.len(), REPORT_LEN);
        assert_eq!(&r[..5], &[0x32, 0x00, 0x91, 0x01, 0x02]);
        assert!(r[5..138].iter().all(|&b| b == 0));
        assert!(crc::verify(crc::PREFIX_OUTPUT, &r));
    }

    #[test]
    fn microphone_enable_survives_every_stream_configuration() {
        let mut buf = [0; MAX_REPORT];
        let mut c = StreamCounters::new();
        for microphone in [false, true] {
            let r = build_with_microphone(&mut c, &[0; HAPTIC_FRAME], Some(microphone), &mut buf);
            assert_eq!(r[4], if microphone { 0xff } else { 0xfe });
            assert_eq!(r[4] & AUDIO_CONTROL_INPUT, AUDIO_CONTROL_INPUT);
            assert!(crc::verify(crc::PREFIX_OUTPUT, r));
            let r = build_audio_with_microphone(
                &mut c,
                &[[0; OPUS_FRAME]; BLOCKS],
                &[[0; HAPTIC_FRAME]; BLOCKS],
                true,
                Some(microphone),
                &mut buf,
            );
            assert_eq!(r[4], if microphone { 0x7f } else { 0x7e });
            assert_eq!(r[10], 0xd6, "headphone playback unchanged");
            assert!(crc::verify(crc::PREFIX_OUTPUT, r));
        }
        let r = build_audio_control(&mut c, AUDIO_CONTROL_INPUT | AUDIO_CONTROL_MIC, &mut buf);
        assert_eq!(&r[2..5], &[0x91, 1, 3]);
        assert!(
            r[5..138].iter().all(|b| *b == 0),
            "capture alone carries no speaker or haptics payload"
        );
        assert!(crc::verify(crc::PREFIX_OUTPUT, r));
    }

    #[test]
    fn unmanaged_media_preserves_the_captured_configuration() {
        let mut c = StreamCounters::new();
        let mut buf = [0; MAX_REPORT];
        for headset in [false, true] {
            let r = build_audio_with_microphone(&mut c, &[[0x55; OPUS_FRAME]; BLOCKS],
                &[[0x33; HAPTIC_FRAME]; BLOCKS], headset, None, &mut buf);
            assert_eq!(&r[2..AUDIO_COUNTER_AT], &AUDIO_CONFIG);
            assert_eq!(&r[2..5], &[0x91, 0x06, 0x7f]);
            assert!(crc::verify(crc::PREFIX_OUTPUT, r));
        }
        let r = build_with_microphone(&mut c, &[0x33; HAPTIC_FRAME], None, &mut buf);
        assert_eq!(&r[2..COUNTER_AT], &CONFIG_PACKET);
        assert!(crc::verify(crc::PREFIX_OUTPUT, r));
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
