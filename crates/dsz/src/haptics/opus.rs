//! libopus, built into the exe from `vendor/opus` (see `build.rs`), for
//! the controller speaker.
//!
//! The frame format is fixed by what the controller decodes: a PS5 sends
//! 200-byte frames whose TOC byte is `0xF4` (RFC 6716 section 3.1: CELT
//! only, fullband, 10 ms, stereo, one frame), so 48 kHz stereo, 10 ms
//! frames, 160 kbit/s constant bitrate. The rest are libopus's own
//! recommendations for music at a high fixed rate: the general audio
//! application, the music signal hint, and the complexity measured in
//! `encode_cost` below.

use std::ffi::{c_int, c_void};

use ds_proto::stream::{OPUS_FRAME, OPUS_SAMPLES};

extern "C" {
    fn opus_encoder_create(
        fs: i32,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut c_void;
    fn opus_encoder_ctl(st: *mut c_void, request: c_int, ...) -> c_int;
    fn opus_encode(
        st: *mut c_void,
        pcm: *const i16,
        frame_size: c_int,
        data: *mut u8,
        max_data_bytes: i32,
    ) -> i32;
    fn opus_encoder_destroy(st: *mut c_void);
    fn opus_decoder_create(fs: i32, channels: c_int, error: *mut c_int) -> *mut c_void;
    fn opus_decode(
        st: *mut c_void,
        data: *const u8,
        len: i32,
        pcm: *mut i16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;
    fn opus_decoder_destroy(st: *mut c_void);
}

// From `opus_defines.h`.
const OPUS_APPLICATION_AUDIO: c_int = 2049;
const OPUS_SET_BITRATE: c_int = 4002;
const OPUS_SET_VBR: c_int = 4006;
const OPUS_SET_BANDWIDTH: c_int = 4008;
const OPUS_SET_COMPLEXITY: c_int = 4010;
const OPUS_SET_SIGNAL: c_int = 4024;
const OPUS_BANDWIDTH_FULLBAND: c_int = 1105;
const OPUS_SIGNAL_MUSIC: c_int = 3002;

/// 0..=10. Encoding 100 stereo frames a second at 10 costs well under 1 %
/// of a core (see the `encode_cost` test).
const COMPLEXITY: c_int = 10;

pub fn version() -> &'static str {
    env!("OPUS_VERSION")
}

/// Interleaved stereo PCM for one frame.
pub type Pcm = [i16; OPUS_SAMPLES * 2];

pub struct Encoder {
    st: *mut c_void,
}

unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new() -> Option<Encoder> {
        unsafe {
            let mut err: c_int = 0;
            let st = opus_encoder_create(48_000, 2, OPUS_APPLICATION_AUDIO, &mut err);
            if st.is_null() || err != 0 {
                log::warn!("opus_encoder_create failed: {err}");
                return None;
            }
            opus_encoder_ctl(st, OPUS_SET_BITRATE, (OPUS_FRAME * 8 * 100) as c_int);
            opus_encoder_ctl(st, OPUS_SET_VBR, 0 as c_int);
            opus_encoder_ctl(st, OPUS_SET_BANDWIDTH, OPUS_BANDWIDTH_FULLBAND);
            opus_encoder_ctl(st, OPUS_SET_COMPLEXITY, COMPLEXITY);
            opus_encoder_ctl(st, OPUS_SET_SIGNAL, OPUS_SIGNAL_MUSIC);
            Some(Encoder { st })
        }
    }

    /// Encode one 10 ms frame into a 200-byte slot. False if libopus
    /// failed; the slot is then zeroed.
    pub fn encode(&mut self, pcm: &Pcm, out: &mut [u8; OPUS_FRAME]) -> bool {
        out.fill(0);
        let n = unsafe {
            opus_encode(
                self.st,
                pcm.as_ptr(),
                OPUS_SAMPLES as c_int,
                out.as_mut_ptr(),
                OPUS_FRAME as i32,
            )
        };
        if n <= 0 {
            out.fill(0);
        }
        n > 0
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { opus_encoder_destroy(self.st) };
    }
}

/// Decoder for the controller's microphone packets (`--probe-virtual`
/// diagnostics), 48 kHz stereo.
pub struct Decoder {
    st: *mut c_void,
}

impl Decoder {
    pub fn new() -> Option<Decoder> {
        let mut err: c_int = 0;
        let st = unsafe { opus_decoder_create(48_000, 2, &mut err) };
        (!st.is_null() && err == 0).then_some(Decoder { st })
    }

    /// Decode one packet into interleaved stereo; returns samples per
    /// channel, or 0 on error.
    pub fn decode(&mut self, packet: &[u8], out: &mut [i16]) -> usize {
        let n = unsafe {
            opus_decode(
                self.st,
                packet.as_ptr(),
                packet.len() as i32,
                out.as_mut_ptr(),
                (out.len() / 2) as c_int,
                0,
            )
        };
        n.max(0) as usize
    }

    /// One 10 ms missing microphone frame, using Opus packet-loss concealment.
    pub fn conceal(&mut self, out: &mut [i16]) -> usize {
        let n = unsafe {
            opus_decode(
                self.st,
                std::ptr::null(),
                0,
                out.as_mut_ptr(),
                (out.len() / 2).min(OPUS_SAMPLES) as c_int,
                0,
            )
        };
        n.max(0) as usize
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.st) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(phase: &mut f32, hz: f32, amp: f32) -> Pcm {
        let mut pcm = [0i16; OPUS_SAMPLES * 2];
        for f in pcm.chunks_exact_mut(2) {
            let v = (phase.sin() * amp * 32767.0) as i16;
            f[0] = v;
            f[1] = v;
            *phase += hz * std::f32::consts::TAU / 48_000.0;
        }
        pcm
    }

    #[test]
    fn frames_match_the_console() {
        let mut enc = Encoder::new().expect("encoder");
        let mut out = [0u8; OPUS_FRAME];
        let mut ph = 0.0;
        for k in 0..20 {
            let pcm = if k < 10 { tone(&mut ph, 440.0, 0.5) } else { [0; OPUS_SAMPLES * 2] };
            assert!(enc.encode(&pcm, &mut out));
            // CELT only, fullband, 10 ms, stereo, one frame, as in the capture.
            assert_eq!(out[0], 0xF4, "frame {k}");
        }
    }

    #[test]
    fn constant_bitrate_fills_the_slot() {
        let mut enc = Encoder::new().expect("encoder");
        let mut out = [0u8; OPUS_FRAME];
        let mut ph = 0.0;
        for _ in 0..5 {
            assert!(enc.encode(&tone(&mut ph, 1000.0, 0.7), &mut out));
        }
        // A CBR frame uses the whole slot: its tail is not padding.
        assert!(out[180..].iter().any(|&b| b != 0));
    }

    #[test]
    fn encode_cost() {
        let mut enc = Encoder::new().expect("encoder");
        let mut out = [0u8; OPUS_FRAME];
        let mut ph = 0.0;
        let frames: Vec<Pcm> = (0..100).map(|_| tone(&mut ph, 523.0, 0.6)).collect();
        let t = std::time::Instant::now();
        for f in &frames {
            enc.encode(f, &mut out);
        }
        // One second of audio. Unoptimized test builds are slower, so the
        // bound is loose; release builds are several times faster.
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        eprintln!("1 s of speaker audio encoded in {ms:.1} ms");
        assert!(ms < 200.0, "{ms} ms");
    }
}
