//! Per-controller packet clock. Every 32/3000 s it turns one window of
//! audio plus event haptics into one Bluetooth haptics report.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ds_proto::input::Button;
use ds_proto::stream::{self, StreamCounters, HAPTIC_FRAME};
use ds_proto::Buttons;

use super::audio::Sub;
use super::dsp::{resample_window, soft_clip, HapticFilter};
use super::synth::Synth;
use crate::device::{Device, StreamPacket, MAX_REPORT};
use crate::engine::{Engine, ProfileCache};
use crate::profile::AudioSource;

const PERIOD: Duration = Duration::from_nanos(stream::PERIOD_NS);
/// Stop sending after this long with nothing to play.
const IDLE_AFTER: Duration = Duration::from_millis(400);

struct Source {
    sub: Arc<Sub>,
    acc: f64,
    primed: bool,
    chunk: Vec<[f32; 2]>,
}

impl Source {
    /// Take one window (about 10.667 ms) of input frames. Empty on underrun.
    fn take(&mut self, prime_windows: usize, underruns: &mut u64) -> (&[[f32; 2]], u32) {
        let rate = self.sub.rate.load(Ordering::Relaxed);
        self.chunk.clear();
        if rate == 0 {
            return (&self.chunk, 48_000);
        }
        self.acc += rate as f64 * 512.0 / 48_000.0;
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

pub fn run(engine: &Arc<Engine>, device: &Arc<Device>) {
    crate::platform::timer_resolution_1ms();
    crate::platform::raise_thread_priority(true);
    let timer = crate::platform::PreciseTimer::new();
    let mut cache = ProfileCache::default();
    let mut counters = StreamCounters::new();
    let mut source: Option<Source> = None;
    let mut filter = HapticFilter::new(48_000.0, 650.0, 25.0);
    let mut synth = Synth::new();
    let mut prev_h = [0f32; 2];
    let mut prev_buttons = Buttons::default();
    let mut deadline = Instant::now();
    let mut last_sound = Instant::now() - Duration::from_secs(10);
    let mut underruns = 0u64;
    let mut buf = [0u8; MAX_REPORT];
    let mut hap = [[0f32; 2]; 32];
    let mut late = 0u32;
    let mut levels = [0f32; 2];
    let mut pulses: VecDeque<crate::device::HapticPulse> = VecDeque::new();
    let mut game = super::game::GameReader::new();
    let mut to_rumble = super::game::ActuatorRumble::new();
    let mut prev_g = [0f32; 2];
    let mut filtered: Vec<[f32; 2]> = Vec::with_capacity(1024);

    // The stream keeps the actuators until this instant; anything that
    // needs it pushes the instant out by `IDLE_AFTER`.
    let mut stream_until = Instant::now();

    while !device.stop.load(Ordering::Acquire) {
        let profile = cache.get(engine, device);
        let h = &profile.haptics;
        let vo = &profile.virtual_out;
        let enabled = h.enabled && device.is_bluetooth();
        let game_src = device.game_audio.lock().clone();
        // The actuators take PCM only while something needs the stream.
        // Otherwise rumble goes to the controller's own rumble emulation,
        // which is what an Xbox game with no haptic modes on gets.
        let continuous = h.source == AudioSource::SystemAudio
            || h.buttons.enabled
            || h.triggers.enabled
            || h.rumble_as_haptics
            || (game_src.is_some() && vo.game_haptics);
        let now = Instant::now();
        if enabled
            && (continuous || !device.feedback.lock().pulses.is_empty() || synth.events_active())
        {
            stream_until = now + IDLE_AFTER;
        }
        let streaming = enabled && now < stream_until;
        device.pcm_mode.store(streaming, Ordering::Release);
        if !streaming {
            source = None;
            device.haptic_level[0].store(0, Ordering::Relaxed);
            device.haptic_level[1].store(0, Ordering::Relaxed);
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
                if let Some(w) = game.take(src.as_ref()) {
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

        // Audio source
        match h.source {
            AudioSource::SystemAudio => {
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
            }
            AudioSource::None => source = None,
        }
        // Clock
        let now = Instant::now();
        deadline += PERIOD;
        if deadline + PERIOD < now {
            late += 1;
            if late % 100 == 1 {
                log::debug!("haptics clock fell behind; resyncing");
            }
            deadline = now + PERIOD;
        }
        timer.sleep_until(deadline);
        let now = Instant::now();

        // Events from live input and feedback requests.
        let input = device.live.lock().input;
        if h.buttons.enabled {
            let pressed = input.buttons.pressed_since(prev_buttons);
            for b in pressed.iter() {
                if b != Button::L2 && b != Button::R2 {
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

        // Audio window
        if let Some(src) = source.as_mut() {
            let (chunk, rate) = src.take(h.latency.prime_windows(), &mut underruns);
            // Haptics: filter at the source rate, then decimate to 3 kHz.
            filter.configure(
                rate as f32,
                h.low_pass_hz.clamp(60.0, 1400.0),
                h.high_pass_hz.clamp(5.0, 300.0),
            );
            filtered.clear();
            for fr in chunk {
                let (l, r) = if h.stereo {
                    (fr[0], fr[1])
                } else {
                    let m = 0.5 * (fr[0] + fr[1]);
                    (m, m)
                };
                let a = filter.run(0, l) * h.gain;
                let b = filter.run(1, r) * h.gain;
                filtered.push([a, b]);
            }
            resample_window(&mut prev_h, &filtered, &mut hap);
        } else {
            hap.fill([0.0; 2]);
        }

        // Game audio from the virtual DualSense: actuator channels into the
        // haptics. The speaker channels are not played.
        if let Some(src) = game_src.as_ref() {
            if let Some(w) = game.take(src.as_ref()) {
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
            }
        }

        synth.render(&mut hap);
        let mut frame = [0u8; HAPTIC_FRAME];
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
        let haptic_nonzero = frame.iter().any(|&b| b != 0);
        if haptic_nonzero || synth.active() {
            last_sound = now;
        }
        if now.duration_since(last_sound) > IDLE_AFTER {
            // Nothing to play: keep the radio quiet.
            continue;
        }

        let len = stream::build(&mut counters, &frame, &mut buf).len();
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
        }
        if underruns > 0 {
            device
                .stats
                .stream_underruns
                .fetch_add(underruns, Ordering::Relaxed);
            underruns = 0;
        }
    }
    device.pcm_mode.store(false, Ordering::Release);
}
