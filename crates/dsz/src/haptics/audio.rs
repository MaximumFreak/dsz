//! System-audio capture (WASAPI loopback through cpal) fanned out to every
//! controller that wants it. One capture stream serves all controllers.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

/// Half a second at 48 kHz; older audio is dropped.
const FIFO_MAX: usize = 24_000;

pub struct Sub {
    pub fifo: Mutex<VecDeque<[f32; 2]>>,
    pub rate: AtomicU32,
    pub device: String,
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
    /// Peak of the last callback, f32 bits.
    pub peak: AtomicU32,
    started: AtomicBool,
}

impl AudioHub {
    pub fn new() -> Arc<AudioHub> {
        Arc::new(AudioHub {
            subs: Mutex::new(Vec::new()),
            status: Mutex::new(CaptureStatus::default()),
            peak: AtomicU32::new(0),
            started: AtomicBool::new(false),
        })
    }

    /// Subscribe to system audio from `device` (empty = Windows default).
    /// Dropping the returned `Arc` unsubscribes.
    pub fn subscribe(self: &Arc<Self>, device: &str) -> Arc<Sub> {
        let sub = Arc::new(Sub {
            fifo: Mutex::new(VecDeque::with_capacity(FIFO_MAX)),
            rate: AtomicU32::new(0),
            device: device.to_string(),
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

    fn wanted(&self) -> Option<String> {
        let mut subs = self.subs.lock();
        subs.retain(|w| w.strong_count() > 0);
        subs.iter()
            .filter_map(|w| w.upgrade())
            .map(|s| s.device.clone())
            .next()
    }

    fn push(&self, frames: &[[f32; 2]], rate: u32) {
        let subs: Vec<Arc<Sub>> = self
            .subs
            .lock()
            .iter()
            .filter_map(|w| w.upgrade())
            .collect();
        for s in subs {
            s.rate.store(rate, Ordering::Relaxed);
            let mut f = s.fifo.lock();
            f.extend(frames.iter().copied());
            let over = f.len().saturating_sub(FIFO_MAX);
            if over > 0 {
                f.drain(..over);
            }
        }
    }

    pub fn output_devices() -> Vec<String> {
        let host = cpal::default_host();
        host.output_devices()
            .map(|it| it.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default()
    }

    fn find_device(name: &str) -> Option<cpal::Device> {
        let host = cpal::default_host();
        if name.is_empty() {
            return host.default_output_device();
        }
        host.output_devices()
            .ok()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .or_else(|| host.default_output_device())
    }

    fn default_name() -> String {
        cpal::default_host()
            .default_output_device()
            .and_then(|d| d.name().ok())
            .unwrap_or_default()
    }

    fn run(self: Arc<Self>) {
        let mut current: Option<(String, String, cpal::Stream)> = None; // (wanted, resolved name, stream)
        let failed = Arc::new(AtomicBool::new(false));
        let mut last_check = Instant::now() - Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let want = self.wanted();
            match (&want, &current) {
                (None, Some(_)) => {
                    current = None;
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
    ) -> Result<(String, cpal::Stream), String> {
        let dev = Self::find_device(want).ok_or("no audio output device")?;
        let name = dev.name().unwrap_or_default();
        let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
        let rate = cfg.sample_rate().0;
        let channels = cfg.channels().max(1);
        let config: cpal::StreamConfig = cfg.config();
        let err_flag = failed.clone();
        let err_fn = move |e: cpal::StreamError| {
            log::warn!("audio stream error: {e}");
            err_flag.store(true, Ordering::Release);
        };
        let hub = self.clone();
        let ch = channels as usize;
        let mut scratch: Vec<[f32; 2]> = Vec::with_capacity(4096);
        let mut handle = move |data: &[f32]| {
            scratch.clear();
            let mut peak = 0f32;
            for frame in data.chunks_exact(ch) {
                let (l, r) = if ch == 1 {
                    (frame[0], frame[0])
                } else {
                    (frame[0], frame[1])
                };
                peak = peak.max(l.abs()).max(r.abs());
                scratch.push([l, r]);
            }
            let old = f32::from_bits(hub.peak.load(Ordering::Relaxed));
            hub.peak
                .store((peak.max(old * 0.85)).to_bits(), Ordering::Relaxed);
            hub.push(&scratch, rate);
        };
        let stream = match cfg.sample_format() {
            cpal::SampleFormat::F32 => dev.build_input_stream(
                &config,
                move |d: &[f32], _: &cpal::InputCallbackInfo| handle(d),
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => {
                let mut buf: Vec<f32> = Vec::new();
                dev.build_input_stream(
                    &config,
                    move |d: &[i16], _: &cpal::InputCallbackInfo| {
                        buf.clear();
                        buf.extend(d.iter().map(|&s| s as f32 / 32768.0));
                        handle(&buf)
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I32 => {
                let mut buf: Vec<f32> = Vec::new();
                dev.build_input_stream(
                    &config,
                    move |d: &[i32], _: &cpal::InputCallbackInfo| {
                        buf.clear();
                        buf.extend(d.iter().map(|&s| s as f32 / 2_147_483_648.0));
                        handle(&buf)
                    },
                    err_fn,
                    None,
                )
            }
            f => return Err(format!("unsupported sample format {f:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        log::info!("capturing system audio from \"{name}\" at {rate} Hz, {channels} ch");
        *self.status.lock() = CaptureStatus {
            running: true,
            device: name.clone(),
            rate,
            channels,
            error: None,
        };
        Ok((name, stream))
    }
}
