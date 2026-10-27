//! Per-controller packet clock. Every 32/3000 s it turns one window of
//! audio plus event haptics into one Bluetooth haptics report (`0x32`).
//! While the speaker may play (a game has the virtual DualSense's audio
//! open, or PC audio goes to the speaker) it runs every 64/3000 s instead,
//! silent between sounds as a PS5 is, and sends
//! two haptic blocks with two Opus frames (`0x39`): the controller plays
//! the two 10 ms frames over that one 21.3 ms tick, so each report carries
//! 1024 frames of 48 kHz audio squeezed into 960.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ds_proto::input::Button;
use ds_proto::output::{SPEAKER_VOLUME_MAX, SPEAKER_VOLUME_MIN};
use ds_proto::stream::{self, StreamCounters, BLOCKS, HAPTIC_FRAME, OPUS_FRAME, OPUS_SAMPLES};
use ds_proto::Buttons;

use super::audio::Sub;
use super::dsp::{resample_window, soft_clip, HapticFilter, Squeeze};
use super::game::WINDOW;
use super::opus::{Encoder, Pcm};
use super::synth::Synth;
use crate::device::{Device, StreamPacket, MAX_REPORT, SPEAKER_HEADSET, SPEAKER_MUTED, SPEAKER_UNTOUCHED};
use crate::engine::{Engine, ProfileCache};
use crate::profile::{AudioSource, Haptics};

const PERIOD: Duration = Duration::from_nanos(stream::PERIOD_NS);
/// Stop sending after this long with nothing to play.
const IDLE_AFTER: Duration = Duration::from_millis(400);
/// Speaker sound below this peak counts as silence.
const SPEAKER_GATE: f32 = 1.0 / 2048.0;
/// Keep microphone-only control packets out of the active media clock.
/// Each media/haptic payload already carries the microphone enable bit.
struct MicrophoneClock {
    state: Option<bool>,
    next_control: Instant,
}

impl MicrophoneClock {
    fn new(now: Instant) -> Self {
        Self {
            state: None,
            next_control: now,
        }
    }

    fn due(&self, now: Instant, microphone: Option<bool>) -> bool {
        microphone.is_some() && (self.state != microphone
            || (microphone == Some(true) && now >= self.next_control))
    }

    fn sent(&mut self, now: Instant, microphone: Option<bool>) {
        self.state = microphone;
        self.next_control = now + Duration::from_secs(1);
    }
}
/// Silent speaker reports sent at once when a speaker stream starts, so the
/// controller has a cushion against radio jitter (about 64 ms). Without it
/// the sound pops wherever a report arrives a little late.
pub const SPEAKER_LEAD: usize = 3;

struct Source {
    sub: Arc<Sub>,
    acc: f64,
    primed: bool,
    chunk: Vec<[f32; 2]>,
}

impl Source {
    /// Take one window (`frames` frames at 48 kHz) of input. Empty on
    /// underrun.
    fn take(&mut self, frames: usize, prime_windows: usize, underruns: &mut u64) -> (&[[f32; 2]], u32) {
        let rate = self.sub.rate.load(Ordering::Relaxed);
        self.chunk.clear();
        if rate == 0 {
            return (&self.chunk, 48_000);
        }
        self.acc += rate as f64 * frames as f64 / 48_000.0;
        let l = self.acc.floor() as usize;
        self.acc -= l as f64;
        let mut f = self.sub.fifo.lock();
        let target = prime_windows * l;
        if !self.primed {
            if f.len() >= target + l {
                self.primed = true;
            } else {
                return (&self.chunk, rate);
            }
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

/// Split one captured PC window into independently enabled destinations.
struct PcAudio {
    filter: HapticFilter,
    prev_haptics: [f32; 2],
    prev_speaker: [f32; 2],
    filtered: Vec<[f32; 2]>,
}

impl PcAudio {
    fn new() -> Self {
        Self {
            filter: HapticFilter::new(48_000.0, 650.0, 25.0),
            prev_haptics: [0.0; 2],
            prev_speaker: [0.0; 2],
            filtered: Vec::with_capacity(1024),
        }
    }

    fn render(
        &mut self,
        h: &Haptics,
        chunk: &[[f32; 2]],
        rate: u32,
        haptics: &mut [[f32; 2]],
        speaker: &mut Vec<[f32; 2]>,
        frames: usize,
    ) {
        haptics.fill([0.0; 2]);
        speaker.clear();
        if h.source == AudioSource::SystemAudio {
            self.filter.configure(
                rate as f32,
                h.low_pass_hz.clamp(60.0, 1400.0),
                h.high_pass_hz.clamp(5.0, 300.0),
            );
            self.filtered.clear();
            for fr in chunk {
                let (l, r) = if h.stereo {
                    (fr[0], fr[1])
                } else {
                    let m = 0.5 * (fr[0] + fr[1]);
                    (m, m)
                };
                self.filtered.push([
                    self.filter.run(0, l) * h.gain,
                    self.filter.run(1, r) * h.gain,
                ]);
            }
            resample_window(&mut self.prev_haptics, &self.filtered, haptics);
        }
        if h.pc_audio_on_speaker && !chunk.is_empty() {
            speaker.resize(frames, [0.0; 2]);
            resample_window(&mut self.prev_speaker, chunk, speaker);
        }
    }
}

/// Speaker volume 1..=100 as the output report's volume byte.
fn volume_byte(v: u8) -> u8 {
    let span = (SPEAKER_VOLUME_MAX - SPEAKER_VOLUME_MIN) as u32;
    SPEAKER_VOLUME_MIN + ((v.clamp(1, 100) as u32 - 1) * span / 99) as u8
}

/// Headphone volume byte for a 0..=100 profile volume.
fn headset_volume_byte(v: u8) -> u8 {
    (v.min(100) as u32 * ds_proto::output::HEADSET_VOLUME_MAX as u32 / 100) as u8
}

fn to_i16(x: f32) -> i16 {
    (soft_clip(x) * 32767.0).round() as i16
}

pub fn run(engine: &Arc<Engine>, device: &Arc<Device>) {
    crate::platform::timer_resolution_1ms();
    crate::platform::raise_thread_priority(true);
    let timer = crate::platform::PreciseTimer::new();
    let mut cache = ProfileCache::default();
    let mut counters = StreamCounters::new();
    let mut source: Option<Source> = None;
    let mut pc_audio = PcAudio::new();
    let mut synth = Synth::new();
    let mut prev_buttons = Buttons::default();
    let mut deadline = Instant::now();
    let mut last_sound = Instant::now() - Duration::from_secs(10);
    let mut underruns = 0u64;
    let mut buf = [0u8; MAX_REPORT];
    let mut hap = [[0f32; 2]; 32];
    let mut late = 0u32;
    let mut levels = [0f32; 2];
    let mut speaker_meter = 0f32;
    let mut pulses: VecDeque<crate::device::HapticPulse> = VecDeque::new();
    let mut game = super::game::GameReader::new();
    let mut to_rumble = super::game::ActuatorRumble::new();
    let mut prev_g = [0f32; 2];
    let mut squeeze = Squeeze::new();
    let mut pc_spk: Vec<[f32; 2]> = Vec::with_capacity(WINDOW);
    // Speaker audio for one report, before and after fitting to 960 frames.
    let mut spk: Vec<[f32; 2]> = Vec::with_capacity(BLOCKS * WINDOW);
    let mut spk_fit: Vec<[f32; 2]> = Vec::with_capacity(BLOCKS * OPUS_SAMPLES + 8);
    let mut frames = [[0u8; HAPTIC_FRAME]; BLOCKS];
    let mut opus = [[0u8; OPUS_FRAME]; BLOCKS];
    let mut pcm: Pcm = [0; OPUS_SAMPLES * 2];
    let mut encoder: Option<Encoder> = None;
    let mut was_media = false;
    let mut lead = 0usize;
    // The sound that starts a speaker stream was noticed one window
    // earlier, so the stream joins it mid-sound: fade it in.
    let mut fade_in = false;

    // The stream keeps the actuators until this instant; anything that
    // needs it pushes the instant out by `IDLE_AFTER`.
    let mut stream_until = Instant::now();

    // Capture needs no speaker/actuator PCM stream. Keep controller state
    // reports enabled with bit1 and change only microphone admission bit0.
    let mic_control = |counters: &mut StreamCounters, microphone: bool| {
        let mut b = [0u8; MAX_REPORT];
        let mut queued = false;
        for _ in 0..2 {
            let flags = stream::AUDIO_CONTROL_INPUT
                | if microphone {
                    stream::AUDIO_CONTROL_MIC
                } else {
                    0
                };
            let len = stream::build_audio_control(counters, flags, &mut b).len();
            let mut pkt = StreamPacket {
                len,
                data: [0u8; MAX_REPORT],
            };
            pkt.data[..len].copy_from_slice(&b[..len]);
            queued |= device.stream_tx.try_send(pkt).is_ok();
        }
        queued
    };
    let mut mic_clock = MicrophoneClock::new(Instant::now());
    let mut capture_used = false;

    let set_route = |route: u8| {
        if device.speaker_route.swap(route, Ordering::AcqRel) != route {
            device.kick();
        }
    };

    while !device.stop.load(Ordering::Acquire) {
        let profile = cache.get(engine, device);
        let h = &profile.haptics;
        let vo = &profile.virtual_out;
        // The recording device belongs to the virtual pad, which a held
        // profile doesn't change, so neither does its microphone.
        let pad_profile = engine.unheld_profile(device);
        device.apply_microphone(&pad_profile.as_deref().unwrap_or(&profile).microphone);
        // Only opt-in capture may change the hardware-tuned media bytes.
        // Keep this latch for this physical connection, even after disabling.
        capture_used |= device.microphone.enabled();
        let recording = capture_used && (device.microphone.recording() || device.mic_tap.lock().is_some());
        let microphone = capture_used.then_some(recording);
        let enabled = h.enabled && device.is_bluetooth();
        let game_src = device.game_audio.lock().clone();
        let game_speaker = game_src.is_some() && vo.game_speaker;
        let pc_speaker = h.pc_audio_on_speaker;
        let speaker_allowed = enabled && h.speaker_volume > 0 && (game_speaker || pc_speaker);
        // The speaker route goes on when the speaker is first needed and
        // stays on (silent between sounds) while the profile allows it:
        // switching the amplifier on for every burst of sound pops. Off
        // (muted) only when the profile turns the speaker off.
        let speaker_possible =
            enabled && h.speaker_volume > 0 && (vo.game_speaker || pc_speaker);
        let route_now = device.speaker_route.load(Ordering::Acquire);
        if speaker_allowed || (speaker_possible && route_now > SPEAKER_MUTED) {
            let headset = h.headset_when_plugged && device.live.lock().input.headphones;
            let hv = headset_volume_byte(h.speaker_volume);
            if device.headset_volume.swap(hv, Ordering::AcqRel) != hv && headset {
                device.kick();
            }
            set_route(if headset {
                SPEAKER_HEADSET
            } else {
                volume_byte(h.speaker_volume)
            });
        } else if !speaker_possible && route_now != SPEAKER_UNTOUCHED {
            set_route(SPEAKER_MUTED);
        }
        // The actuators take PCM only while something needs the stream.
        // Otherwise rumble goes to the controller's own rumble emulation,
        // which is what an Xbox game with no haptic modes on gets.
        let continuous = h.source == AudioSource::SystemAudio
            || pc_speaker
            || h.buttons.enabled
            || h.triggers.enabled
            || h.rumble_as_haptics
            || (game_src.is_some() && (vo.game_haptics || game_speaker));
        let now = Instant::now();
        if enabled
            && (continuous || !device.feedback.lock().pulses.is_empty() || synth.events_active())
        {
            stream_until = now + IDLE_AFTER;
        }
        let streaming = enabled && now < stream_until;
        device.pcm_mode.store(streaming, Ordering::Release);
        if !streaming {
            if mic_clock.due(now, microphone) && mic_control(&mut counters, recording) {
                mic_clock.sent(now, microphone);
            }
            source = None;
            encoder = None;
            if was_media {
                was_media = false;
                if mic_control(&mut counters, recording) {
                    mic_clock.sent(now, microphone);
                }
                log::info!("controller speaker stream off");
            }
            device.haptic_level[0].store(0, Ordering::Relaxed);
            device.haptic_level[1].store(0, Ordering::Relaxed);
            device.speaker_level.store(0, Ordering::Relaxed);
            synth.rumble(0, 0, 1.0);
            // Pulses without a stream play as a short classic rumble.
            let mut fb = device.feedback.lock();
            if !fb.pulses.is_empty() {
                let pending: Vec<_> = std::mem::take(&mut fb.pulses);
                fb.rumbles.extend(pending.iter().map(|p| p.as_rumble(now)));
                drop(fb);
                device.kick();
            } else {
                drop(fb);
            }
            // Actuators are off: game haptics become classic rumble.
            let converting = game_src.is_some() && vo.game_haptics;
            if let Some(src) = game_src.filter(|_| vo.game_haptics) {
                if let Some(w) = game.take(src.as_ref(), WINDOW) {
                    let (heavy, light) = to_rumble.convert(&w.haptics);
                    if heavy > 0 || light > 0 {
                        let until = Instant::now() + Duration::from_millis(40);
                        device.feedback.lock().rumbles.push((heavy, light, until));
                        device.kick();
                    }
                }
            }
            if converting {
                timer.sleep_until(Instant::now() + PERIOD);
            } else {
                // Nothing to do until a pulse arrives or the profile
                // changes; check back now and then for the latter.
                let _ = device.haptics_wake.recv_timeout(Duration::from_millis(100));
            }
            deadline = Instant::now();
            continue;
        }

        // Speaker reports while there is speaker sound.
        if speaker_allowed && encoder.is_none() {
            encoder = Encoder::new();
        }
        if !speaker_allowed {
            encoder = None;
        }
        // Speaker reports run the whole time the speaker may play, silent
        // between sounds, as a PS5 sends them: starting the controller's
        // audio for each sound glitches its first moments (heard through
        // its own microphone as a hard edge or a short burst and a gap).
        // Don't wait for the game's first speaker sound either: game haptics
        // ride in these reports, and in the haptics-only `0x32` reports they
        // felt harsher and later (tried and reverted in 0.3.3).
        let media = speaker_allowed && encoder.is_some();
        if media != was_media {
            was_media = media;
            if media {
                squeeze = Squeeze::new();
                lead = device.speaker_lead.load(Ordering::Relaxed).clamp(1, 12) as usize;
                fade_in = true;
            } else {
                if mic_control(&mut counters, recording) {
                    mic_clock.sent(now, microphone);
                }
            }
            // Start the new cadence from now.
            deadline = now;
            device.flight.note(format!("speaker stream {}", if media { "on" } else { "off" }));
            log::info!(
                "controller speaker stream {}",
                if media { "on" } else { "off" }
            );
        }
        if media && lead > 0 {
            // A fresh speaker stream: queue the cushion of silence.
            lead -= 1;
            if let Some(enc) = encoder.as_mut() {
                let silent: Pcm = [0; OPUS_SAMPLES * 2];
                for slot in opus.iter_mut() {
                    enc.encode(&silent, slot);
                }
                let headset = device.speaker_route.load(Ordering::Acquire) == SPEAKER_HEADSET;
                let len =
                    stream::build_audio_with_microphone(&mut counters, &opus, &[[0u8; HAPTIC_FRAME]; BLOCKS], headset,
                    microphone, &mut buf)
                        .len();
                let mut pkt = StreamPacket {
                    len,
                    data: [0u8; MAX_REPORT],
                };
                pkt.data[..len].copy_from_slice(&buf[..len]);
                if device.stream_tx.try_send(pkt).is_ok() {
                    mic_clock.sent(Instant::now(), microphone);
                }
            }
            continue;
        }
        let (period, blocks, win) = if media {
            (PERIOD * BLOCKS as u32, BLOCKS, WINDOW)
        } else {
            (PERIOD, 1, WINDOW)
        };

        // Audio source
        // PC audio is captured for audio-to-vibration, for the speaker, or both.
        if h.source == AudioSource::SystemAudio || pc_speaker {
            if source
                .as_ref()
                .map(|s| s.sub.device != h.device)
                .unwrap_or(true)
            {
                source = Some(Source {
                    sub: engine.audio.subscribe(&h.device),
                    acc: 0.0,
                    primed: false,
                    chunk: Vec::with_capacity(1024),
                });
            }
        } else {
            source = None;
        }
        // Clock
        let now = Instant::now();
        deadline += period;
        if deadline + period < now {
            late += 1;
            if late % 100 == 1 {
                log::debug!("haptics clock fell behind; resyncing");
            }
            deadline = now + period;
        }
        timer.sleep_until(deadline);
        let now = Instant::now();

        // Events from live input and feedback requests.
        let input = device.live.lock().input;
        if h.buttons.enabled {
            let pressed = input.buttons.pressed_since(prev_buttons);
            for b in pressed.iter() {
                // Zone clicks are the same physical press as Touchpad.
                if b != Button::L2 && b != Button::R2 && !b.is_touchpad_zone() {
                    synth.button_click(
                        b,
                        h.buttons.intensity,
                        h.buttons.frequency,
                        h.buttons.duration_ms,
                    );
                }
            }
        }
        prev_buttons = input.buttons;
        synth.triggers(
            input.l2,
            input.r2,
            h.triggers.intensity,
            h.triggers.frequency,
            h.triggers.enabled,
        );
        {
            let mut fb = device.feedback.lock();
            let (mut heavy, mut light) = fb.rumble_now(now);
            if profile.virtual_out.game_rumble {
                let (gh, gl) = fb.game_rumble_now();
                heavy = heavy.max(gh);
                light = light.max(gl);
            }
            synth.rumble(heavy, light, h.rumble_intensity);
            pulses.extend(fb.pulses.drain(..));
        }
        for p in pulses.drain(..) {
            synth.pulse(p.left, p.right, p.freq, p.ms);
        }

        spk.clear();
        let mut speaker_peak = 0f32;
        let mut any_haptics = false;
        for frame in frames.iter_mut().take(blocks) {
            // PC audio window
            pc_spk.clear();
            hap.fill([0.0; 2]);
            if let Some(src) = source.as_mut() {
                let (chunk, rate) = src.take(win, h.latency.prime_windows(), &mut underruns);
                pc_audio.render(h, chunk, rate, &mut hap, &mut pc_spk, win);
            }

            // Game audio from the virtual DualSense: actuator channels into
            // the haptics, speaker channels to the speaker.
            let at = spk.len();
            spk.resize(at + win, [0.0; 2]);
            if let Some(src) = game_src.as_ref() {
                if let Some(w) = game.take(src.as_ref(), win) {
                    if vo.game_haptics {
                        let lvl = vo.game_haptics_level.min(100) as f32 / 100.0;
                        let mut g = [[0f32; 2]; 32];
                        resample_window(&mut prev_g, &w.haptics, &mut g);
                        for (o, v) in hap.iter_mut().zip(g.iter()) {
                            o[0] += v[0] * lvl;
                            o[1] += v[1] * lvl;
                        }
                        if w.peak > 1e-4 {
                            last_sound = now;
                        }
                    }
                    if vo.game_speaker {
                        let lvl = vo.game_speaker_level.min(100) as f32 / 100.0;
                        for (o, v) in spk[at..].iter_mut().zip(w.speaker.iter()) {
                            o[0] += v[0] * lvl;
                            o[1] += v[1] * lvl;
                        }
                    }
                }
            }
            for (o, v) in spk[at..].iter_mut().zip(pc_spk.iter()) {
                o[0] += v[0];
                o[1] += v[1];
            }
            for f in &spk[at..] {
                speaker_peak = speaker_peak.max(f[0].abs()).max(f[1].abs());
            }

            synth.render(&mut hap);
            let mut peak = [0f32; 2];
            for (k, s) in hap.iter().enumerate() {
                let l = soft_clip(s[0]);
                let r = soft_clip(s[1]);
                peak[0] = peak[0].max(l.abs());
                peak[1] = peak[1].max(r.abs());
                frame[2 * k] = stream::haptic_sample(l);
                frame[2 * k + 1] = stream::haptic_sample(r);
            }
            for c in 0..2 {
                levels[c] = peak[c].max(levels[c] * 0.88);
                device.haptic_level[c].store((levels[c] * 255.0) as u8, Ordering::Relaxed);
            }
            any_haptics |= frame.iter().any(|&b| b != 0);
        }
        speaker_meter = speaker_peak.min(1.0).max(speaker_meter * 0.85);
        device
            .speaker_level
            .store((speaker_meter * 255.0) as u8, Ordering::Relaxed);
        let speaker_sound = speaker_peak > SPEAKER_GATE;
        if any_haptics || synth.active() || (media && speaker_sound) {
            last_sound = now;
        }
        if now.duration_since(last_sound) > IDLE_AFTER && !media {
            // Nothing to play: keep the radio quiet unless recording needs refresh.
            if mic_clock.due(now, microphone) && mic_control(&mut counters, recording) {
                mic_clock.sent(now, microphone);
            }
            continue;
        }

        let len = if media {
            // 1024 frames squeezed into 960 (the controller's 21.3 ms tick).
            spk_fit.clear();
            squeeze.run(&spk, &mut spk_fit);
            spk_fit.resize(BLOCKS * OPUS_SAMPLES, [0.0; 2]);
            if fade_in {
                fade_in = false;
                for (k, f) in spk_fit.iter_mut().take(OPUS_SAMPLES).enumerate() {
                    let g = k as f32 / OPUS_SAMPLES as f32;
                    *f = [f[0] * g, f[1] * g];
                }
            }
            let headset = device.speaker_route.load(Ordering::Acquire) == SPEAKER_HEADSET;
            if let Some(enc) = encoder.as_mut() {
                for (k, slot) in opus.iter_mut().enumerate() {
                    for (i, f) in spk_fit[k * OPUS_SAMPLES..(k + 1) * OPUS_SAMPLES].iter().enumerate() {
                        let (l, r) = if headset {
                            (f[0], f[1])
                        } else {
                            // One speaker: a mono mix on both channels (it
                            // plays the right one).
                            let m = 0.5 * (f[0] + f[1]);
                            (m, m)
                        };
                        pcm[2 * i] = to_i16(l);
                        pcm[2 * i + 1] = to_i16(r);
                    }
                    if !enc.encode(&pcm, slot) {
                        device.stats.stream_underruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            stream::build_audio_with_microphone(&mut counters, &opus, &frames, headset,
                microphone, &mut buf).len()
        } else {
            stream::build_with_microphone(&mut counters, &frames[0], microphone, &mut buf).len()
        };
        let mut pkt = StreamPacket {
            len,
            data: [0u8; MAX_REPORT],
        };
        pkt.data[..len].copy_from_slice(&buf[..len]);
        // Never block the clock on a busy radio: drop the packet instead.
        if device.stream_tx.try_send(pkt).is_err() {
            device
                .stats
                .stream_underruns
                .fetch_add(1, Ordering::Relaxed);
        } else {
            mic_clock.sent(Instant::now(), microphone);
        }
        if underruns > 0 {
            device
                .stats
                .stream_underruns
                .fetch_add(underruns, Ordering::Relaxed);
            underruns = 0;
        }
    }
    device.microphone.configure(false, true, 100);
    if was_media || capture_used {
        mic_control(&mut counters, false);
    }
    device.pcm_mode.store(false, Ordering::Release);
    if device.speaker_route.load(Ordering::Acquire) != SPEAKER_UNTOUCHED {
        device.speaker_route.store(SPEAKER_MUTED, Ordering::Release);
        device.kick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pc_tone() -> Vec<[f32; 2]> {
        (0..WINDOW)
            .map(|i| {
                let t = i as f32 / 48_000.0;
                [
                    (t * 300.0 * std::f32::consts::TAU).sin() * 0.25,
                    (t * 600.0 * std::f32::consts::TAU).sin() * 0.5,
                ]
            })
            .collect()
    }

    #[test]
    fn pc_playback_keeps_stereo_without_driving_the_actuators() {
        let mut route = PcAudio::new();
        let mut h = Haptics::default();
        h.source = AudioSource::None;
        h.pc_audio_on_speaker = true;
        let chunk = pc_tone();
        let mut actuators = [[99.0; 2]; 32];
        let mut speaker = Vec::new();
        route.render(&h, &chunk, 48_000, &mut actuators, &mut speaker, WINDOW);
        assert_eq!(actuators, [[0.0; 2]; 32]);
        assert_eq!(speaker.len(), WINDOW);
        assert!(speaker
            .iter()
            .any(|f| f[0].abs() > 0.1 && (f[0] - f[1]).abs() > 0.1));

        // Selecting audio-derived haptics must not alter the audible PCM.
        let mut with_haptics = PcAudio::new();
        h.source = AudioSource::SystemAudio;
        let mut second_speaker = Vec::new();
        with_haptics.render(
            &h,
            &chunk,
            48_000,
            &mut actuators,
            &mut second_speaker,
            WINDOW,
        );
        assert!(actuators.iter().flatten().any(|v| v.abs() > 0.01));
        assert_eq!(speaker, second_speaker);

        // Switching back must clear the previous haptic window immediately.
        h.source = AudioSource::None;
        with_haptics.render(
            &h,
            &chunk,
            48_000,
            &mut actuators,
            &mut second_speaker,
            WINDOW,
        );
        assert_eq!(actuators, [[0.0; 2]; 32]);
        assert!(second_speaker.iter().flatten().any(|v| v.abs() > 0.1));
    }

    #[test]
    fn audio_haptics_and_playback_can_be_disabled_independently() {
        let mut route = PcAudio::new();
        let mut h = Haptics::default();
        h.source = AudioSource::SystemAudio;
        h.pc_audio_on_speaker = false;
        let mut actuators = [[0.0; 2]; 32];
        let mut speaker = vec![[99.0; 2]; WINDOW];
        route.render(&h, &pc_tone(), 48_000, &mut actuators, &mut speaker, WINDOW);
        assert!(actuators.iter().flatten().any(|v| v.abs() > 0.01));
        assert!(speaker.is_empty());
        h.source = AudioSource::None;
        route.render(&h, &pc_tone(), 48_000, &mut actuators, &mut speaker, WINDOW);
        assert_eq!(actuators, [[0.0; 2]; 32]);
        assert!(speaker.is_empty());
    }

    #[test]
    fn microphone_controls_are_opt_in_and_idle_stays_quiet() {
        let start = Instant::now();
        let mut clock = MicrophoneClock::new(start);
        assert!(!clock.due(start, None));
        assert!(!clock.due(start + Duration::from_secs(3600), None));
        // Enabling capture while muted still stops the implicit media admission.
        assert!(clock.due(start, Some(false)));
        clock.sent(start, Some(false));
        assert!(!clock.due(start + Duration::from_secs(3600), Some(false)));
        assert!(clock.due(start, Some(true)));
        clock.sent(start, Some(true));
        assert!(!clock.due(start + Duration::from_millis(999), Some(true)));
        assert!(clock.due(start + Duration::from_secs(1), Some(true)));
        // Active media carries capture; it needs no standalone refresh.
        for tick in 1..=300 {
            let now = start + PERIOD * tick;
            clock.sent(now, Some(true));
            assert!(!clock.due(now, Some(true)));
        }
        let end = start + PERIOD * 300;
        assert!(clock.due(end, Some(false)));
        clock.sent(end, Some(false));
        assert!(!clock.due(end + Duration::from_secs(3600), Some(false)));
    }

    #[test]
    fn volume_spans_the_accepted_range() {
        assert_eq!(volume_byte(1), SPEAKER_VOLUME_MIN);
        assert_eq!(volume_byte(100), SPEAKER_VOLUME_MAX);
        assert!(volume_byte(50) > SPEAKER_VOLUME_MIN && volume_byte(50) < SPEAKER_VOLUME_MAX);
        assert!(volume_byte(1) > SPEAKER_HEADSET, "volume bytes never collide with route codes");
    }
}
