//! `--probe-virtual speaker`: drive the controller speaker directly and
//! record every input report the controller sends back, before, during and
//! after. Nothing is injected (no mouse, keys or virtual pad); this only
//! reads and prints. Quit the app first: the probe opens the controller
//! itself.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use ds_proto::output::AudioRoute;
use ds_proto::stream::{self, StreamCounters, BLOCKS, HAPTIC_FRAME, OPUS_FRAME, OPUS_SAMPLES};
use ds_proto::{Connection, Model, OutputState};

use super::opus::{Encoder, Pcm};
use crate::hid::{self, HidDevice};

#[derive(Default)]
struct Phase {
    /// (report id, length) → count
    kinds: BTreeMap<(u8, usize), u64>,
    crc_bad: u64,
    /// `0x31` reports that are not controller state (microphone audio).
    not_state: u64,
    implausible: u64,
    parsed: u64,
    touch_active: u64,
    buttons_nonzero: u64,
    sticks_off_center: u64,
    samples: Vec<String>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

/// Gaps between the controller's state reports: a proxy for radio
/// trouble that needs neither the microphone nor writing to the pad.
#[derive(Default)]
struct Gaps {
    last: Option<Instant>,
    n: u64,
    over_8: u32,
    over_15: u32,
    over_30: u32,
    max: Duration,
    /// When each gap over 30 ms ended, and how long it was.
    long: Vec<(Instant, Duration)>,
}

impl Gaps {
    fn tick(&mut self, now: Instant) {
        if let Some(l) = self.last {
            let g = now - l;
            self.max = self.max.max(g);
            self.over_8 += (g > Duration::from_millis(8)) as u32;
            self.over_15 += (g > Duration::from_millis(15)) as u32;
            self.over_30 += (g > Duration::from_millis(30)) as u32;
            if g > Duration::from_millis(20) && self.long.len() < 4096 {
                self.long.push((now, g));
            }
        }
        self.last = Some(now);
        self.n += 1;
    }

    fn summary(&self, secs: f32) -> String {
        // Typical spacing of the gaps over 20 ms (a steady rhythm points at
        // something scheduled on the radio).
        let mut every: Vec<f32> = self.long.windows(2).map(|w| (w[1].0 - w[0].0).as_secs_f32()).collect();
        every.sort_by(|a, b| a.total_cmp(b));
        let median = every.get(every.len() / 2).copied().unwrap_or(0.0);
        format!(
            "state reports {:.0}/s · gaps over 8 ms {}, over 15 ms {}, over 30 ms {} · longest {:.1} ms · over 20 ms {} (median spacing {:.2} s)",
            self.n as f32 / secs.max(0.001),
            self.over_8,
            self.over_15,
            self.over_30,
            self.max.as_secs_f64() * 1000.0,
            self.long.len(),
            median
        )
    }
}

pub fn speaker(secs: u64) -> String {
    let mut out = String::new();
    let all = hid::enumerate();
    for i in all.iter().filter(|i| i.vid == ds_proto::SONY_VID) {
        let _ = writeln!(out, "seen {:04X}:{:04X} in {} out {} usage {:X}/{:X}", i.vid, i.pid, i.input_len, i.output_len, i.usage_page, i.usage);
    }
    let Some(info) = all.into_iter().find(|i| {
        i.vid == ds_proto::SONY_VID
            && Model::from_pid(i.pid).is_some()
            && ds_proto::connection_from_input_len(i.input_len) == Connection::Bluetooth
    }) else {
        return format!("{out}No DualSense over Bluetooth found (is DSZ still running and hiding it?)");
    };
    let model = Model::from_pid(info.pid).unwrap();
    let _ = writeln!(
        out,
        "{} over Bluetooth: input {} B, output {} B, feature {} B",
        model.name(),
        info.input_len,
        info.output_len,
        info.feature_len
    );
    let dev = match HidDevice::open(&info) {
        Ok(d) => d,
        Err(e) => return format!("{out}open failed: {e}"),
    };
    // The calibration read switches the controller to the full 0x31 report.
    let _ = dev.get_feature(0x05, 41);

    // 0 baseline, 1 speaker streaming, 2 after (route muted), 3 after the
    // mic-off command.
    let phase = AtomicU8::new(0);
    let done = AtomicBool::new(false);
    let mut phases: [Phase; 4] = Default::default();
    let mut write_errors = 0u64;

    std::thread::scope(|s| {
        // Writer: a 0x31 state report every 100 ms, the speaker report
        // every 20 ms during phase 1.
        let writer = s.spawn(|| {
            let mut w = match dev.writer() {
                Ok(w) => w,
                Err(_) => return 1,
            };
            let mut enc = Encoder::new().expect("opus");
            let mut counters = StreamCounters::new();
            let mut buf = [0u8; stream::MAX_REPORT];
            let mut ctl = [0u8; 128];
            let mut t = 0u64;
            let mut next = Instant::now();
            let mut last_state = Instant::now() - Duration::from_secs(1);
            let mut errors = 0u64;
            let haptics = [[0u8; HAPTIC_FRAME]; BLOCKS];
            let mut mic_off_sent = false;
            while !done.load(Ordering::Acquire) {
                let p = phase.load(Ordering::Acquire);
                let now = Instant::now();
                if now.duration_since(last_state) >= Duration::from_millis(100) {
                    last_state = now;
                    let st = OutputState {
                        lightbar: [0, 40, 80],
                        audio: match p {
                            1 => AudioRoute::Speaker { volume: 0x50 },
                            2 | 3 => AudioRoute::Muted,
                            _ => AudioRoute::Untouched,
                        },
                        ..Default::default()
                    };
                    if w.write(st.build(Connection::Bluetooth, &mut ctl), 50).is_err() {
                        errors += 1;
                    }
                }
                if p == 3 && !mic_off_sent {
                    mic_off_sent = true;
                    for _ in 0..3 {
                        let flags = std::env::var("DSZ_MIC_FLAGS")
                            .ok()
                            .and_then(|v| u8::from_str_radix(&v, 16).ok())
                            .unwrap_or(0);
                        let r = stream::build_audio_control(&mut counters, flags, &mut buf);
                        if w.write(r, 50).is_err() {
                            errors += 1;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
                if p == 1 {
                    let mut opus = [[0u8; OPUS_FRAME]; BLOCKS];
                    for slot in opus.iter_mut() {
                        let mut pcm: Pcm = [0; OPUS_SAMPLES * 2];
                        for f in pcm.chunks_exact_mut(2) {
                            let secs = t as f32 / 48_000.0;
                            let v = ((secs * 440.0 * std::f32::consts::TAU).sin() * 0.4 * 32767.0) as i16;
                            f[0] = v;
                            f[1] = v;
                            t += 1;
                        }
                        enc.encode(&pcm, slot);
                    }
                    let r = stream::build_audio(&mut counters, &opus, &haptics, &mut buf);
                    if w.write(r, 50).is_err() {
                        errors += 1;
                    }
                    next += Duration::from_millis(20);
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        next = now;
                    }
                } else {
                    next = Instant::now();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            errors
        });

        // Reader, on this thread.
        let mut r = match dev.reader() {
            Ok(r) => r,
            Err(_) => {
                done.store(true, Ordering::Release);
                return;
            }
        };
        let start = Instant::now();
        let base = Duration::from_secs(2);
        let streaming = Duration::from_secs(secs);
        let after = base + streaming + Duration::from_secs(2);
        let end = after + Duration::from_secs(3);
        loop {
            let el = start.elapsed();
            if el >= end {
                break;
            }
            let p = if el < base {
                0
            } else if el < base + streaming {
                1
            } else if el < after {
                2
            } else {
                3
            };
            phase.store(p, Ordering::Release);
            let ph = &mut phases[p as usize];
            match r.read(100) {
                Ok(Some(b)) => {
                    let id = b.first().copied().unwrap_or(0);
                    let entry = ph.kinds.entry((id, b.len())).or_insert(0);
                    *entry += 1;
                    if *entry <= 1 {
                        ph.samples.push(format!("first 0x{id:02X} len {}: {}", b.len(), hex(&b[..b.len().min(78)])));
                    }
                    match ds_proto::input::parse(b, Connection::Bluetooth, model, true) {
                        Ok(st) => {
                            ph.parsed += 1;
                            if st.touch[0].active || st.touch[1].active {
                                ph.touch_active += 1;
                            }
                            if st.buttons.0 != 0 {
                                ph.buttons_nonzero += 1;
                            }
                            let off = |v: u8| (v as i16 - 128).abs() > 40;
                            if off(st.lx) || off(st.ly) || off(st.rx) || off(st.ry) {
                                ph.sticks_off_center += 1;
                            }
                            if (st.touch[0].active || st.buttons.0 != 0) && ph.samples.len() < 12 {
                                ph.samples.push(format!(
                                    "odd 0x31 (touch {} {}x{}, buttons {:08X}): {}",
                                    st.touch[0].active,
                                    st.touch[0].x,
                                    st.touch[0].y,
                                    st.buttons.0,
                                    hex(&b[..b.len().min(78)])
                                ));
                            }
                        }
                        Err(ds_proto::input::ParseError::BadCrc) => ph.crc_bad += 1,
                        Err(ds_proto::input::ParseError::NotState(_)) => ph.not_state += 1,
                        Err(ds_proto::input::ParseError::Implausible) => ph.implausible += 1,
                        Err(_) => {}
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    if hid::is_fatal(&e) {
                        break;
                    }
                }
            }
        }
        done.store(true, Ordering::Release);
        write_errors = writer.join().unwrap_or(0);
    });

    for (name, ph) in ["before", "speaker on", "after (muted)", "after mic off"].iter().zip(phases.iter()) {
        let _ = writeln!(out, "\n== {name}");
        for ((id, len), n) in &ph.kinds {
            let _ = writeln!(out, "  report 0x{id:02X} len {len}: {n}");
        }
        let _ = writeln!(
            out,
            "  parsed {} · not state (mic) {} · implausible {} · bad CRC {} · touch active {} · buttons held {} · sticks off centre {}",
            ph.parsed, ph.not_state, ph.implausible, ph.crc_bad, ph.touch_active, ph.buttons_nonzero, ph.sticks_off_center
        );
        for s in &ph.samples {
            let _ = writeln!(out, "  {s}");
        }
    }
    let _ = writeln!(out, "\nwrite errors: {write_errors}");
    out
}

/// One pacing variant for `speaker_ab`.
struct Variant {
    name: &'static str,
    period: Duration,
    /// Input samples per report (squeezed to 960 when not 960).
    samples: usize,
    /// Reports sent at once before pacing starts.
    lead: usize,
    /// Gap between `0x31` control reports.
    control: Duration,
}

/// `--probe-virtual speaker-ab [--variant N]`: pacing variants of a steady
/// 440 Hz tone at full speaker volume, 10 s each, for listening for pops
/// (all four with 3 s of quiet between, or just variant N). Prints write
/// and timing figures per variant. Nothing is injected.
///
/// `secs` sets each variant's length; `mic_off_at` sends `91 01 02` once
/// that far into each variant, to see whether the microphone stays off
/// while `0x39` reports keep coming (mic packets before and after are
/// counted).
pub fn speaker_ab(only: Option<usize>, secs: u64, mic_off_at: Option<Duration>) -> String {
    crate::platform::timer_resolution_1ms();
    let timer = crate::platform::PreciseTimer::new();
    let mut out = String::new();
    let Some(info) = hid::enumerate().into_iter().find(|i| {
        i.vid == ds_proto::SONY_VID
            && Model::from_pid(i.pid).is_some()
            && ds_proto::connection_from_input_len(i.input_len) == Connection::Bluetooth
    }) else {
        return "No DualSense over Bluetooth found (is DSZ still running and hiding it?)".into();
    };
    let dev = match HidDevice::open(&info) {
        Ok(d) => d,
        Err(e) => return format!("open failed: {e}"),
    };
    let _ = dev.get_feature(0x05, 41);
    let variants = [
        Variant { name: "1: 20 ms, control every 16 ms (as DSZ now)", period: Duration::from_millis(20), samples: 960, lead: 0, control: Duration::from_millis(16) },
        Variant { name: "2: 20 ms, 3 reports sent ahead", period: Duration::from_millis(20), samples: 960, lead: 3, control: Duration::from_millis(16) },
        Variant { name: "3: 21.3 ms, 1024 samples squeezed into 960", period: Duration::from_nanos(21_333_333), samples: 1024, lead: 0, control: Duration::from_millis(16) },
        Variant { name: "4: 20 ms, control every 100 ms", period: Duration::from_millis(20), samples: 960, lead: 0, control: Duration::from_millis(100) },
        Variant { name: "5: 21.3 ms, 1024 squeezed into 960, 3 reports ahead", period: Duration::from_nanos(21_333_333), samples: 1024, lead: 3, control: Duration::from_millis(16) },
        Variant { name: "6: 21.3 ms, 960 not squeezed, 3 reports ahead", period: Duration::from_nanos(21_333_333), samples: 960, lead: 3, control: Duration::from_millis(16) },
    ];
    let done = AtomicBool::new(false);
    // Microphone packets heard while a variant plays: (variant, bytes 3..74).
    let current = AtomicU8::new(0);
    let mic: std::sync::Mutex<Vec<(u8, u8, [u8; super::mic::MIC_PACKET])>> = Default::default();
    // Per variant: state-report gaps, and when each mic packet arrived.
    let gaps: std::sync::Mutex<BTreeMap<u8, Gaps>> = Default::default();
    let mic_times: std::sync::Mutex<Vec<(u8, Instant)>> = Default::default();
    std::thread::scope(|s| {
        // Keep reading so the input queue never backs up, and keep the
        // microphone packets: the controller's mic hears its own speaker.
        s.spawn(|| {
            if let Ok(mut r) = dev.reader() {
                while !done.load(Ordering::Acquire) {
                    if let Ok(Some(b)) = r.read(50) {
                        let v = current.load(Ordering::Acquire);
                        if v != 0 && b.len() >= 78 && b[0] == 0x31 && b[1] & 0x0F == 1 {
                            gaps.lock().unwrap().entry(v).or_default().tick(Instant::now());
                        }
                        if b.len() >= 78 && b[0] == 0x31 && b[1] & 0x0F == 2 && v != 0 {
                            mic_times.lock().unwrap().push((v, Instant::now()));
                            let mut p = [0u8; super::mic::MIC_PACKET];
                            p.copy_from_slice(&b[3..74]);
                            mic.lock().unwrap().push((v, b[2], p));
                        }
                    }
                }
            }
        });
        let mut w = match dev.writer() {
            Ok(w) => w,
            Err(e) => {
                let _ = writeln!(out, "writer: {e}");
                done.store(true, Ordering::Release);
                return;
            }
        };
        let mut enc = Encoder::new().expect("opus");
        let mut counters = StreamCounters::new();
        let mut buf = [0u8; stream::MAX_REPORT];
        let mut ctl = [0u8; 128];
        let haptics = [[0u8; HAPTIC_FRAME]; BLOCKS];
        let mut phase = 0f64;
        for (n, v) in variants.iter().enumerate() {
            if only.is_some_and(|o| o != n + 1) {
                continue;
            }
            let mut write_max = Duration::ZERO;
            let mut write_sum = Duration::ZERO;
            let mut slow = 0u32;
            let mut late_max = Duration::ZERO;
            let mut reports = 0u32;
            let mut errors = 0u32;
            current.store(n as u8 + 1, Ordering::Release);
            let start = Instant::now();
            let mut next = start;
            let mut last_ctl = start - Duration::from_secs(1);
            let mut src = vec![[0f32; 2]; v.samples];
            let mut fit = [[0f32; 2]; BLOCKS * OPUS_SAMPLES];
            let mut prev = [0f32; 2];
            let mut sent = 0usize;
            let mut mic_off_sent = false;
            while start.elapsed() < Duration::from_secs(secs) {
                let now = Instant::now();
                if let Some(at) = mic_off_at {
                    if !mic_off_sent && now.duration_since(start) >= at {
                        mic_off_sent = true;
                        if w.write(stream::build_mic_off(&mut counters, &mut buf), 50).is_err() {
                            errors += 1;
                        }
                    }
                }
                if now.duration_since(last_ctl) >= v.control {
                    last_ctl = now;
                    let st = OutputState {
                        lightbar: [0, 40, 80],
                        audio: AudioRoute::Speaker { volume: ds_proto::output::SPEAKER_VOLUME_MAX },
                        ..Default::default()
                    };
                    if w.write(st.build(Connection::Bluetooth, &mut ctl), 50).is_err() {
                        errors += 1;
                    }
                }
                for f in src.iter_mut() {
                    let x = (phase * 440.0 * std::f64::consts::TAU / 48_000.0).sin() as f32 * 0.8;
                    *f = [x, x];
                    phase += 1.0;
                }
                if v.samples == fit.len() {
                    fit.copy_from_slice(&src);
                } else {
                    super::dsp::resample_window(&mut prev, &src, &mut fit);
                }
                let mut opus = [[0u8; OPUS_FRAME]; BLOCKS];
                for (k, slot) in opus.iter_mut().enumerate() {
                    let mut pcm: Pcm = [0; OPUS_SAMPLES * 2];
                    for (i, f) in fit[k * OPUS_SAMPLES..(k + 1) * OPUS_SAMPLES].iter().enumerate() {
                        let x = (f[0] * 32767.0) as i16;
                        pcm[2 * i] = x;
                        pcm[2 * i + 1] = x;
                    }
                    enc.encode(&pcm, slot);
                }
                let r = stream::build_audio(&mut counters, &opus, &haptics, &mut buf);
                let t0 = Instant::now();
                if w.write(r, 100).is_err() {
                    errors += 1;
                }
                let took = t0.elapsed();
                write_max = write_max.max(took);
                write_sum += took;
                if took > Duration::from_millis(10) {
                    slow += 1;
                }
                reports += 1;
                sent += 1;
                if errors > 50 {
                    let _ = writeln!(out, "  stopped: writes are failing");
                    break;
                }
                if sent <= v.lead {
                    continue;
                }
                next += v.period;
                let now = Instant::now();
                if next > now {
                    timer.sleep_until(next);
                    late_max = late_max.max(Instant::now().saturating_duration_since(next));
                } else {
                    late_max = late_max.max(now - next);
                }
            }
            let _ = writeln!(
                out,
                "{}\n  reports {reports} · write avg {:.2} ms, max {:.1} ms, over 10 ms {slow} · most behind {:.1} ms · errors {errors}",
                v.name,
                write_sum.as_secs_f64() * 1000.0 / reports.max(1) as f64,
                write_max.as_secs_f64() * 1000.0,
                late_max.as_secs_f64() * 1000.0
            );
            current.store(0, Ordering::Release);
            let id = n as u8 + 1;
            if let Some(g) = gaps.lock().unwrap().get(&id) {
                let _ = writeln!(out, "  {}", g.summary(start.elapsed().as_secs_f32()));
                // Times from the first mic packet, to line up with the
                // dropout times below.
                let t0 = mic_times.lock().unwrap().iter().find(|x| x.0 == id).map(|x| x.1).unwrap_or(start);
                let mut line = String::from("  state gaps over 20 ms ending at");
                for (at, d) in &g.long {
                    let _ = write!(line, " {:.2}s ({:.0})", at.saturating_duration_since(t0).as_secs_f32(), d.as_secs_f32() * 1000.0);
                }
                let _ = writeln!(out, "{line}");
            }
            if let Some(at) = mic_off_at {
                let t = mic_times.lock().unwrap();
                let split = start + at;
                let before = t.iter().filter(|x| x.0 == id && x.1 < split).count();
                let after = t.iter().filter(|x| x.0 == id && x.1 >= split + Duration::from_millis(200)).count();
                let _ = writeln!(
                    out,
                    "  mic packets: {before} before mic-off at {:.1} s, {after} from 0.2 s after it",
                    at.as_secs_f32()
                );
            }
            {
                let m = mic.lock().unwrap();
                let pk: Vec<super::mic::Packet> =
                    m.iter().filter(|x| x.0 == n as u8 + 1).map(|x| (x.1, x.2)).collect();
                let as_vec: Vec<(u8, Vec<u8>)> = pk.iter().map(|x| (n as u8 + 1, x.1.to_vec())).collect();
                let _ = writeln!(out, "  {}", pitch(&as_vec, n as u8 + 1));
                let _ = writeln!(out, "  {}", super::mic::analyze(&pk).summary);
            }
            // Quiet between variants: mute, mic off.
            let st = OutputState { audio: AudioRoute::Muted, ..Default::default() };
            let _ = w.write(st.build(Connection::Bluetooth, &mut ctl), 50);
            // Several times: a lost mic-off leaves the microphone streaming,
            // and anything else reading the pad may take it for input.
            for _ in 0..5 {
                let _ = w.write(stream::build_mic_off(&mut counters, &mut buf), 50);
                std::thread::sleep(Duration::from_millis(20));
            }
            if only.is_none() {
                std::thread::sleep(Duration::from_secs(3));
            }
        }
        done.store(true, Ordering::Release);
    });
    out
}

/// The tone's frequency as the controller's microphone heard it: decode the
/// mic packets of variant `v`, skip the first second, and find the
/// strongest frequency between 350 and 520 Hz (Goertzel, 0.5 Hz steps).
fn pitch(mic: &[(u8, Vec<u8>)], v: u8) -> String {
    let Some(mut dec) = super::opus::Decoder::new() else {
        return "mic: no decoder".into();
    };
    let mut mono: Vec<f32> = Vec::new();
    let mut buf = [0i16; 960 * 2];
    let mut bad = 0;
    let packets: Vec<&Vec<u8>> = mic.iter().filter(|(x, _)| *x == v).map(|(_, p)| p).collect();
    for p in &packets {
        let n = dec.decode(p, &mut buf);
        if n == 0 {
            bad += 1;
        }
        for f in buf[..n * 2].chunks_exact(2) {
            mono.push((f[0] as f32 + f[1] as f32) / 65536.0);
        }
    }
    if mono.len() < 48_000 * 2 {
        return format!("mic: only {} packets ({} samples, {bad} undecodable)", packets.len(), mono.len());
    }
    let x = &mono[48_000..];
    let n = x.len() as f32;
    let mut best = (0f32, 0f32);
    let mut total = 0f32;
    let mut hz = 350.0f32;
    while hz <= 520.0 {
        let w = std::f32::consts::TAU * hz / 48_000.0;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0f32, 0f32);
        for &v in x {
            let s0 = v + c * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let p = (s1 * s1 + s2 * s2 - c * s1 * s2) / n;
        total += p;
        if p > best.1 {
            best = (hz, p);
        }
        hz += 0.5;
    }
    let rms = (x.iter().map(|v| v * v).sum::<f32>() / n).sqrt();
    format!(
        "mic heard {:.1} Hz (sent 440) · {} packets, {bad} undecodable · level {:.3} rms · peak share {:.0} %",
        best.0,
        packets.len(),
        rms,
        100.0 * best.1 / total.max(1e-12)
    )
}

/// `--probe-virtual mic-listen [--secs N]`: listen only. Opens the
/// controller shared and never writes to it, so another app can drive the
/// speaker meanwhile (for comparing its dropouts with DSZ's). Keeps the
/// microphone packets for N seconds, then runs the same pitch and click
/// analysis as the self-test and saves the audio to
/// `%TEMP%\dsz-mic-listen.wav`.
pub fn mic_listen(secs: u64) -> String {
    let mut out = String::new();
    let Some(info) = hid::enumerate().into_iter().find(|i| {
        i.vid == ds_proto::SONY_VID
            && Model::from_pid(i.pid).is_some()
            && ds_proto::connection_from_input_len(i.input_len) == Connection::Bluetooth
    }) else {
        return "No DualSense over Bluetooth found (hidden by HidHide, or not connected?)".into();
    };
    let dev = match HidDevice::open(&info) {
        Ok(d) => d,
        Err(e) => return format!("open failed: {e}"),
    };
    // `--full`: the calibration read switches a freshly connected pad from
    // the short 0x01 report to 0x31 (a feature read, nothing written).
    if std::env::args().any(|a| a == "--full") {
        let _ = dev.get_feature(0x05, 41);
    }
    let mut r = match dev.reader() {
        Ok(r) => r,
        Err(e) => return format!("reader: {e}"),
    };
    let mut kinds: BTreeMap<(u8, u8, usize), u64> = BTreeMap::new();
    let mut packets: Vec<super::mic::Packet> = Vec::new();
    // Gaps between mic packets, to tell radio loss from a quiet controller.
    let mut last: Option<Instant> = None;
    let mut gap_max = Duration::ZERO;
    let mut gaps_over_30ms = 0u32;
    let mut state = Gaps::default();
    // (arrival, controller sensor timestamp) per state report.
    let mut arrivals: Vec<(Instant, u32)> = Vec::new();
    let model = Model::from_pid(info.pid).unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        match r.read(100) {
            Ok(Some(b)) => {
                let id = b.first().copied().unwrap_or(0);
                let ty = b.get(1).map_or(0, |t| t & 0x0F);
                *kinds.entry((id, ty, b.len())).or_insert(0) += 1;
                if id == 0x31 && ty == 1 {
                    let now = Instant::now();
                    state.tick(now);
                    if let Ok(st) = ds_proto::input::parse(b, Connection::Bluetooth, model, true) {
                        arrivals.push((now, st.sensor_ts));
                    }
                }
                if b.len() >= 78 && id == 0x31 && ty == 2 {
                    let now = Instant::now();
                    if let Some(l) = last {
                        let g = now - l;
                        gap_max = gap_max.max(g);
                        if g > Duration::from_millis(30) {
                            gaps_over_30ms += 1;
                        }
                    }
                    last = Some(now);
                    let mut p = [0u8; super::mic::MIC_PACKET];
                    p.copy_from_slice(&b[3..74]);
                    packets.push((b[2], p));
                }
            }
            Ok(None) => {}
            Err(e) => {
                if hid::is_fatal(&e) {
                    let _ = writeln!(out, "read failed: {e}");
                    break;
                }
            }
        }
    }
    let _ = writeln!(out, "listened {:.1} s", start.elapsed().as_secs_f32());
    let _ = writeln!(out, "{}", state.summary(start.elapsed().as_secs_f32()));
    // Across each long gap: did the controller's clock move as far as the
    // PC's (reports missing), or did the reports arrive late in a burst?
    let mut shown = 0;
    let mut same = 0;
    let mut late = 0;
    for i in 1..arrivals.len() {
        let host = (arrivals[i].0 - arrivals[i - 1].0).as_secs_f64() * 1000.0;
        if host <= 20.0 {
            continue;
        }
        let pad = arrivals[i].1.wrapping_sub(arrivals[i - 1].1) as f64 / 3000.0;
        let burst = arrivals[i..]
            .iter()
            .take_while(|a| a.0 - arrivals[i].0 < Duration::from_millis(3))
            .count();
        if pad > host * 0.6 {
            same += 1;
        } else {
            late += 1;
        }
        if shown < 12 {
            shown += 1;
            let _ = writeln!(out, "  gap: PC {host:.1} ms, controller clock {pad:.1} ms, then {burst} reports within 3 ms");
        }
    }
    let _ = writeln!(out, "  gaps where the controller's clock moved too (reports missing): {same}; reports late in a burst: {late}");
    for ((id, ty, len), n) in &kinds {
        let _ = writeln!(out, "  report 0x{id:02X} type {ty} len {len}: {n}");
    }
    if packets.is_empty() {
        let _ = writeln!(out, "no microphone packets (the other app doesn't turn the mic on)");
        return out;
    }
    let _ = writeln!(
        out,
        "mic packets {} · longest gap {:.1} ms · gaps over 30 ms {gaps_over_30ms}",
        packets.len(),
        gap_max.as_secs_f64() * 1000.0
    );
    let as_vec: Vec<(u8, Vec<u8>)> = packets.iter().map(|x| (1, x.1.to_vec())).collect();
    let _ = writeln!(out, "{}", pitch(&as_vec, 1));
    let rep = super::mic::analyze(&packets);
    let _ = writeln!(out, "{}", rep.summary);
    let wav = std::env::temp_dir().join("dsz-mic-listen.wav");
    if std::fs::write(&wav, super::mic::wav(&rep.audio)).is_ok() {
        let _ = writeln!(out, "audio: {}", wav.display());
    }
    out
}
