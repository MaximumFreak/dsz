//! Physical Bluetooth microphone PCM queued for the virtual USB capture endpoint.

use std::collections::VecDeque;

use parking_lot::Mutex;

const CHANNELS: usize = 2;
/// Keep latency bounded when the USB host falls behind the Bluetooth producer.
const MAX_FRAMES: usize = 4_800;

#[derive(Default)]
struct State {
    enabled: bool,
    samples: VecDeque<i16>,
}

/// A bounded queue of stereo, signed 16-bit, 48 kHz capture samples.
///
/// Samples are accepted only while the virtual USB microphone is open and
/// unmuted. Disabling or reopening it discards the previous stream's samples.
/// The producer must also call [`Self::clear`] when the physical source changes
/// or disconnects, so a persistent virtual pad never replays old speech.
pub struct MicrophoneRing {
    state: Mutex<State>,
}

impl MicrophoneRing {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                enabled: false,
                samples: VecDeque::with_capacity(MAX_FRAMES * CHANNELS),
            }),
        }
    }

    /// Append interleaved left/right PCM. An incomplete final frame is ignored.
    /// When full, discard the oldest complete frames and retain recent audio.
    pub fn push(&self, pcm: &[i16]) {
        let mut state = self.state.lock();
        if !state.enabled {
            return;
        }
        let samples = pcm.len() / CHANNELS * CHANNELS;
        let keep = samples.min(MAX_FRAMES * CHANNELS);
        let overflow = (state.samples.len() + keep).saturating_sub(MAX_FRAMES * CHANNELS);
        state.samples.drain(..overflow);
        state.samples.extend(&pcm[samples - keep..samples]);
    }

    /// Purge buffered speech, retaining the USB host's open/mute state.
    pub fn clear(&self) {
        self.state.lock().samples.clear();
    }

    pub(super) fn set_enabled(&self, enabled: bool) {
        let mut state = self.state.lock();
        if state.enabled != enabled {
            state.samples.clear();
            state.enabled = enabled;
        }
    }

    /// Fill complete stereo frames with little-endian PCM, padding underruns
    /// with silence. Muted packets still consume their samples, preventing a
    /// backlog from becoming audible when the host unmutes capture.
    pub(super) fn read_packet(&self, out: &mut [u8], gain: f32) {
        out.fill(0);
        let mut state = self.state.lock();
        for frame in out.chunks_exact_mut(CHANNELS * 2) {
            for sample in frame.chunks_exact_mut(2) {
                let value = state.samples.pop_front().unwrap_or(0);
                let value = (value as f32 * gain) as i16;
                sample.copy_from_slice(&value.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_is_stereo_little_endian_and_underflow_is_silence() {
        let ring = MicrophoneRing::new();
        ring.set_enabled(true);
        ring.push(&[0x1234, -2, 123]); // incomplete stereo frame is ignored
        let mut out = [0xFF; 8];
        ring.read_packet(&mut out, 1.0);
        assert_eq!(out, [0x34, 0x12, 0xFE, 0xFF, 0, 0, 0, 0]);
        ring.read_packet(&mut out, 1.0);
        assert_eq!(out, [0; 8]);
    }

    #[test]
    fn overflow_keeps_the_newest_complete_frames() {
        let ring = MicrophoneRing::new();
        ring.set_enabled(true);
        ring.push(&[11, 12]);
        let mut pcm = vec![1; MAX_FRAMES * CHANNELS];
        pcm.extend_from_slice(&[7, 8]);
        ring.push(&pcm);
        let state = ring.state.lock();
        assert_eq!(state.samples.len(), MAX_FRAMES * CHANNELS);
        assert_eq!(state.samples.front(), Some(&1));
        assert_eq!(state.samples.back(), Some(&8));
        drop(state);
        ring.push(&[9, 10]);
        assert_eq!(ring.state.lock().samples.len(), MAX_FRAMES * CHANNELS);
    }

    #[test]
    fn disabled_clear_and_reopened_stream_never_replay_samples() {
        let ring = MicrophoneRing::new();
        ring.push(&[1, 2]);
        ring.set_enabled(true);
        let mut out = [0xFF; 4];
        ring.read_packet(&mut out, 1.0);
        assert_eq!(out, [0; 4]);
        ring.push(&[1, 2]);
        ring.set_enabled(false);
        ring.push(&[3, 4]);
        ring.set_enabled(true);
        ring.read_packet(&mut out, 1.0);
        assert_eq!(out, [0; 4]);
        ring.push(&[5, 6]);
        ring.clear();
        ring.read_packet(&mut out, 1.0);
        assert_eq!(out, [0; 4]);
    }

    #[test]
    fn gain_saturates_and_muted_samples_are_consumed() {
        let ring = MicrophoneRing::new();
        ring.set_enabled(true);
        ring.push(&[20_000, -20_000]);
        let mut out = [0; 4];
        ring.read_packet(&mut out, 2.0);
        assert_eq!(out, [0xFF, 0x7F, 0, 0x80]);
        ring.push(&[1, 2]);
        ring.read_packet(&mut out, 0.0);
        assert_eq!(out, [0; 4]);
        ring.read_packet(&mut out, 1.0);
        assert_eq!(out, [0; 4]);
    }
}
