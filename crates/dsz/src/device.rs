//! One connected controller: shared state plus the reader, writer, and
//! haptics threads that serve it.
//!
//! Reliability rules:
//! - A read timeout is not a failure. The read stays queued; a quiet radio
//!   just shows "no signal" until reports come back.
//! - Write errors back off and retry. They never remove the device.
//! - The device goes away only when Windows says the node is gone.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use ds_proto::input::ParseError;
use ds_proto::{Connection, InputState, Model};
use parking_lot::Mutex;

use crate::composer::Composer;
use crate::engine::Engine;
use crate::hid::{self, DeviceInfo, HidDevice};
use crate::virt::mirror::Mirror;

pub const MAX_REPORT: usize = ds_proto::stream::MAX_REPORT;

pub struct StreamPacket {
    pub len: usize,
    pub data: [u8; MAX_REPORT],
}

#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub left_trigger: Option<[u8; 11]>,
    pub right_trigger: Option<[u8; 11]>,
    pub rgb: Option<[u8; 3]>,
    pub player_leds: Option<u8>,
    pub mic_led: Option<u8>,
    pub last_update: Option<Instant>,
}

impl Overrides {
    pub fn any(&self) -> bool {
        self.left_trigger.is_some()
            || self.right_trigger.is_some()
            || self.rgb.is_some()
            || self.player_leds.is_some()
            || self.mic_led.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HapticPulse {
    pub left: f32,
    pub right: f32,
    pub freq: f32,
    pub ms: f32,
}

impl HapticPulse {
    /// The same pulse as a short classic rumble, for when nothing streams:
    /// (heavy, light, until).
    pub fn as_rumble(&self, now: Instant) -> (u8, u8, Instant) {
        let level = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        (
            level(self.left),
            level(self.right),
            now + Duration::from_millis(self.ms.clamp(30.0, 5000.0) as u64),
        )
    }
}

/// What a game sent to the virtual controller.
#[derive(Debug, Default, Clone)]
pub struct GameState {
    /// (heavy, light). Holds until the game changes it, as on a real pad;
    /// cleared when the game closes the virtual controller.
    pub rumble: Option<(u8, u8)>,
    pub right_trigger: Option<[u8; 11]>,
    pub left_trigger: Option<[u8; 11]>,
    pub rgb: Option<[u8; 3]>,
    pub player_leds: Option<u8>,
    pub mute_led: Option<u8>,
    pub reports: u64,
}

#[derive(Debug, Default)]
pub struct Feedback {
    /// (heavy, light, until)
    pub rumbles: Vec<(u8, u8, Instant)>,
    pub identify_until: Option<Instant>,
    pub pulses: Vec<HapticPulse>,
    pub trigger_preview: Option<(Option<[u8; 11]>, Option<[u8; 11]>, Instant)>,
    pub game: GameState,
}

impl Feedback {
    pub fn rumble_now(&self, now: Instant) -> (u8, u8) {
        self.rumbles
            .iter()
            .filter(|r| r.2 > now)
            .fold((0, 0), |a, r| (a.0.max(r.0), a.1.max(r.1)))
    }
    /// Game rumble in force.
    pub fn game_rumble_now(&self) -> (u8, u8) {
        self.game.rumble.unwrap_or((0, 0))
    }
    pub fn clear_game(&mut self) {
        self.game = GameState::default();
    }
    pub fn prune(&mut self, now: Instant) {
        self.rumbles.retain(|r| r.2 > now);
        if self.identify_until.map(|u| u <= now).unwrap_or(false) {
            self.identify_until = None;
        }
        if self.trigger_preview.map(|t| t.2 <= now).unwrap_or(false) {
            self.trigger_preview = None;
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Toggles {
    pub gyro: Option<bool>,
    pub touchpad: Option<bool>,
    pub mute_engaged: bool,
    pub gyro_toggled_on: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Opening,
    Live,
    /// Open, but no input for a while (asleep, out of range, or interference).
    Quiet,
    Error(String),
}

#[derive(Clone, Debug, Default)]
pub struct Live {
    pub input: InputState,
    /// Bias-corrected angular velocity, deg/s, wire order (pitch, yaw, roll).
    pub gyro_dps: [f32; 3],
    pub gyro_bias: [f32; 3],
    pub calibrating: bool,
    pub gyro_active: bool,
    pub touch_mouse_active: bool,
}

#[derive(Default)]
pub struct Stats {
    pub reports: AtomicU64,
    pub crc_errors: AtomicU64,
    pub writes: AtomicU64,
    pub write_errors: AtomicU64,
    pub stream_packets: AtomicU64,
    pub stream_underruns: AtomicU64,
    pub input_hz: AtomicU32,
    pub output_hz: AtomicU32,
    pub stream_hz: AtomicU32,
    /// Microseconds, smoothed write duration.
    pub write_us: AtomicU32,
}

pub struct Device {
    pub id: u64,
    pub info: DeviceInfo,
    pub model: Model,
    pub conn: Connection,
    pub mac: Mutex<String>,
    pub firmware: Mutex<String>,
    pub live: Mutex<Live>,
    pub stats: Stats,
    pub link: Mutex<Link>,
    pub overrides: Mutex<Overrides>,
    pub feedback: Mutex<Feedback>,
    pub toggles: Mutex<Toggles>,
    pub stream_tx: Sender<StreamPacket>,
    stream_rx: Receiver<StreamPacket>,
    /// Wakes the writer as soon as the control report may have changed.
    kick_tx: Sender<()>,
    kick_rx: Receiver<()>,
    /// Wakes an idle haptics thread when a pulse arrives.
    haptics_wake_tx: Sender<()>,
    pub(crate) haptics_wake: Receiver<()>,
    /// The haptics thread owns the actuators (PCM mode).
    pub pcm_mode: AtomicBool,
    /// Haptic output level 0..=255 per side, for meters.
    pub haptic_level: [AtomicU8; 2],
    pub calibrate_request: AtomicBool,
    /// Calibration feature report (`0x05`) read at connect.
    /// Feature reports a virtual DualSense driven by this controller answers with.
    pub mirror: Mutex<Mirror>,
    /// The virtual controller standing in for this one, if any.
    pub virt: crate::virt::Slot,
    /// Game audio for the haptics thread: a virtual DualSense's PCM ring,
    /// or the test signal.
    pub game_audio: Mutex<Option<Arc<dyn crate::haptics::game::PcmSource>>>,
    pub force_output: AtomicBool,
    pub stop: AtomicBool,
    pub dead: AtomicBool,
}

impl Device {
    pub fn display_name(&self, engine: &Engine) -> String {
        let mac = self.mac.lock().clone();
        if let Some(n) = engine.settings.read().device_names.get(&mac) {
            if !n.trim().is_empty() {
                return n.clone();
            }
        }
        self.model.name().to_string()
    }

    /// Wake the writer now instead of at its next tick. Call after changing
    /// anything the control report carries (feedback, overrides, toggles).
    pub fn kick(&self) {
        let _ = self.kick_tx.try_send(());
    }

    pub fn identify(&self) {
        let until = Instant::now() + Duration::from_millis(1500);
        let mut f = self.feedback.lock();
        f.identify_until = Some(until);
        f.rumbles
            .push((90, 160, Instant::now() + Duration::from_millis(250)));
        drop(f);
        self.pulse(HapticPulse {
            left: 0.7,
            right: 0.7,
            freq: 180.0,
            ms: 160.0,
        });
        self.kick();
    }

    pub fn test_rumble(&self, heavy: u8, light: u8, ms: u64) {
        self.feedback.lock().rumbles.push((
            heavy,
            light,
            Instant::now() + Duration::from_millis(ms),
        ));
        self.kick();
    }

    /// A haptic pulse. Over Bluetooth the haptics thread plays it (as PCM,
    /// or as a short rumble while nothing streams); over USB it is a rumble.
    pub fn pulse(&self, p: HapticPulse) {
        let mut f = self.feedback.lock();
        if self.is_bluetooth() {
            // The haptics thread drains these every 10 ms; cap a backlog.
            if f.pulses.len() < 32 {
                f.pulses.push(p);
            }
            let _ = self.haptics_wake_tx.try_send(());
        } else {
            f.rumbles.push(p.as_rumble(Instant::now()));
            drop(f);
            self.kick();
        }
    }

    pub fn snapshot(&self) -> Live {
        self.live.lock().clone()
    }

    pub fn is_bluetooth(&self) -> bool {
        self.conn == Connection::Bluetooth
    }

    /// Rumble from the game.
    pub fn set_game_rumble(&self, heavy: u8, light: u8) {
        let mut f = self.feedback.lock();
        f.game.rumble = (heavy != 0 || light != 0).then_some((heavy, light));
        drop(f);
        self.kick();
    }

    /// Lightbar color from the game (a virtual DualShock 4).
    pub fn set_game_lightbar(&self, rgb: [u8; 3]) {
        let mut f = self.feedback.lock();
        if f.game.rgb == Some(rgb) {
            return;
        }
        f.game.rgb = Some(rgb);
        drop(f);
        self.kick();
    }

    /// One USB output report (`0x02`) the game wrote to the virtual DualSense.
    pub fn apply_game_output(&self, report: &[u8]) {
        let Some(o) = ds_proto::virtual_pad::parse_ds_usb_output(report) else {
            return;
        };
        let mut f = self.feedback.lock();
        let g = &mut f.game;
        g.reports += 1;
        let (heavy, light) = o.rumble;
        g.rumble = (heavy != 0 || light != 0).then_some((heavy, light));
        if let Some(t) = o.right_trigger {
            g.right_trigger = Some(t);
        }
        if let Some(t) = o.left_trigger {
            g.left_trigger = Some(t);
        }
        if o.release_leds {
            // The game hands the lights back to the profile.
            g.rgb = None;
            g.player_leds = None;
            g.mute_led = None;
        }
        if let Some(c) = o.rgb {
            g.rgb = Some(c);
        }
        if let Some(p) = o.player_leds {
            g.player_leds = Some(p);
        }
        if let Some(m) = o.mute_led {
            g.mute_led = Some(m);
        }
        drop(f);
        self.kick();
    }
}

fn normalize_mac(raw: &str) -> String {
    let hex: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return String::new();
    }
    let up = hex.to_ascii_uppercase();
    (0..6)
        .map(|i| &up[i * 2..i * 2 + 2])
        .collect::<Vec<_>>()
        .join(":")
}

fn read_identity(dev: &HidDevice, conn: Connection) -> (String, String, Mirror) {
    // The calibration read inside also switches a Bluetooth controller from
    // the short 0x01 report to the full 0x31 report.
    let mirror = Mirror::read(|id, len| match dev.get_feature(id, len) {
        Ok(b) => Some(b),
        Err(e) => {
            log::debug!("feature 0x{id:02X} read failed: {e}");
            None
        }
    });
    let mut firmware = String::new();
    if let Some(b) = &mirror.firmware {
        if b.len() >= 46 {
            let date: String = b[1..12]
                .iter()
                .map(|&c| c as char)
                .filter(|c| !c.is_control())
                .collect();
            let upd = u16::from_le_bytes([b[44], b[45]]);
            firmware = format!("{:X}.{:02X}", upd >> 8, upd & 0xFF);
            if !date.trim().is_empty() {
                firmware.push_str(&format!(" · {}", date.trim()));
            }
        }
    }
    let mut mac = normalize_mac(&dev.info.serial);
    if mac.is_empty() || conn == Connection::Usb {
        if let Some(b) = &mirror.pairing {
            let m: Vec<String> = (1..7).rev().map(|i| format!("{:02X}", b[i])).collect();
            mac = m.join(":");
        }
    }
    (mac, firmware, mirror)
}

/// Stops every worker of a device when one of them panics.
struct StopOnPanic<'a>(&'a Device);

impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.stop.store(true, Ordering::Release);
            self.0.dead.store(true, Ordering::Release);
            // Mapping state died with the thread; don't leave keys down.
            crate::inputsim::release_everything();
        }
    }
}

/// Open a controller and run it until it disappears or `stop` is set.
/// Blocks; call from a dedicated thread.
pub fn run(engine: Arc<Engine>, info: DeviceInfo, id: u64) {
    let Some(model) = Model::from_pid(info.pid) else {
        return;
    };
    let conn = ds_proto::connection_from_input_len(info.input_len);
    let dev = match HidDevice::open(&info) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("{} ({:?}): open failed: {e}", model.name(), conn);
            engine.note_open_failure(&info.path, &e);
            return;
        }
    };
    let (mac, firmware, mirror) = read_identity(&dev, conn);
    let (tx, rx) = bounded::<StreamPacket>(4);
    let (kick_tx, kick_rx) = bounded::<()>(1);
    let (haptics_wake_tx, haptics_wake) = bounded::<()>(1);
    let device = Arc::new(Device {
        id,
        info: info.clone(),
        model,
        conn,
        mac: Mutex::new(mac.clone()),
        firmware: Mutex::new(firmware.clone()),
        live: Mutex::new(Live::default()),
        stats: Stats::default(),
        link: Mutex::new(Link::Opening),
        overrides: Mutex::new(Overrides::default()),
        feedback: Mutex::new(Feedback::default()),
        toggles: Mutex::new(Toggles::default()),
        stream_tx: tx,
        stream_rx: rx,
        kick_tx,
        kick_rx,
        haptics_wake_tx,
        haptics_wake,
        pcm_mode: AtomicBool::new(false),
        haptic_level: [AtomicU8::new(0), AtomicU8::new(0)],
        calibrate_request: AtomicBool::new(false),
        mirror: Mutex::new(mirror),
        virt: Default::default(),
        game_audio: Mutex::new(None),
        force_output: AtomicBool::new(true),
        stop: AtomicBool::new(false),
        dead: AtomicBool::new(false),
    });
    log::info!(
        "connected {} over {:?}, MAC {}, firmware {}",
        model.name(),
        conn,
        if mac.is_empty() { "?" } else { &mac },
        if firmware.is_empty() { "?" } else { &firmware }
    );
    engine.add_device(device.clone());

    // A panic in one worker stops the others, and the device is still
    // removed, so hot-plug opens it again instead of leaving a dead entry.
    let workers = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::thread::scope(|s| {
            let d = &device;
            let e = &engine;
            let devr = &dev;
            std::thread::Builder::new()
                .name(format!("ds-read-{id}"))
                .spawn_scoped(s, move || {
                    let _g = StopOnPanic(d);
                    reader_loop(e, d, devr)
                })
                .expect("spawn reader");
            std::thread::Builder::new()
                .name(format!("ds-write-{id}"))
                .spawn_scoped(s, move || {
                    let _g = StopOnPanic(d);
                    writer_loop(e, d, devr)
                })
                .expect("spawn writer");
            std::thread::Builder::new()
                .name(format!("ds-virtual-{id}"))
                .spawn_scoped(s, move || {
                    let _g = StopOnPanic(d);
                    crate::virt::run(e, d)
                })
                .expect("spawn virtual");
            if conn == Connection::Bluetooth {
                std::thread::Builder::new()
                    .name(format!("ds-haptics-{id}"))
                    .spawn_scoped(s, move || {
                        let _g = StopOnPanic(d);
                        crate::haptics::streamer::run(e, d)
                    })
                    .expect("spawn haptics");
            }
        })
    }));
    if workers.is_err() {
        log::error!(
            "{}: a worker thread failed; reopening the controller",
            model.name()
        );
    }

    engine.remove_device(id);
    log::info!("{} disconnected", model.name());
}

fn reader_loop(engine: &Arc<Engine>, device: &Arc<Device>, dev: &HidDevice) {
    crate::platform::raise_thread_priority(false);
    let mut reader = match dev.reader() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("reader setup failed: {e}");
            *device.link.lock() = Link::Error(format!("Can't read input: {e}"));
            device.dead.store(true, Ordering::Release);
            return;
        }
    };
    let mut pipeline = crate::pipeline::Pipeline::new();
    let mut prev = InputState::default();
    let mut last_input = Instant::now();
    let mut last_rate = Instant::now();
    let mut count_since = 0u32;
    let mut last_short_retry = Instant::now();
    let mut last_activity = Instant::now();
    let mut errors = 0u32;
    let mut check_crc = engine.settings.read().check_input_crc;

    while !device.stop.load(Ordering::Acquire) {
        match reader.read(100) {
            Ok(Some(buf)) => {
                errors = 0;
                match ds_proto::input::parse(buf, device.conn, device.model, check_crc) {
                    Ok(state) => {
                        let now = Instant::now();
                        device.stats.reports.fetch_add(1, Ordering::Relaxed);
                        count_since += 1;
                        last_input = now;
                        if !state.full
                            && now.duration_since(last_short_retry) > Duration::from_secs(1)
                        {
                            // Still on the short Bluetooth report: ask again.
                            last_short_retry = now;
                            let _ = dev.get_feature(0x05, 41);
                        }
                        if is_active(&state, &prev) {
                            last_activity = now;
                        }
                        {
                            let mut link = device.link.lock();
                            if *link != Link::Live {
                                *link = Link::Live;
                            }
                        }
                        let raw = ds_proto::virtual_pad::usb_layout(buf);
                        pipeline.process(engine, device, &state, &prev, raw.as_ref(), now);
                        prev = state;
                    }
                    Err(ParseError::BadCrc) => {
                        device.stats.crc_errors.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {}
                }
            }
            Ok(None) => {}
            Err(e) => {
                if hid::is_fatal(&e) {
                    log::info!("input stream closed: {e}");
                    break;
                }
                errors += 1;
                if errors % 50 == 1 {
                    log::warn!("read error (retrying): {e}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let now = Instant::now();
        if now.duration_since(last_input) > Duration::from_millis(1500) {
            let mut link = device.link.lock();
            if *link == Link::Live {
                *link = Link::Quiet;
            }
        }
        if now.duration_since(last_rate) >= Duration::from_secs(1) {
            let secs = now.duration_since(last_rate).as_secs_f32();
            device.stats.input_hz.store(
                (count_since as f32 / secs).round() as u32,
                Ordering::Relaxed,
            );
            count_since = 0;
            last_rate = now;
            check_crc = engine.settings.read().check_input_crc;
            // Idle power-off for Bluetooth controllers.
            let idle_min = engine.settings.read().idle_off_minutes;
            if idle_min > 0
                && device.is_bluetooth()
                && now.duration_since(last_activity) > Duration::from_secs(idle_min as u64 * 60)
            {
                log::info!("idle for {idle_min} min, turning the controller off");
                last_activity = now;
                let mac = device.mac.lock().clone();
                crate::platform::bt_disconnect(&mac);
            }
        }
    }
    pipeline.release_all();
    device.stop.store(true, Ordering::Release);
    device.dead.store(true, Ordering::Release);
}

fn is_active(s: &InputState, p: &InputState) -> bool {
    let moved = |a: u8, b: u8| (a as i16 - b as i16).abs() > 6;
    let off_center = |v: u8| (v as i16 - 128).abs() > 40;
    s.buttons != p.buttons
        || moved(s.l2, p.l2)
        || moved(s.r2, p.r2)
        || off_center(s.lx)
        || off_center(s.ly)
        || off_center(s.rx)
        || off_center(s.ry)
        || s.touch[0].active
}

/// Control report cadence: animations, battery gauge, keepalive.
const TICK: Duration = Duration::from_millis(8);
const TICK_STREAMING: Duration = Duration::from_millis(16);
/// Lighting-only changes (breathing, rainbow, strobe, the battery pulse)
/// go out at most this often: 40 Hz looks smooth, and the controller's
/// radio receives a third as many reports as at the 8 ms tick, which saves
/// battery. Triggers, rumble, and everything else still go out at once.
const LIGHTING_GAP: Duration = Duration::from_millis(25);

/// `new` differs from `old` only in the lightbar and player LEDs.
fn lighting_only(old: &ds_proto::OutputState, new: &ds_proto::OutputState) -> bool {
    let mut o = *old;
    o.lightbar = new.lightbar;
    o.player_leds = new.player_leds;
    o == *new
}

/// Shortest gap between control reports when a change wakes the writer
/// between ticks. Bluetooth shares the radio with input and, while
/// streaming, the haptics packets; USB has room to spare.
fn min_gap(conn: Connection, pcm: bool) -> Duration {
    match (conn, pcm) {
        (Connection::Usb, _) => Duration::from_millis(1),
        (Connection::Bluetooth, false) => Duration::from_millis(4),
        (Connection::Bluetooth, true) => Duration::from_millis(8),
    }
}

fn writer_loop(engine: &Arc<Engine>, device: &Arc<Device>, dev: &HidDevice) {
    crate::platform::raise_thread_priority(true);
    let mut writer = match dev.writer() {
        Ok(w) => w,
        Err(e) => {
            log::warn!("writer setup failed: {e}");
            *device.link.lock() = Link::Error(format!("Can't write output: {e}"));
            device.stop.store(true, Ordering::Release);
            return;
        }
    };
    let mut composer = Composer::new();
    let mut buf = [0u8; MAX_REPORT];
    let mut last_sent: Option<ds_proto::OutputState> = None;
    let mut last_write = Instant::now() - Duration::from_secs(10);
    let mut next_tick = Instant::now();
    let mut fails = 0u32;
    let mut backoff_until = Instant::now();
    let mut writes_since = 0u32;
    let mut stream_since = 0u32;
    let mut last_rate = Instant::now();
    let mut profile_cache = crate::engine::ProfileCache::default();

    let do_write =
        |w: &mut hid::Writer, data: &[u8], fails: &mut u32, backoff_until: &mut Instant| -> bool {
            let t0 = Instant::now();
            match w.write(data, 250) {
                Ok(()) => {
                    let us = t0.elapsed().as_micros().min(u32::MAX as u128) as u32;
                    let old = device.stats.write_us.load(Ordering::Relaxed);
                    device.stats.write_us.store(
                        if old == 0 { us } else { (old * 7 + us) / 8 },
                        Ordering::Relaxed,
                    );
                    device.stats.writes.fetch_add(1, Ordering::Relaxed);
                    if *fails > 0 {
                        log::info!("output recovered after {} failed write(s)", fails);
                    }
                    *fails = 0;
                    true
                }
                Err(e) => {
                    device.stats.write_errors.fetch_add(1, Ordering::Relaxed);
                    *fails += 1;
                    let wait = (25u64 << (*fails - 1).min(6)).min(1000);
                    *backoff_until = Instant::now() + Duration::from_millis(wait);
                    if *fails <= 3 || (*fails).is_multiple_of(20) {
                        log::warn!("write failed ({}x), retrying in {wait} ms: {e}", fails);
                    }
                    if hid::is_fatal(&e) {
                        device.stop.store(true, Ordering::Release);
                    }
                    false
                }
            }
        };

    while !device.stop.load(Ordering::Acquire) {
        let now = Instant::now();
        let wait = next_tick
            .saturating_duration_since(now)
            .min(Duration::from_millis(20));
        let mut kicked = false;
        crossbeam_channel::select! {
            recv(device.stream_rx) -> pkt => match pkt {
                Ok(pkt) => {
                    if Instant::now() >= backoff_until
                        && do_write(
                            &mut writer,
                            &pkt.data[..pkt.len],
                            &mut fails,
                            &mut backoff_until,
                        )
                    {
                        device.stats.stream_packets.fetch_add(1, Ordering::Relaxed);
                        stream_since += 1;
                    }
                }
                Err(_) => break,
            },
            recv(device.kick_rx) -> _ => kicked = true,
            default(wait) => {}
        }

        let pcm = device.pcm_mode.load(Ordering::Acquire);
        if kicked {
            // Something the report carries changed (a game's trigger or
            // rumble, a mod, a test): send it now rather than at the next
            // tick, keeping a short gap so a chatty game can't flood the
            // radio.
            next_tick = next_tick.min(last_write + min_gap(device.conn, pcm));
        }
        let now = Instant::now();
        if now < next_tick {
            continue;
        }
        // Lighter control cadence while the media stream shares the radio.
        next_tick = now + if pcm { TICK_STREAMING } else { TICK };

        let profile = profile_cache.get(engine, device);
        let input = device.live.lock().input;
        let overrides = device.overrides.lock().clone();
        let toggles = *device.toggles.lock();
        let state = {
            let mut fb = device.feedback.lock();
            fb.prune(now);
            composer.compose(
                &profile,
                &input,
                &overrides,
                &fb,
                &toggles,
                pcm,
                now,
            )
        };
        let keepalive =
            now.duration_since(last_write) > Duration::from_millis(if pcm { 1000 } else { 500 });
        let changed = last_sent.as_ref() != Some(&state);
        let throttled = changed
            && last_sent
                .as_ref()
                .map(|l| lighting_only(l, &state))
                .unwrap_or(false)
            && now.duration_since(last_write) < LIGHTING_GAP;
        let due =
            keepalive || (changed && !throttled) || device.force_output.load(Ordering::Acquire);
        if due && now >= backoff_until {
            device.force_output.store(false, Ordering::Release);
            let report = state.build(device.conn, &mut buf);
            if do_write(&mut writer, report, &mut fails, &mut backoff_until) {
                last_sent = Some(state);
                last_write = now;
                writes_since += 1;
            } else {
                // Make sure the state goes out once the link is back.
                last_sent = None;
            }
        }

        if now.duration_since(last_rate) >= Duration::from_secs(1) {
            let secs = now.duration_since(last_rate).as_secs_f32();
            device.stats.output_hz.store(
                (writes_since as f32 / secs).round() as u32,
                Ordering::Relaxed,
            );
            device.stats.stream_hz.store(
                (stream_since as f32 / secs).round() as u32,
                Ordering::Relaxed,
            );
            writes_since = 0;
            stream_since = 0;
            last_rate = now;
        }
    }

    // Leave the controller in a calm state: motors off, triggers released.
    let calm = ds_proto::OutputState {
        lightbar: last_sent.map(|s| s.lightbar).unwrap_or([0, 0, 64]),
        ..Default::default()
    };
    let report = calm.build(device.conn, &mut buf).to_vec();
    let _ = writer.write(&report, 100);
}
