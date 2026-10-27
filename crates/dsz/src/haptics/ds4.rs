//! Sound on a DualShock 4's speaker or headphone jack over Bluetooth: PC
//! audio, a game's speaker channels from the virtual DualSense, or both.
//!
//! ```text
//! WASAPI loopback ─┐
//!                  ├─► low-pass ─► 32 kHz ─► 4 × SBC (109 B) ─► 0x17 report, every 16 ms
//! virtual DualSense speaker channels (48 kHz) ─┘
//! ```
//!
//! The reports are DS4 Sound Link's (https://github.com/abh1shekbana/DS4-Sound-Link,
//! by abh1shekbana, MIT): the control report `0x11` arms the audio path and
//! sets the volume (the writer adds those fields from `Device::ds4_audio`),
//! and `0x17` carries the audio. Off by default: the thread then only reads
//! the profile, and the controller gets exactly the reports it got before.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ds_proto::ds4::{AudioOut, AUDIO_FRAMES, AUDIO_PERIOD_NS, AUDIO_RATE, AUDIO_VOLUME_MAX, BT_POLL_MS};
use ds_proto::sbc;

use super::audio::Sub;
use super::dsp::{resample_window, soft_clip, Biquad};
use super::game::{GameReader, PcmSource};
use crate::device::{Device, StreamPacket, MAX_REPORT};
use crate::engine::{Engine, ProfileCache};
use crate::profile::{Ds4Audio, Ds4AudioOutput, Profile};

const PERIOD: Duration = Duration::from_nanos(AUDIO_PERIOD_NS);
/// 32 kHz frames in one report.
const REPORT_FRAMES: usize = sbc::SAMPLES * AUDIO_FRAMES;
/// 48 kHz game frames in one report.
const GAME_FRAMES: usize = REPORT_FRAMES * 3 / 2;
/// PC audio kept buffered ahead, in reports (about 48 ms), against jitter
/// in the capture and the radio.
const PRIME_REPORTS: usize = 3;
/// Low-pass below the new Nyquist before going to 32 kHz.
const CUTOFF_HZ: f32 = 14_500.0;

/// Volume 1..=100 as the report's volume byte.
fn volume_byte(v: u8) -> u8 {
    (v.min(100) as u32 * AUDIO_VOLUME_MAX as u32 / 100) as u8
}

/// What the profile asks of the controller's audio path right now. `game`
/// is whether a virtual DualSense is handing over game audio.
pub fn wanted(a: &Ds4Audio, game_speaker: bool, game: bool) -> AudioOut {
    let source = a.pc_audio || (a.game_speaker && game_speaker && game);
    if !source || a.volume == 0 {
        return AudioOut::Untouched;
    }
    let volume = volume_byte(a.volume);
    match a.output {
        Ds4AudioOutput::Speaker => AudioOut::Speaker { volume },
        Ds4AudioOutput::Headphones => AudioOut::Headphones { volume },
    }
}

/// The audio fields to send next: what is wanted while a source plays; one
/// `Off` after it stops (the writer then goes back to `Untouched`); and
/// `Untouched` for a controller that never had audio on.
pub fn next_route(current: AudioOut, wanted: AudioOut) -> AudioOut {
    if wanted.is_on() {
        wanted
    } else if current.is_on() {
        AudioOut::Off
    } else {
        current
    }
}

/// 4th-order Butterworth low-pass for one stereo signal at `fs`.
struct LowPass {
    fs: f32,
    stages: [[Biquad; 2]; 2],
}

impl LowPass {
    fn new(fs: f32) -> LowPass {
        let make = |q: f32| Biquad::lowpass(fs, CUTOFF_HZ, q);
        LowPass {
            fs,
            stages: [[make(0.541_196); 2], [make(1.306_563); 2]],
        }
    }

    fn run(&mut self, fs: f32, input: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
        if fs != self.fs {
            *self = LowPass::new(fs);
        }
        out.clear();
        for f in input {
            let mut x = *f;
            if fs > AUDIO_RATE as f32 {
                for stage in self.stages.iter_mut() {
                    x = [stage[0].run(x[0]), stage[1].run(x[1])];
                }
            }
            out.push(x);
        }
    }
}

/// One report's worth of input at the source rate, turned into 32 kHz.
struct Resampled {
    filter: LowPass,
    prev: [f32; 2],
    filtered: Vec<[f32; 2]>,
    out: Vec<[f32; 2]>,
}

impl Resampled {
    fn new() -> Self {
        Resampled {
            filter: LowPass::new(48_000.0),
            prev: [0.0; 2],
            filtered: Vec::with_capacity(GAME_FRAMES * 2),
            out: vec![[0.0; 2]; REPORT_FRAMES],
        }
    }

    /// `input` empty is silence.
    fn run(&mut self, rate: u32, input: &[[f32; 2]]) -> &[[f32; 2]] {
        self.filter.run(rate as f32, input, &mut self.filtered);
        resample_window(&mut self.prev, &self.filtered, &mut self.out);
        &self.out
    }
}

/// Captured PC audio, taken one report at a time.
struct PcInput {
    sub: Arc<Sub>,
    acc: f64,
    primed: bool,
    chunk: Vec<[f32; 2]>,
}

impl PcInput {
    fn new(sub: Arc<Sub>) -> Self {
        PcInput {
            sub,
            acc: 0.0,
            primed: false,
            chunk: Vec::with_capacity(4096),
        }
    }

    /// One report of PC audio at the capture rate; empty while priming or
    /// on an underrun.
    fn take(&mut self, underruns: &mut u64) -> (&[[f32; 2]], u32) {
        self.chunk.clear();
        let rate = self.sub.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return (&self.chunk, 48_000);
        }
        self.acc += rate as f64 * REPORT_FRAMES as f64 / AUDIO_RATE as f64;
        let l = self.acc.floor() as usize;
        self.acc -= l as f64;
        let mut f = self.sub.fifo.lock();
        let target = PRIME_REPORTS * l;
        if !self.primed {
            if f.len() < target + l {
                return (&self.chunk, rate);
            }
            self.primed = true;
        }
        if f.len() < l {
            self.primed = false;
            if !f.is_empty() {
                *underruns += 1;
            }
            return (&self.chunk, rate);
        }
        // Clock drift or a burst: trim back to the target latency.
        if f.len() > target + 6 * l {
            let drop = f.len() - target - l;
            f.drain(..drop);
        }
        self.chunk.extend(f.drain(..l));
        (&self.chunk, rate)
    }
}

/// PC and game sound mixed into one report's PCM: a mono mix for the
/// speaker, stereo for headphones.
pub fn mix(
    pc: &[[f32; 2]],
    game: &[[f32; 2]],
    game_gain: f32,
    headphones: bool,
    left: &mut [i16; REPORT_FRAMES],
    right: &mut [i16; REPORT_FRAMES],
) -> f32 {
    let mut peak = 0f32;
    let at = |s: &[[f32; 2]], i: usize| s.get(i).copied().unwrap_or([0.0; 2]);
    for i in 0..REPORT_FRAMES {
        let (p, g) = (at(pc, i), at(game, i));
        let mut f = [p[0] + g[0] * game_gain, p[1] + g[1] * game_gain];
        if !headphones {
            let m = 0.5 * (f[0] + f[1]);
            f = [m, m];
        }
        peak = peak.max(f[0].abs()).max(f[1].abs());
        left[i] = (soft_clip(f[0]) * 32767.0).round() as i16;
        right[i] = (soft_clip(f[1]) * 32767.0).round() as i16;
    }
    peak
}

/// Encode one report's PCM as four SBC frames.
pub fn encode(
    enc: &mut sbc::Encoder,
    left: &[i16; REPORT_FRAMES],
    right: &[i16; REPORT_FRAMES],
    frames: &mut [[u8; sbc::FRAME_LEN]; AUDIO_FRAMES],
) {
    for (i, f) in frames.iter_mut().enumerate() {
        let r = i * sbc::SAMPLES..(i + 1) * sbc::SAMPLES;
        enc.encode(
            left[r.clone()].try_into().unwrap(),
            right[r].try_into().unwrap(),
            f,
        );
    }
}

fn game_wanted(profile: &Profile) -> bool {
    profile.haptics.ds4_audio.game_speaker && profile.virtual_out.game_speaker
}

pub fn run(engine: &Arc<Engine>, device: &Arc<Device>) {
    let mut cache = ProfileCache::default();
    let mut timer: Option<crate::platform::PreciseTimer> = None;
    let mut pc: Option<PcInput> = None;
    let mut pc_resampled = Resampled::new();
    let mut game = GameReader::new();
    let mut game_chunk: Vec<[f32; 2]> = Vec::with_capacity(GAME_FRAMES);
    let mut game_resampled = Resampled::new();
    let mut enc = sbc::Encoder::new();
    let mut left = [0i16; REPORT_FRAMES];
    let mut right = [0i16; REPORT_FRAMES];
    let mut frames = [[0u8; sbc::FRAME_LEN]; AUDIO_FRAMES];
    let mut buf = [0u8; MAX_REPORT];
    let mut counter = 0u16;
    let mut deadline = Instant::now();
    let mut meter = 0f32;
    let mut on = false;

    while !device.stop.load(Ordering::Acquire) {
        let profile = cache.get(engine, device);
        let a = &profile.haptics.ds4_audio;
        // Only while a game has the virtual DualSense's audio endpoint open,
        // so an idle virtual pad doesn't keep the controller's radio busy.
        let game_src: Option<Arc<dyn PcmSource>> =
            if game_wanted(&profile) && device.virt.status.lock().game_audio_open {
                device.game_audio.lock().clone()
            } else {
                None
            };
        let want = wanted(a, profile.virtual_out.game_speaker, game_src.is_some());
        {
            let mut cur = device.ds4_audio.lock();
            let next = next_route(*cur, want);
            if *cur != next {
                *cur = next;
                drop(cur);
                device.force_output.store(true, Ordering::Release);
                device.kick();
            }
        }
        if !want.is_on() {
            if on {
                on = false;
                pc = None;
                timer = None;
                device.speaker_level.store(0, Ordering::Relaxed);
                device.flight.note("DualShock 4 audio off");
                log::info!("DualShock 4 audio off");
            }
            // Nothing is sent; just look at the profile now and then.
            let _ = device.haptics_wake.recv_timeout(Duration::from_millis(250));
            continue;
        }
        if !on {
            on = true;
            crate::platform::timer_resolution_1ms();
            crate::platform::raise_thread_priority(true);
            timer = Some(crate::platform::PreciseTimer::new());
            enc.reset();
            counter = 0;
            pc_resampled = Resampled::new();
            game_resampled = Resampled::new();
            game = GameReader::new();
            // Let the writer arm the audio path with 0x11 first.
            deadline = Instant::now() + PERIOD;
            device.flight.note(format!("DualShock 4 audio on: {want:?}"));
            log::info!("DualShock 4 audio on: {want:?}");
        }
        if !a.pc_audio {
            pc = None;
        } else if pc.as_ref().map(|p| p.sub.device != profile.haptics.device).unwrap_or(true) {
            pc = Some(PcInput::new(engine.audio.subscribe(&profile.haptics.device)));
        }

        if let Some(t) = &timer {
            t.sleep_until(deadline);
        }
        let now = Instant::now();
        deadline += PERIOD;
        if now > deadline + 3 * PERIOD {
            // Fell far behind (a stall): start the cadence over.
            deadline = now + PERIOD;
        }

        let mut underruns = 0u64;
        let pc_pcm: &[[f32; 2]] = match pc.as_mut() {
            Some(p) => {
                let (chunk, rate) = p.take(&mut underruns);
                pc_resampled.run(rate, chunk)
            }
            None => &[],
        };
        game_chunk.clear();
        if let Some(src) = &game_src {
            // The reader's window is at most 512 frames; a report is 768.
            for _ in 0..2 {
                match game.take(src.as_ref(), GAME_FRAMES / 2) {
                    Some(w) => game_chunk.extend_from_slice(&w.speaker),
                    None => game_chunk.resize(game_chunk.len() + GAME_FRAMES / 2, [0.0; 2]),
                }
            }
        }
        let game_pcm: &[[f32; 2]] = if game_src.is_some() {
            game_resampled.run(48_000, &game_chunk)
        } else {
            &[]
        };
        if underruns > 0 {
            device.stats.stream_underruns.fetch_add(underruns, Ordering::Relaxed);
        }
        let headphones = matches!(want, AudioOut::Headphones { .. });
        let game_gain = profile.virtual_out.game_speaker_level.min(100) as f32 / 100.0;
        let peak = mix(pc_pcm, game_pcm, game_gain, headphones, &mut left, &mut right);
        meter = peak.max(meter * 0.8);
        device.speaker_level.store((meter.min(1.0) * 255.0) as u8, Ordering::Relaxed);
        encode(&mut enc, &left, &right, &mut frames);
        let len = ds_proto::ds4::audio_report(counter, &frames, headphones, BT_POLL_MS, &mut buf).len();
        counter = counter.wrapping_add(AUDIO_FRAMES as u16);
        let mut pkt = StreamPacket {
            len,
            data: [0u8; MAX_REPORT],
        };
        pkt.data[..len].copy_from_slice(&buf[..len]);
        // The writer is behind: drop this report rather than queue latency.
        let _ = device.stream_tx.try_send(pkt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_by_default_leaves_the_report_alone() {
        let a = Ds4Audio::default();
        assert!(!a.pc_audio && !a.game_speaker);
        // Even with a game handing over audio and the virtual game speaker on.
        assert_eq!(wanted(&a, true, true), AudioOut::Untouched);
        assert_eq!(next_route(AudioOut::Untouched, wanted(&a, true, true)), AudioOut::Untouched);
    }

    #[test]
    fn sources_and_outputs() {
        let pc = Ds4Audio {
            pc_audio: true,
            ..Default::default()
        };
        assert_eq!(wanted(&pc, false, false), AudioOut::Speaker { volume: 101 });
        let hp = Ds4Audio {
            output: Ds4AudioOutput::Headphones,
            volume: 100,
            ..pc.clone()
        };
        assert_eq!(wanted(&hp, false, false), AudioOut::Headphones { volume: 127 });
        let silent = Ds4Audio { volume: 0, ..pc };
        assert_eq!(wanted(&silent, false, false), AudioOut::Untouched);
        // Game speaker needs the game, and the virtual output's game speaker.
        let game = Ds4Audio {
            game_speaker: true,
            ..Default::default()
        };
        assert_eq!(wanted(&game, true, false), AudioOut::Untouched);
        assert_eq!(wanted(&game, false, true), AudioOut::Untouched);
        assert!(wanted(&game, true, true).is_on());
    }

    #[test]
    fn turning_off_sends_one_off() {
        let on = AudioOut::Speaker { volume: 90 };
        assert_eq!(next_route(AudioOut::Untouched, on), on);
        assert_eq!(next_route(on, AudioOut::Untouched), AudioOut::Off);
        // The writer turns Off back into Untouched once it is sent.
        assert_eq!(next_route(AudioOut::Off, AudioOut::Untouched), AudioOut::Off);
        assert_eq!(next_route(AudioOut::Off, on), on);
    }

    #[test]
    fn speaker_is_a_mono_mix_and_headphones_stay_stereo() {
        let pc = vec![[0.5, -0.5]; REPORT_FRAMES];
        let game = vec![[0.2, 0.0]; REPORT_FRAMES];
        let (mut l, mut r) = ([0i16; REPORT_FRAMES], [0i16; REPORT_FRAMES]);
        mix(&pc, &game, 0.5, false, &mut l, &mut r);
        assert_eq!(l, r);
        assert_eq!(l[0], (0.05f32 * 32767.0).round() as i16);
        mix(&pc, &[], 1.0, true, &mut l, &mut r);
        assert_eq!((l[0], r[0]), (16384, -16384));
        // Silence when there is nothing to play.
        mix(&[], &[], 1.0, true, &mut l, &mut r);
        assert!(l.iter().chain(r.iter()).all(|s| *s == 0));
    }

    #[test]
    fn resampling_keeps_a_tone_and_its_pitch() {
        // 1 kHz at 48 kHz in, 32 kHz out: 16 cycles per 512 frames.
        let mut rs = Resampled::new();
        let mut n = 0usize;
        let mut out = Vec::new();
        for _ in 0..4 {
            let input: Vec<[f32; 2]> = (0..GAME_FRAMES)
                .map(|_| {
                    let v = (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 48_000.0).sin() * 0.5;
                    n += 1;
                    [v, v]
                })
                .collect();
            out = rs.run(48_000, &input).to_vec();
        }
        let crossings = out.windows(2).filter(|w| w[0][0] < 0.0 && w[1][0] >= 0.0).count();
        assert!((15..=17).contains(&crossings), "{crossings}");
        let peak = out.iter().fold(0f32, |m, f| m.max(f[0].abs()));
        assert!((0.45..0.55).contains(&peak), "{peak}");
    }

    #[test]
    fn a_report_carries_four_frames_of_one_report_of_audio() {
        let mut enc = sbc::Encoder::new();
        let (l, r) = ([0i16; REPORT_FRAMES], [0i16; REPORT_FRAMES]);
        let mut frames = [[0u8; sbc::FRAME_LEN]; AUDIO_FRAMES];
        encode(&mut enc, &l, &r, &mut frames);
        assert!(frames.iter().all(|f| f[0] == 0x9C));
        let mut buf = [0u8; MAX_REPORT];
        let rep = ds_proto::ds4::audio_report(4, &frames, false, BT_POLL_MS, &mut buf);
        assert_eq!(rep.len(), 462);
        assert_eq!(&rep[..6], &[0x17, 0x44, 0xA0, 4, 0, 0x02]);
    }
}
