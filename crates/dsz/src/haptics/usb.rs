//! Game haptics for a DualSense on a USB cable, whenever the profile's
//! Game haptics is on, as over Bluetooth.
//!
//! Over Bluetooth the streamer turns the virtual DualSense's actuator
//! channels into haptics reports. On a cable the controller is a Windows
//! sound device with four channels, the last two its actuators, which is
//! where a game writes them on a wired DualSense. Without this the game's
//! haptic audio stops at the virtual pad, and a wired controller gets only
//! the game's classic rumble. This plays the actuator channels on the
//! controller's own playback endpoint (found by its device container, so
//! never another controller's or a virtual pad's), at the profile's
//! Haptics level. The speaker channels are left silent, as before.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use super::dsp::{full_clip, resample_window, Limiter};
use super::game::{GameReader, PcmSource, WINDOW};
use super::streamer::{haptic_drive, HAPTICS_CEILING, LEVEL_MAX};
use crate::device::Device;
use crate::engine::{Engine, ProfileCache};
use crate::profile::{VirtualKind, VirtualOutput};

/// How often the profile and the game's audio endpoint are looked at.
const POLL: Duration = Duration::from_millis(250);
/// Wait before trying the controller's sound device again after a failure.
const RETRY: Duration = Duration::from_secs(10);

/// Whether the actuator channels should play on a wired controller now.
pub fn wanted(vo: &VirtualOutput, has_game_audio: bool, game_audio_open: bool) -> bool {
    vo.kind == VirtualKind::DualSense
        && vo.game_haptics
        && has_game_audio
        && game_audio_open
}

/// One actuator sample at `level` percent for the wired controller: plain
/// scaling up to 100 % (clamped to full scale, as the sound card would),
/// past 100 % the same limiter and full-range knee as over Bluetooth.
fn shape(x: f32, level: u8, drive: f32) -> f32 {
    let v = x * level.min(LEVEL_MAX) as f32 / 100.0;
    let clamped = v.clamp(-1.0, 1.0);
    if drive <= 0.0 {
        return if clamped.is_nan() { 0.0 } else { clamped };
    }
    let y = clamped + (full_clip(v) - clamped) * drive.min(1.0);
    if y.is_nan() {
        0.0
    } else {
        y.clamp(-1.0, 1.0)
    }
}

/// State shared between the control thread and the audio callback.
struct Shared {
    source: Mutex<Option<Arc<dyn PcmSource>>>,
    level: AtomicU8,
    failed: AtomicBool,
    /// Peak of the last window per actuator, f32 bits (for the meters).
    peak: [AtomicU32; 2],
}

/// The audio callback's side: the game reader and what is left of its
/// last window.
struct Render {
    shared: Arc<Shared>,
    reader: GameReader,
    queue: VecDeque<[f32; 2]>,
    prev: [f32; 2],
    window: Vec<[f32; 2]>,
    limiter: Limiter,
    rate: u32,
}

impl Render {
    fn new(shared: Arc<Shared>, rate: u32) -> Self {
        Render {
            shared,
            reader: GameReader::new(),
            queue: VecDeque::with_capacity(4 * WINDOW),
            prev: [0.0; 2],
            window: Vec::with_capacity(2 * WINDOW),
            limiter: Limiter::new(rate as f32, 0.08, HAPTICS_CEILING),
            rate,
        }
    }

    /// One window of actuator samples at the device rate, or silence.
    fn refill(&mut self) {
        let frames = ((WINDOW as u64 * self.rate as u64) / 48_000).max(1) as usize;
        let src = self.shared.source.lock().clone();
        let level = self.shared.level.load(Ordering::Relaxed);
        self.window.clear();
        self.window.resize(frames, [0.0; 2]);
        let mut peak = [0f32; 2];
        if let Some(w) = src.as_ref().and_then(|s| self.reader.take(s.as_ref(), WINDOW)) {
            resample_window(&mut self.prev, &w.haptics, &mut self.window);
            if level > 100 {
                let lvl = level.min(LEVEL_MAX) as f32 / 100.0;
                for f in self.window.iter_mut() {
                    *f = [f[0] * lvl, f[1] * lvl];
                }
                self.limiter.run(&mut self.window);
                let drive = haptic_drive(Some(level), 1.0);
                for f in self.window.iter_mut() {
                    *f = [shape(f[0], 100, drive), shape(f[1], 100, drive)];
                }
            } else {
                for f in self.window.iter_mut() {
                    *f = [shape(f[0], level, 0.0), shape(f[1], level, 0.0)];
                }
            }
            for f in &self.window {
                peak[0] = peak[0].max(f[0].abs());
                peak[1] = peak[1].max(f[1].abs());
            }
        } else {
            self.prev = [0.0; 2];
        }
        for c in 0..2 {
            self.shared.peak[c].store(peak[c].to_bits(), Ordering::Relaxed);
        }
        self.queue.extend(self.window.iter().copied());
    }

    /// Interleaved frames of `channels`; the actuators are channels 3 and 4.
    fn fill(&mut self, data: &mut [f32], channels: usize) {
        for frame in data.chunks_mut(channels) {
            if self.queue.is_empty() {
                self.refill();
            }
            let h = self.queue.pop_front().unwrap_or([0.0; 2]);
            for (c, s) in frame.iter_mut().enumerate() {
                *s = match c {
                    2 => h[0],
                    3 => h[1],
                    _ => 0.0,
                };
            }
        }
    }
}

/// Why the controller's sound card couldn't be opened.
enum OpenError {
    /// Set to fewer than the 4 channels the actuators need: the one case
    /// the user can fix, so it gets a toast too.
    Channels(String),
    Other(String),
}

impl From<String> for OpenError {
    fn from(e: String) -> Self {
        OpenError::Other(e)
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Channels(e) | OpenError::Other(e) => f.write_str(e),
        }
    }
}

/// An open playback stream on the controller's sound card.
struct Output {
    _stream: cpal::Stream,
    name: String,
}

/// Name of the wired controller's own playback endpoint.
fn endpoint_name(device: &Device) -> Result<String, String> {
    let id = crate::virt::win::instance_id_for_interface(&device.info.path)
        .ok_or("the controller's device node wasn't found")?;
    let container = crate::virt::win::container_id(&id).ok_or("the controller has no device container")?;
    let mut names = crate::virt::audio_default::render_endpoints_in_container(&container);
    match names.len() {
        1 => Ok(names.remove(0)),
        0 => Err("the controller's sound device isn't active (is it disabled in Sound settings?)".into()),
        _ => Err(format!("the controller has {} playback devices: {names:?}", names.len())),
    }
}

impl Output {
    fn open(device: &Device, shared: Arc<Shared>) -> Result<Output, OpenError> {
        let name = endpoint_name(device)?;
        let host = cpal::default_host();
        let mut found: Vec<cpal::Device> = host
            .output_devices()
            .map_err(|e| e.to_string())?
            .filter(|d| d.name().map(|n| n == name).unwrap_or(false))
            .collect();
        if found.len() != 1 {
            return Err(format!("{} playback devices are named \"{name}\"", found.len()).into());
        }
        let out = found.remove(0);
        let config = out.default_output_config().map_err(|e| e.to_string())?;
        let channels = config.channels() as usize;
        if channels < 4 {
            return Err(OpenError::Channels(format!(
                "\"{name}\" is set to {channels} channels; the actuators need 4 (Sound settings → the controller → Configure → Quadraphonic)"
            )));
        }
        let rate = config.sample_rate().0;
        let mut render = Render::new(shared.clone(), rate);
        let failed = shared.clone();
        let on_error = move |e: cpal::StreamError| {
            log::warn!("game haptics over USB: playback error: {e}");
            failed.failed.store(true, Ordering::Release);
        };
        let cfg: cpal::StreamConfig = config.config();
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => out.build_output_stream(
                &cfg,
                move |data: &mut [f32], _| render.fill(data, channels),
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => {
                let mut buf: Vec<f32> = Vec::new();
                out.build_output_stream(
                    &cfg,
                    move |data: &mut [i16], _| {
                        buf.resize(data.len(), 0.0);
                        render.fill(&mut buf, channels);
                        for (d, s) in data.iter_mut().zip(&buf) {
                            *d = (s * 32767.0).round() as i16;
                        }
                    },
                    on_error,
                    None,
                )
            }
            f => return Err(format!("unsupported sample format {f:?}").into()),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        log::info!("game haptics over USB: playing the actuator channels on \"{name}\" ({channels} ch, {rate} Hz)");
        Ok(Output { _stream: stream, name })
    }
}

/// Runs for a DualSense on USB until it disconnects. Does nothing unless
/// the profile's Game haptics is on and a game has the virtual DualSense's
/// audio open.
pub fn run(engine: &Arc<Engine>, device: &Arc<Device>) {
    let mut cache = ProfileCache::default();
    let shared = Arc::new(Shared {
        source: Mutex::new(None),
        level: AtomicU8::new(100),
        failed: AtomicBool::new(false),
        peak: [AtomicU32::new(0), AtomicU32::new(0)],
    });
    let mut out: Option<Output> = None;
    let mut retry_at = Instant::now();
    let mut warned = false;
    let mut levels = [0f32; 2];
    let mut was_open = false;
    let mut haptics_logged = false;
    while !device.stop.load(Ordering::Acquire) {
        let profile = cache.get(engine, device);
        let vo = &profile.virtual_out;
        let src = device.game_audio.lock().clone();
        let open = device.virt.status.lock().game_audio_open;
        if open && src.is_some() && !was_open && vo.kind == VirtualKind::DualSense {
            haptics_logged = false;
            super::log_game_settings(engine, device, &profile, "a game opened the virtual DualSense's sound device");
        }
        was_open = open && src.is_some();
        shared.level.store(vo.game_haptics_level, Ordering::Relaxed);
        if wanted(vo, src.is_some(), open) {
            *shared.source.lock() = src;
            if out.is_none() && Instant::now() >= retry_at {
                shared.failed.store(false, Ordering::Release);
                match Output::open(device, shared.clone()) {
                    Ok(o) => {
                        warned = false;
                        out = Some(o);
                    }
                    Err(e) => {
                        if !warned {
                            log::warn!("game haptics over USB: can't play on the controller: {e}");
                            if let OpenError::Channels(_) = e {
                                engine.warn_toast(crate::i18n::tr(
                                    "Game haptics over USB need the controller's sound device set to 4 channels (Quadraphonic)",
                                ));
                            }
                            warned = true;
                        }
                        retry_at = Instant::now() + RETRY;
                    }
                }
            }
        } else if let Some(o) = out.take() {
            log::info!("game haptics over USB: stopped on \"{}\"", o.name);
            *shared.source.lock() = None;
        }
        if out.is_some() && shared.failed.load(Ordering::Acquire) {
            // The sound card went away or Windows reset it: open it again.
            out = None;
            retry_at = Instant::now() + Duration::from_secs(1);
        }
        // The Haptics page meters, as over Bluetooth.
        for c in 0..2 {
            let p = if out.is_some() {
                f32::from_bits(shared.peak[c].load(Ordering::Relaxed))
            } else {
                0.0
            };
            if p > 1e-3 && !haptics_logged {
                haptics_logged = true;
                log::info!("the game sends haptics as audio; playing them on the wired controller (peak {:.0} %)", p * 100.0);
            }
            levels[c] = p.max(levels[c] * 0.5);
            device.haptic_level[c].store((levels[c].min(1.0) * 255.0) as u8, Ordering::Relaxed);
        }
        std::thread::sleep(POLL);
    }
    if let Some(o) = out.take() {
        log::info!("game haptics over USB: stopped on \"{}\"", o.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tone {
        frames: Mutex<usize>,
    }

    impl PcmSource for Tone {
        fn filled_frames(&self) -> usize {
            *self.frames.lock()
        }
        fn read(&self, out: &mut [i16]) -> usize {
            let n = (out.len() / 4).min(*self.frames.lock());
            for (i, f) in out.chunks_mut(4).take(n).enumerate() {
                let v = if i % 2 == 0 { 16_384 } else { -16_384 };
                f.copy_from_slice(&[1000, 1000, v, -v]);
            }
            *self.frames.lock() -= n;
            n
        }
        fn drop_frames(&self, n: usize) {
            let mut f = self.frames.lock();
            *f -= n.min(*f);
        }
    }

    fn shared(level: u8, frames: usize) -> Arc<Shared> {
        Arc::new(Shared {
            source: Mutex::new(Some(Arc::new(Tone { frames: Mutex::new(frames) }) as Arc<dyn PcmSource>)),
            level: AtomicU8::new(level),
            failed: AtomicBool::new(false),
            peak: [AtomicU32::new(0), AtomicU32::new(0)],
        })
    }

    #[test]
    fn plays_with_game_haptics_on_while_a_game_plays() {
        let mut vo = VirtualOutput::default();
        vo.kind = VirtualKind::DualSense;
        assert!(vo.game_haptics, "on by default");
        assert!(wanted(&vo, true, true));
        assert!(!wanted(&vo, true, false), "no game has the audio open");
        assert!(!wanted(&vo, false, true));
        vo.game_haptics = false;
        assert!(!wanted(&vo, true, true), "game haptics off");
        vo.game_haptics = true;
        vo.kind = VirtualKind::Xbox360;
        assert!(!wanted(&vo, true, true));
    }

    #[test]
    fn actuators_go_to_channels_3_and_4_at_the_level() {
        let s = shared(100, 48_000);
        let mut r = Render::new(s.clone(), 48_000);
        let mut data = vec![1.0f32; 4 * 2048];
        r.fill(&mut data, 4);
        let frames: Vec<&[f32]> = data.chunks(4).collect();
        assert!(frames.iter().all(|f| f[0] == 0.0 && f[1] == 0.0), "speaker channels silent");
        let peak = |c: usize| frames.iter().map(|f| f[c].abs()).fold(0.0f32, f32::max);
        assert!((peak(2) - 0.5).abs() < 0.01, "{}", peak(2));
        assert!((peak(3) - 0.5).abs() < 0.01);
        // Half level halves it; 200 % doubles a half-scale effect to the top.
        let mut half = Render::new(shared(50, 48_000), 48_000);
        let mut d = vec![0.0f32; 4 * 2048];
        half.fill(&mut d, 4);
        let p = d.chunks(4).map(|f| f[2].abs()).fold(0.0f32, f32::max);
        assert!((p - 0.25).abs() < 0.01, "{p}");
        let mut boosted = Render::new(shared(200, 48_000), 48_000);
        let mut d = vec![0.0f32; 4 * 2048];
        boosted.fill(&mut d, 4);
        let p = d.chunks(4).map(|f| f[2].abs()).fold(0.0f32, f32::max);
        assert!(p > 0.9 && p <= 1.0, "{p}");
    }

    #[test]
    fn silence_until_the_game_has_written_enough_and_without_a_game() {
        let mut r = Render::new(shared(100, 100), 48_000);
        let mut d = vec![1.0f32; 4 * 600];
        r.fill(&mut d, 4);
        assert!(d.iter().all(|&v| v == 0.0));
        let none = Arc::new(Shared {
            source: Mutex::new(None),
            level: AtomicU8::new(100),
            failed: AtomicBool::new(false),
            peak: [AtomicU32::new(0), AtomicU32::new(0)],
        });
        let mut r = Render::new(none, 44_100);
        let mut d = vec![1.0f32; 6 * 600];
        r.fill(&mut d, 6);
        assert!(d.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn shape_is_plain_scaling_up_to_100() {
        for i in -150..=150 {
            let x = i as f32 / 100.0;
            assert_eq!(shape(x, 100, 0.0), x.clamp(-1.0, 1.0));
            assert_eq!(shape(x, 50, 0.0), (x * 0.5).clamp(-1.0, 1.0));
        }
        assert_eq!(shape(f32::NAN, 100, 0.0), 0.0);
        assert_eq!(shape(f32::NAN, 100, 1.0), 0.0);
        assert!(shape(1.6, 100, 1.0) > 0.99);
    }
}
