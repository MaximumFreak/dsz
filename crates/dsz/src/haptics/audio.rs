//! System-audio capture (direct WASAPI loopback) fanned out to every
//! controller that wants it. One capture stream serves all controllers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::wasapi;

/// Half a second at 48 kHz; older audio is dropped.
const FIFO_MAX: usize = 24_000;

pub struct Sub {
    pub fifo: Mutex<VecDeque<[f32; 2]>>,
    pub rate: AtomicU32,
    pub device: String,
    /// False for the lightbar meter: the level is updated, samples are not queued.
    queue: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CaptureStatus {
    pub running: bool,
    pub device: String,
    pub rate: u32,
    pub channels: u16,
    pub error: Option<String>,
}

pub struct AudioHub {
    subs: Mutex<Vec<Weak<Sub>>>,
    pub status: Mutex<CaptureStatus>,
    /// Peak of the last callback, f32 bits. The haptics page meter reads this.
    pub peak: AtomicU32,
    /// Smoothed full-band level for the audio lightbar, f32 bits.
    loudness: AtomicU32,
    /// When the last capture buffer arrived, in ms since `epoch`. Loopback
    /// capture delivers nothing while nothing plays, so the level is let
    /// down from here when buffers stop.
    last_audio_ms: AtomicU64,
    epoch: Instant,
    started: AtomicBool,
}

impl AudioHub {
    pub fn new() -> Arc<AudioHub> {
        Arc::new(AudioHub {
            subs: Mutex::new(Vec::new()),
            status: Mutex::new(CaptureStatus::default()),
            peak: AtomicU32::new(0),
            loudness: AtomicU32::new(0),
            last_audio_ms: AtomicU64::new(0),
            epoch: Instant::now(),
            started: AtomicBool::new(false),
        })
    }

    /// Subscribe to system audio from `device` (empty = Windows default).
    /// Dropping the returned `Arc` unsubscribes.
    pub fn subscribe(self: &Arc<Self>, device: &str) -> Arc<Sub> {
        self.subscribe_with(device, true)
    }

    /// Same capture as [`subscribe`](Self::subscribe), without queueing samples.
    pub fn subscribe_meter(self: &Arc<Self>, device: &str) -> Arc<Sub> {
        self.subscribe_with(device, false)
    }

    fn subscribe_with(self: &Arc<Self>, device: &str, queue: bool) -> Arc<Sub> {
        let sub = Arc::new(Sub {
            fifo: Mutex::new(VecDeque::with_capacity(if queue { FIFO_MAX } else { 0 })),
            rate: AtomicU32::new(0),
            device: device.to_string(),
            queue,
        });
        self.subs.lock().push(Arc::downgrade(&sub));
        if !self.started.swap(true, Ordering::AcqRel) {
            let hub = self.clone();
            std::thread::Builder::new()
                .name("audio-capture".into())
                .spawn(move || hub.run())
                .unwrap();
        }
        sub
    }

    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    /// Smoothed system-audio level, 0 at silence. Falls toward 0 once
    /// capture buffers stop arriving (nothing playing).
    pub fn loudness(&self) -> f32 {
        let v = f32::from_bits(self.loudness.load(Ordering::Relaxed));
        let now_ms = self.epoch.elapsed().as_millis() as u64;
        let quiet_ms = now_ms.saturating_sub(self.last_audio_ms.load(Ordering::Relaxed));
        crate::audio_led::after_gap(v, quiet_ms as f32 / 1000.0)
    }

    /// The device to capture. A haptics subscriber's choice wins over a
    /// lightbar meter's, since only haptics plays what is captured.
    fn wanted(&self) -> Option<String> {
        let mut subs = self.subs.lock();
        subs.retain(|w| w.strong_count() > 0);
        let live: Vec<Arc<Sub>> = subs.iter().filter_map(|w| w.upgrade()).collect();
        live.iter()
            .find(|s| s.queue)
            .or(live.first())
            .map(|s| s.device.clone())
    }

    fn push(&self, frames: &[[f32; 2]], rate: u32, scratch: &mut Vec<Arc<Sub>>) {
        scratch.clear();
        {
            let subs = self.subs.lock();
            for w in subs.iter() {
                if let Some(s) = w.upgrade() {
                    scratch.push(s);
                }
            }
        }
        for s in scratch.drain(..) {
            s.rate.store(rate, Ordering::Relaxed);
            if !s.queue {
                continue;
            }
            let mut f = s.fifo.lock();
            f.extend(frames.iter().copied());
            let over = f.len().saturating_sub(FIFO_MAX);
            if over > 0 {
                f.drain(..over);
            }
        }
    }

    pub fn output_devices() -> Vec<String> {
        wasapi::output_device_names()
    }

    fn default_name() -> String {
        wasapi::default_device_name(false)
    }

    fn run(self: Arc<Self>) {
        let mut current: Option<(String, String, wasapi::Stream)> = None; // (wanted, resolved name, stream)
        let failed = Arc::new(AtomicBool::new(false));
        let mut last_check = Instant::now() - Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let want = self.wanted();
            match (&want, &current) {
                (None, Some(_)) => {
                    current = None;
                    self.loudness.store(0.0f32.to_bits(), Ordering::Relaxed);
                    let mut st = self.status.lock();
                    st.running = false;
                    log::info!("system audio capture stopped");
                }
                (None, None) => {}
                (Some(w), cur) => {
                    let mut rebuild = cur.is_none() || failed.load(Ordering::Acquire);
                    if let Some((cw, name, _)) = cur {
                        if cw != w {
                            rebuild = true;
                        } else if w.is_empty() && last_check.elapsed() > Duration::from_secs(2) {
                            // Follow the Windows default output when it changes.
                            last_check = Instant::now();
                            let d = Self::default_name();
                            if !d.is_empty() && d != *name {
                                rebuild = true;
                            }
                        }
                    }
                    if rebuild {
                        current = None;
                        failed.store(false, Ordering::Release);
                        match self.open(w, failed.clone()) {
                            Ok((name, stream)) => current = Some((w.clone(), name, stream)),
                            Err(e) => {
                                self.loudness.store(0.0f32.to_bits(), Ordering::Relaxed);
                                let mut st = self.status.lock();
                                st.running = false;
                                if st.error.as_deref() != Some(e.as_str()) {
                                    log::warn!("system audio capture: {e}");
                                }
                                st.error = Some(e);
                                drop(st);
                                std::thread::sleep(Duration::from_secs(2));
                            }
                        }
                    }
                }
            }
        }
    }

    fn open(
        self: &Arc<Self>,
        want: &str,
        failed: Arc<AtomicBool>,
    ) -> Result<(String, wasapi::Stream), String> {
        let hub = self.clone();
        let mut sub_scratch: Vec<Arc<Sub>> = Vec::with_capacity(4);
        let err_flag = failed.clone();
        let (info, stream) = wasapi::open_loopback(
            want,
            move |frames, rate| {
                let mut peak = 0f32;
                for f in frames {
                    peak = peak.max(f[0].abs()).max(f[1].abs());
                }
                let old = f32::from_bits(hub.peak.load(Ordering::Relaxed));
                hub.peak
                    .store((peak.max(old * 0.85)).to_bits(), Ordering::Relaxed);
                let prev = f32::from_bits(hub.loudness.load(Ordering::Relaxed));
                let level = crate::audio_led::next_level(prev, frames, rate);
                hub.loudness.store(level.to_bits(), Ordering::Relaxed);
                hub.last_audio_ms
                    .store(hub.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
                hub.push(frames, rate, &mut sub_scratch);
            },
            move |e| {
                log::warn!("audio stream error: {e}");
                err_flag.store(true, Ordering::Release);
            },
        )?;
        let name = info.name;
        log::info!(
            "capturing system audio from \"{name}\" at {} Hz, {} ch",
            info.rate,
            info.channels
        );
        *self.status.lock() = CaptureStatus {
            running: true,
            device: name.clone(),
            rate: info.rate,
            channels: info.channels,
            error: None,
        };
        Ok((name, stream))
    }
}
