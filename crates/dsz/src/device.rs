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

/// `Device::speaker_route` values below the lowest speaker volume byte.
pub const SPEAKER_UNTOUCHED: u8 = 0;
pub const SPEAKER_MUTED: u8 = 1;
pub const SPEAKER_HEADSET: u8 = 2;

/// The output-report audio route for a `speaker_route` value and the
/// headphone volume byte.
pub fn audio_route(v: u8, headset_volume: u8) -> ds_proto::AudioRoute {
    use ds_proto::AudioRoute;
    match v {
        SPEAKER_UNTOUCHED => AudioRoute::Untouched,
        SPEAKER_MUTED => AudioRoute::Muted,
        SPEAKER_HEADSET => AudioRoute::Headset { volume: headset_volume },
        volume => AudioRoute::Speaker { volume },
    }
}

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

/// A rumble level the game set that no report to the controller has
/// carried yet; see [`GameState::set_rumble`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeldRumble {
    pub heavy: u8,
    pub light: u8,
    /// Which of the game's rumble reports this is, so a report composed
    /// before a newer level came in doesn't let that one go.
    pub seq: u64,
    pub at: Instant,
    /// When the oldest level merged into this one was set, for how long
    /// pulses wait for a report (the session log, issue 43).
    pub since: Instant,
}

/// A held rumble level no report took within this long is dropped rather
/// than played late (a stalled link, or rumble going nowhere, such as a
/// wired controller playing the game's haptic audio).
pub const PULSE_STALE: Duration = Duration::from_millis(100);

/// Shortest a game's rumble pulse plays on the controller (issue 43). GTA V
/// switches its rumble on and off again 0.2 ms later, about 11 times a
/// second, so the motors ran only until the next report: about 4 ms on
/// Bluetooth (felt as skipping) or 8 ms while streaming ("so close"). A
/// stop that comes sooner than this after the first report carrying the
/// pulse waits until then; one frame at 60 fps, which is likely how long
/// the game means a pulse to last.
pub const PULSE_MIN: Duration = Duration::from_millis(16);

/// The game's last rumble pulse, kept on until [`PULSE_MIN`] after a report
/// first carried it.
#[derive(Debug, Clone, Copy)]
pub struct PulseStretch {
    /// The level the game last set in it.
    pub heavy: u8,
    pub light: u8,
    pub began: Instant,
    /// Set when a report first carries the pulse.
    pub until: Option<Instant>,
    /// The game stopped it early, so it is being kept on.
    pub cut: bool,
}

/// What a game sent to the virtual controller.
#[derive(Debug, Default, Clone)]
pub struct GameState {
    /// (heavy, light). Holds until the game changes it, as on a real pad;
    /// cleared when the game closes the virtual controller.
    pub rumble: Option<(u8, u8)>,
    /// Issue 43: the highest level set since a report last carried the
    /// game's rumble. GTA V pulses its rumble, a level in one report and
    /// zero in the next, often closer together than the control reports
    /// go out; without this the zero overwrote the level before any report
    /// carried it, and those pulses never reached the controller.
    pub held: Option<HeldRumble>,
    /// The pulse a stop cut short of [`PULSE_MIN`], or the one playing.
    pub stretch: Option<PulseStretch>,
    pub rumble_seq: u64,
    pub right_trigger: Option<[u8; 11]>,
    pub left_trigger: Option<[u8; 11]>,
    pub rgb: Option<[u8; 3]>,
    pub player_leds: Option<u8>,
    pub mute_led: Option<u8>,
    /// Speaker volume, audio control and preamp the game last asked for,
    /// logged when they change. Not applied: the profile sets the speaker.
    pub audio: (Option<u8>, Option<u8>, Option<u8>),
    pub reports: u64,
    /// The first rumble of this session was logged.
    pub rumble_logged: bool,
    /// How the game's reports changed its rumble this session, logged when
    /// the virtual controller goes (issue 43): reports that set a level,
    /// that stopped it with zero levels, that stopped it with no flags at
    /// all (SDL's way), and that stopped it while changing something else.
    pub rumble_sets: u64,
    pub rumble_zeroed: u64,
    pub rumble_flags_off: u64,
    pub rumble_cut_by_other: u64,
    /// How long the game kept its rumble on each time, for the session
    /// line (issue 43): runs, the shortest, the total, and those under
    /// 8 ms (shorter than a control report gap while streaming).
    pub rumble_on_since: Option<Instant>,
    pub rumble_runs: u64,
    pub rumble_run_min: Option<Duration>,
    pub rumble_run_total: Duration,
    pub rumble_runs_short: u64,
    /// Pulses that reached the controller only because they were held.
    pub rumble_saved: u64,
    /// Whether every pulse reached the controller, and on time (issue 43):
    /// pulses merged into a level still waiting for a report (the
    /// controller gets one pulse for them), held levels dropped or left
    /// with no report carrying them, and how long carried ones waited.
    pub rumble_merged: u64,
    pub rumble_uncarried: u64,
    pub rumble_carried: u64,
    pub rumble_wait_total: Duration,
    pub rumble_wait_max: Duration,
    pub rumble_waits_long: u64,
    /// Pulses a stop came too soon for, kept on to [`PULSE_MIN`] (issue 43).
    pub rumble_stretched: u64,
    /// Time between one pulse's start and the next's, for how often the
    /// game pulses: the shortest, the total, and gaps under 50 ms.
    pub rumble_last_start: Option<Instant>,
    pub rumble_gaps: u64,
    pub rumble_gap_min: Option<Duration>,
    pub rumble_gap_total: Duration,
    pub rumble_gaps_short: u64,
    /// The highest level each motor had in a pulse, and how many pulses
    /// used each motor.
    pub rumble_peak: (u8, u8),
    pub rumble_on_heavy: u64,
    pub rumble_on_light: u64,
    /// Whether the motors are on in the last report sent to the
    /// controller, since when, and how long they stayed on each time:
    /// what the controller actually played, as opposed to what the game
    /// asked for (issue 43).
    pub out_on_since: Option<Instant>,
    pub out_runs: u64,
    pub out_run_min: Option<Duration>,
    pub out_run_max: Duration,
    pub out_run_total: Duration,
}

impl GameState {
    /// The game set its classic rumble. A non-zero level is also held,
    /// merged with any level still waiting, until a report carries it
    /// ([`GameState::rumble_sent`]); a zero or lower level never cuts a
    /// level no report has carried yet.
    pub fn set_rumble(&mut self, heavy: u8, light: u8, now: Instant) {
        let on = heavy != 0 || light != 0;
        // A new pulse, not the game repeating the level it holds.
        let starts = on && self.rumble_on_since.is_none();
        match (self.rumble_on_since, on) {
            (None, true) => self.rumble_on_since = Some(now),
            (Some(since), false) => {
                let run = now.saturating_duration_since(since);
                self.rumble_on_since = None;
                self.rumble_runs += 1;
                self.rumble_run_total += run;
                self.rumble_run_min = Some(self.rumble_run_min.map_or(run, |m| m.min(run)));
                if run < Duration::from_millis(8) {
                    self.rumble_runs_short += 1;
                }
            }
            _ => {}
        }
        self.rumble = on.then_some((heavy, light));
        if starts {
            if let Some(last) = self.rumble_last_start {
                let gap = now.saturating_duration_since(last);
                self.rumble_gaps += 1;
                self.rumble_gap_total += gap;
                self.rumble_gap_min = Some(self.rumble_gap_min.map_or(gap, |m| m.min(gap)));
                if gap < Duration::from_millis(50) {
                    self.rumble_gaps_short += 1;
                }
            }
            self.rumble_last_start = Some(now);
            self.rumble_on_heavy += (heavy > 0) as u64;
            self.rumble_on_light += (light > 0) as u64;
            // Its minimum length counts from the first report that carries
            // it (`rumble_sent`), so a pulse waiting for a report still gets
            // all of it on the controller.
            self.stretch = Some(PulseStretch { heavy, light, began: now, until: None, cut: false });
        } else if on {
            if let Some(s) = self.stretch.as_mut() {
                s.heavy = heavy;
                s.light = light;
            }
        } else if self.stretch_owed(now).is_some() {
            if let Some(s) = self.stretch.as_mut().filter(|s| !s.cut) {
                s.cut = true;
                self.rumble_stretched += 1;
            }
        }
        if on {
            self.rumble_peak = (self.rumble_peak.0.max(heavy), self.rumble_peak.1.max(light));
            self.rumble_seq += 1;
            let waiting = self.held_now(now);
            if waiting.is_some() {
                self.rumble_merged += starts as u64;
            } else if self.held.is_some() {
                self.rumble_uncarried += 1;
            }
            let (h, l, since) = waiting.map_or((0, 0, now), |p| (p.heavy, p.light, p.since));
            self.held = Some(HeldRumble {
                heavy: h.max(heavy),
                light: l.max(light),
                seq: self.rumble_seq,
                at: now,
                since,
            });
        }
    }

    fn held_now(&self, now: Instant) -> Option<HeldRumble> {
        self.held
            .filter(|p| now.saturating_duration_since(p.at) < PULSE_STALE)
    }

    /// The level of a pulse that still owes the controller some of
    /// [`PULSE_MIN`]: not carried yet (and not stale), or carried less than
    /// that long ago.
    fn stretch_owed(&self, now: Instant) -> Option<(u8, u8)> {
        self.stretch
            .filter(|s| match s.until {
                Some(u) => now < u,
                None => now.saturating_duration_since(s.began) < PULSE_STALE,
            })
            .map(|s| (s.heavy, s.light))
    }

    /// A report went out to the controller; `on`: its motors are running.
    /// Times how long the controller actually played each burst.
    pub fn motors_written(&mut self, on: bool, now: Instant) {
        match (self.out_on_since, on) {
            (None, true) => self.out_on_since = Some(now),
            (Some(since), false) => {
                let run = now.saturating_duration_since(since);
                self.out_on_since = None;
                self.out_runs += 1;
                self.out_run_total += run;
                self.out_run_max = self.out_run_max.max(run);
                self.out_run_min = Some(self.out_run_min.map_or(run, |m| m.min(run)));
            }
            _ => {}
        }
    }

    /// When a pulse the game already stopped ends on the controller, for
    /// the writer to send the stop right then rather than at its next tick.
    pub fn stretch_end(&self, now: Instant) -> Option<Instant> {
        if self.rumble.is_some() {
            return None;
        }
        self.stretch.and_then(|s| s.until).filter(|&u| now < u)
    }

    /// The game's rumble for the next report: what it last set (or a pulse
    /// it stopped before [`PULSE_MIN`] was up), raised to any level it set
    /// that no report has carried yet. With the hold, the held report's
    /// sequence number and whether it raised the level.
    pub fn rumble_to_send(&self, now: Instant) -> ((u8, u8), Option<(u64, bool)>) {
        // Before a report carries the pulse, the held level below sends it.
        let carried_stretch = self
            .stretch
            .filter(|s| s.until.is_some_and(|u| now < u))
            .map(|s| (s.heavy, s.light));
        let (h, l) = match self.rumble {
            Some(level) => level,
            None => carried_stretch.unwrap_or((0, 0)),
        };
        match self.held_now(now) {
            Some(p) => (
                (h.max(p.heavy), l.max(p.light)),
                Some((p.seq, p.heavy > h || p.light > l)),
            ),
            None => ((h, l), None),
        }
    }

    /// A report to the controller (or the haptic stream) carried the held
    /// level `seq` from [`GameState::rumble_to_send`]: let it go, unless a
    /// newer level came in since. `raised`: the hold made the difference.
    pub fn rumble_sent(&mut self, seq: u64, raised: bool, now: Instant) {
        if let Some(p) = self.held.filter(|p| p.seq == seq) {
            self.held = None;
            // The pulse is on the controller from now: it stays on at
            // least PULSE_MIN, however soon the game stops it.
            if let Some(s) = self.stretch.as_mut().filter(|s| s.until.is_none()) {
                s.until = Some(now + PULSE_MIN);
            }
            if raised {
                self.rumble_saved += 1;
            }
            let wait = now.saturating_duration_since(p.since);
            self.rumble_carried += 1;
            self.rumble_wait_total += wait;
            self.rumble_wait_max = self.rumble_wait_max.max(wait);
            if wait > Duration::from_millis(8) {
                self.rumble_waits_long += 1;
            }
        }
    }
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
        let g = &self.game;
        if g.rumble_logged {
            log::info!(
                "game rumble this session: {} report(s) set it, stopped by {} with zero levels, {} with no flags, {} that changed only other things",
                g.rumble_sets,
                g.rumble_zeroed,
                g.rumble_flags_off,
                g.rumble_cut_by_other
            );
            if g.rumble_runs > 0 {
                log::info!(
                    "game rumble pulses: {} on for {:.1} ms on average, shortest {:.1} ms, {} under 8 ms; {} reached the controller only because DSZ held them",
                    g.rumble_runs,
                    g.rumble_run_total.as_secs_f64() * 1000.0 / g.rumble_runs as f64,
                    g.rumble_run_min.unwrap_or_default().as_secs_f64() * 1000.0,
                    g.rumble_runs_short,
                    g.rumble_saved
                );
            }
            if g.rumble_carried > 0 || g.rumble_merged > 0 || g.rumble_uncarried > 0 || g.held.is_some() {
                log::info!(
                    "game rumble delivery: {} held level(s) carried, after {:.1} ms on average, longest {:.1} ms, {} over 8 ms; {} pulse(s) merged into one still waiting; {} never carried",
                    g.rumble_carried,
                    g.rumble_wait_total.as_secs_f64() * 1000.0 / g.rumble_carried.max(1) as f64,
                    g.rumble_wait_max.as_secs_f64() * 1000.0,
                    g.rumble_waits_long,
                    g.rumble_merged,
                    g.rumble_uncarried + g.held.is_some() as u64
                );
            }
            if g.rumble_runs > 0 {
                log::info!(
                    "game rumble stretch: {} pulse(s) the game stopped early kept on the controller for {} ms",
                    g.rumble_stretched,
                    PULSE_MIN.as_millis()
                );
            }
            if g.rumble_gaps > 0 {
                log::info!(
                    "game rumble pattern: a pulse every {:.1} ms on average, closest {:.1} ms apart, {} under 50 ms apart; heavy motor in {} pulse(s), light in {}; highest levels heavy {}, light {}",
                    g.rumble_gap_total.as_secs_f64() * 1000.0 / g.rumble_gaps as f64,
                    g.rumble_gap_min.unwrap_or_default().as_secs_f64() * 1000.0,
                    g.rumble_gaps_short,
                    g.rumble_on_heavy,
                    g.rumble_on_light,
                    g.rumble_peak.0,
                    g.rumble_peak.1
                );
            }
            if g.out_runs > 0 {
                log::info!(
                    "controller rumble: the motors ran {} time(s) in the reports sent, {:.1} ms on average, shortest {:.1} ms, longest {:.1} ms",
                    g.out_runs,
                    g.out_run_total.as_secs_f64() * 1000.0 / g.out_runs as f64,
                    g.out_run_min.unwrap_or_default().as_secs_f64() * 1000.0,
                    g.out_run_max.as_secs_f64() * 1000.0
                );
            }
        }
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Toggles {
    pub gyro: Option<bool>,
    pub touchpad: Option<bool>,
    pub mute_engaged: bool,
    pub gyro_toggled_on: bool,
}

/// Lock-free atomic storage for [`Toggles`], read on every HID input report.
#[derive(Default)]
pub struct AtomicToggles(AtomicU32);

impl AtomicToggles {
    fn pack(t: Toggles) -> u32 {
        let opt = |o: Option<bool>| match o {
            None => 0u32,
            Some(false) => 1,
            Some(true) => 2,
        };
        opt(t.gyro)
            | (opt(t.touchpad) << 2)
            | (u32::from(t.mute_engaged) << 4)
            | (u32::from(t.gyro_toggled_on) << 5)
    }

    fn unpack(v: u32) -> Toggles {
        let opt = |bits: u32| match bits & 3 {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        };
        Toggles {
            gyro: opt(v),
            touchpad: opt(v >> 2),
            mute_engaged: (v & (1 << 4)) != 0,
            gyro_toggled_on: (v & (1 << 5)) != 0,
        }
    }

    #[inline]
    pub fn load(&self) -> Toggles {
        Self::unpack(self.0.load(Ordering::Acquire))
    }

    pub fn update(&self, mut f: impl FnMut(&mut Toggles)) {
        let mut cur = self.0.load(Ordering::Relaxed);
        loop {
            let mut t = Self::unpack(cur);
            f(&mut t);
            let next = Self::pack(t);
            match self
                .0
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(v) => cur = v,
            }
        }
    }
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
    /// The active gyro drives the virtual right stick, not the mouse.
    pub gyro_stick: bool,
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
    /// Gyro and accelerometer are present. A clone without them leaves
    /// `InputState::motion_valid` clear so the gyro mouse stays put.
    pub has_motion: bool,
    /// Lightbar and rumble can be written. A pad without them never opens
    /// the write handle, so a refused write cannot drop the device.
    pub has_output: bool,
    pub conn: Connection,
    pub mac: Mutex<String>,
    pub firmware: Mutex<String>,
    pub live: Mutex<Live>,
    pub stats: Stats,
    pub link: Mutex<Link>,
    pub overrides: Mutex<Overrides>,
    pub feedback: Mutex<Feedback>,
    pub toggles: AtomicToggles,
    /// A profile in use only while a mapping's buttons are held.
    pub held_profile: Mutex<Option<crate::engine::HeldProfile>>,
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
    /// "Classic rumble always" has the actuators: the firmware's rumble
    /// emulation plays and the haptic stream leaves its haptic packet out.
    /// Set by the writer from [`crate::composer::Composer::classic_owns`].
    pub classic_rumble: AtomicBool,
    /// A game's haptic audio is playing on a wired controller's own sound
    /// device: the control reports leave the motor fields unmarked, or the
    /// firmware's rumble emulation takes the actuators and the audio isn't
    /// felt.
    pub usb_haptics: AtomicBool,
    /// Haptic output level 0..=255 per side, for meters.
    pub haptic_level: [AtomicU8; 2],
    /// Speaker sound level 0..=255 (what the game or PC sends to the
    /// speaker, whether or not it is playing), for the meter.
    pub speaker_level: AtomicU8,
    /// Microphone packets kept while a speaker test runs.
    pub mic_tap: Mutex<Option<Vec<crate::haptics::mic::Packet>>>,
    pub microphone: Arc<crate::haptics::capture::Capture>,
    microphone_source: AtomicU8,
    microphone_managed: AtomicBool,
    /// Silent reports queued when a speaker stream starts.
    pub speaker_lead: AtomicU8,
    /// What the last speaker test heard, for the Haptics page.
    pub speaker_test_result: Mutex<Option<String>>,
    /// Where the controller plays audio, set by the haptics thread:
    /// `SPEAKER_UNTOUCHED`, `SPEAKER_MUTED`, `SPEAKER_HEADSET`, or the
    /// speaker volume byte.
    pub speaker_route: AtomicU8,
    /// Headphone volume byte for `SPEAKER_HEADSET`.
    pub headset_volume: AtomicU8,
    /// Audio fields of a DualShock 4's Bluetooth `0x11` report, set by its
    /// audio thread. `Untouched` (the report as before) unless the profile
    /// turns DualShock 4 audio on.
    pub ds4_audio: Mutex<ds_proto::ds4::AudioOut>,
    pub calibrate_request: AtomicBool,
    /// Feature reports a virtual DualSense driven by this controller answers with.
    pub mirror: Mutex<Mirror>,
    /// A game asked a virtual DualSense for a report the controller didn't
    /// give at connect: the writer thread reads the missing ones again.
    mirror_request: AtomicBool,
    /// When the last read again was asked for, and how many have been.
    mirror_retry: Mutex<(Option<Instant>, u32)>,
    /// The virtual controller standing in for this one, if any.
    pub virt: crate::virt::Slot,
    /// Game audio for the haptics thread: a virtual DualSense's PCM ring,
    /// or the test signal.
    pub game_audio: Mutex<Option<Arc<dyn crate::haptics::game::PcmSource>>>,
    /// A running test signal in `game_audio`, and what it set aside: the
    /// virtual DualSense's own stream, put back when the test ends.
    game_audio_test: Mutex<Option<GameAudioTest>>,
    pub force_output: AtomicBool,
    pub stop: AtomicBool,
    pub dead: AtomicBool,
    /// Quick Charge already dropped this Bluetooth link, so the USB and
    /// Bluetooth halves don't both ask the radio.
    quick_charge_off: AtomicBool,
    /// Recent radio traffic, logged if the controller drops by itself.
    pub flight: crate::flight::Recorder,
    /// Last button, stick, trigger or touch use (or microphone recording):
    /// what the idle power-off counts from, and the idle lightbar fade.
    pub last_activity: Mutex<Instant>,
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

    /// Have the writer thread read the feature reports missing from
    /// [`Device::mirror`] again, at most every [`MIRROR_RETRY_GAP`] and
    /// [`MIRROR_RETRIES`] times. A game asking for one every half second
    /// asks here as often; the virtual pad answers with a stand-in meanwhile.
    pub fn request_mirror(&self) {
        if self.model.is_dualshock4() {
            return;
        }
        let mut r = self.mirror_retry.lock();
        if r.1 >= MIRROR_RETRIES || r.0.is_some_and(|t| t.elapsed() < MIRROR_RETRY_GAP) {
            return;
        }
        *r = (Some(Instant::now()), r.1 + 1);
        drop(r);
        self.mirror_request.store(true, Ordering::Release);
        self.kick();
    }

    /// Apply a profile's microphone settings. Capture runs only while an app
    /// records from the virtual DualSense, or from the microphone device
    /// beside another pad (Discord in a call, a recorder), so the rest of
    /// the time the controller's microphone, its radio traffic and the
    /// idle-off timer are as if it were off.
    pub fn apply_microphone(&self, mic: &crate::profile::Microphone) {
        let enabled = mic.enabled && self.virt.microphone_ready();
        let muted = mic.muted || (mic.mute_button && self.toggles.load().mute_engaged);
        let listening = self.virt.microphone_open();
        self.set_microphone(enabled, muted || !listening, mic.gain_percent, mic.source);
    }

    /// Microphone capture is independent of speaker/haptics settings. The
    /// controller's hardware selector chooses built-in, headset, or auto.
    pub fn set_microphone(
        &self,
        enabled: bool,
        muted: bool,
        gain_percent: u16,
        source: crate::profile::MicrophoneSource,
    ) {
        let enabled = enabled && self.is_bluetooth() && !self.model.is_dualshock4();
        // Off, the other settings don't matter: keep them fixed, so a Mute
        // press or a source change on a profile without the microphone
        // changes nothing here.
        let (muted, gain_percent, source) = if enabled {
            (muted, gain_percent, source)
        } else {
            (false, 100, crate::profile::MicrophoneSource::Auto)
        };
        let source = match source {
            crate::profile::MicrophoneSource::Auto => 0,
            crate::profile::MicrophoneSource::BuiltIn => 1,
            crate::profile::MicrophoneSource::Headset => 2,
        };
        let changed_source = self.microphone_source.swap(source, Ordering::AcqRel) != source;
        let changed = self.microphone.configure(enabled, muted, gain_percent);
        if changed_source {
            self.microphone.clear();
        }
        self.microphone_managed.store(enabled, Ordering::Release);
        if enabled {
            // Enabling only capture must never power a speaker left in an
            // unknown state by another application.
            if self
                .speaker_route
                .compare_exchange(
                    SPEAKER_UNTOUCHED,
                    SPEAKER_MUTED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.kick();
            }
        }
        if changed || changed_source {
            self.kick();
            let _ = self.haptics_wake_tx.try_send(());
        }
    }

    fn microphone_output(&self) -> Option<ds_proto::output::Microphone> {
        use ds_proto::output::{Microphone, MicrophoneSource};
        if !self.microphone_managed.load(Ordering::Acquire) {
            return None;
        }
        if self.mic_tap.lock().is_some() {
            // The speaker self-test must hear the nearby internal microphone
            // even after an earlier session left hardware mute enabled.
            return Some(Microphone {
                source: MicrophoneSource::BuiltIn,
                muted: false,
            });
        }
        Some(Microphone {
            source: if !self.microphone.enabled() {
                MicrophoneSource::Auto
            } else {
                match self.microphone_source.load(Ordering::Acquire) {
                    1 => MicrophoneSource::BuiltIn,
                    2 => MicrophoneSource::Headset,
                    _ => MicrophoneSource::Auto,
                }
            },
            muted: !self.microphone.recording(),
        })
    }

    /// Play a test signal through the game-audio path. A virtual
    /// DualSense's own stream is set aside while no game has its sound
    /// device open, and put back by `end_game_audio_test`. False while a
    /// game or another test is using the path.
    pub fn begin_game_audio_test(&self, src: Arc<dyn crate::haptics::game::PcmSource>) -> bool {
        let open = self.virt.status.lock().game_audio_open;
        let mut g = self.game_audio.lock();
        let mut test = self.game_audio_test.lock();
        if open || test.is_some() {
            return false;
        }
        let set_aside = g.replace(src.clone());
        *test = Some(GameAudioTest { src, set_aside });
        drop(test);
        drop(g);
        let _ = self.haptics_wake_tx.try_send(());
        true
    }

    /// End the test `begin_game_audio_test` started with `src`. What it
    /// set aside comes back, unless the virtual pad changed meanwhile and
    /// the path already holds the new pad's stream.
    pub fn end_game_audio_test(&self, src: &Arc<dyn crate::haptics::game::PcmSource>) {
        let mut g = self.game_audio.lock();
        let mut test = self.game_audio_test.lock();
        if !test.as_ref().is_some_and(|t| same_source(&t.src, src)) {
            return;
        }
        let t = test.take().unwrap();
        if g.as_ref().is_some_and(|x| same_source(x, src)) {
            *g = t.set_aside;
        }
        drop(test);
        drop(g);
        let _ = self.haptics_wake_tx.try_send(());
    }

    /// A game or a test is using the game-audio path.
    pub fn game_audio_busy(&self) -> bool {
        let open = self.virt.status.lock().game_audio_open;
        open || self.game_audio_testing()
    }

    /// A test signal is playing through the game-audio path.
    pub fn game_audio_testing(&self) -> bool {
        self.game_audio_test.lock().is_some()
    }

    /// Play the speaker test through the game-audio path and listen to it
    /// with the controller's own microphone; the result is logged and kept
    /// in `speaker_test_result`, and the microphone audio is saved as
    /// `dsz-speaker-test.wav` in the temp folder. False if a game or another
    /// test is using the path.
    pub fn start_speaker_test(self: &Arc<Self>) -> bool {
        use crate::haptics::game::{TestSignal, TestSource};
        let len = Duration::from_millis(4000);
        let src = Arc::new(TestSource::new(len, TestSignal::Speaker));
        if !self.begin_game_audio_test(src.clone()) {
            return false;
        }
        *self.mic_tap.lock() = Some(Vec::with_capacity(512));
        self.microphone.clear();
        self.kick();
        let _ = self.haptics_wake_tx.try_send(());
        *self.speaker_test_result.lock() = Some("Playing…".into());
        let d = self.clone();
        std::thread::spawn(move || {
            // The sound plus the cushion and the radio's delay.
            std::thread::sleep(len + Duration::from_millis(900));
            d.end_game_audio_test(&(src as Arc<dyn crate::haptics::game::PcmSource>));
            let packets = d.mic_tap.lock().take().unwrap_or_default();
            d.microphone.clear();
            d.kick();
            let _ = d.haptics_wake_tx.try_send(());
            let report = crate::haptics::mic::analyze(&packets);
            log::info!("speaker test: {}", report.summary);
            let wav = std::env::temp_dir().join("dsz-speaker-test.wav");
            let _ = std::fs::write(&wav, crate::haptics::mic::wav(&report.audio));
            *d.speaker_test_result.lock() = Some(report.summary);
        });
        true
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
        if self.is_bluetooth() && !self.model.is_dualshock4() {
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

    /// A Sony DualShock 4 on Bluetooth, which can play sound. Over USB
    /// Windows already has it as a sound device.
    pub fn ds4_audio_capable(&self) -> bool {
        self.model == Model::DualShock4
            && self.is_bluetooth()
            && self.has_output
            && ds_proto::ds4::supports_bt_audio(self.info.vid, self.info.pid)
    }

    /// Rumble from the game.
    pub fn set_game_rumble(&self, heavy: u8, light: u8) {
        let mut f = self.feedback.lock();
        f.game.set_rumble(heavy, light, Instant::now());
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
        let was = g.rumble.is_some();
        g.set_rumble(heavy, light, Instant::now());
        let [f0, f1, f2] = o.flags;
        if g.rumble.is_some() {
            g.rumble_sets += 1;
        } else if was {
            let other = f0 & !0x03 != 0 || f1 != 0 || f2 & !0x04 != 0;
            if f0 & 0x03 != 0 {
                g.rumble_zeroed += 1;
            } else if !other {
                g.rumble_flags_off += 1;
            } else {
                if g.rumble_cut_by_other == 0 {
                    log::info!(
                        "the game stopped its rumble with a report that changes other things (flags {f0:02X} {f1:02X} {f2:02X})"
                    );
                }
                g.rumble_cut_by_other += 1;
            }
        }
        if g.rumble.is_some() && !g.rumble_logged {
            // Which way a game sends its feedback (rumble, haptic audio, or
            // both) decides which level applies; issue 29. The flags and
            // motor power show which rumble emulation it asks for (issue 43).
            g.rumble_logged = true;
            log::info!(
                "the game sends classic rumble to the virtual DualSense (heavy {heavy}, light {light}; flags {f0:02X} {f1:02X} {f2:02X}, motor power {})",
                o.motor_power.map_or("-".to_string(), |b| format!("0x{b:02X}"))
            );
        }
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
        let audio = (o.speaker_volume, o.audio_control, o.speaker_preamp);
        if audio != (None, None, None) && audio != g.audio {
            g.audio = audio;
            let hex = |v: Option<u8>| v.map_or("-".to_string(), |b| format!("0x{b:02X}"));
            log::info!(
                "game asks for speaker volume {}, audio control {}, preamp {}",
                hex(audio.0),
                hex(audio.1),
                hex(audio.2)
            );
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

/// Waits before each read again of a feature report Windows calls busy.
/// Right after a Bluetooth controller connects, all three reads can fail
/// with "The requested resource is in use" (os error 170).
const FEATURE_BUSY_RETRIES: [Duration; 3] = [
    Duration::from_millis(50),
    Duration::from_millis(150),
    Duration::from_millis(300),
];
/// Shortest gap between reads again asked for by a game, and how many.
const MIRROR_RETRY_GAP: Duration = Duration::from_secs(2);
const MIRROR_RETRIES: u32 = 10;
/// `ERROR_BUSY`.
const OS_ERROR_BUSY: i32 = 170;

/// One feature report, read again while Windows calls it busy.
fn read_feature(dev: &HidDevice, id: u8, len: usize) -> Option<Vec<u8>> {
    let mut waits = FEATURE_BUSY_RETRIES.iter();
    let mut tries = 1;
    loop {
        match dev.get_feature(id, len) {
            Ok(b) => {
                if tries > 1 {
                    log::debug!("feature 0x{id:02X} read on try {tries}");
                }
                return Some(b);
            }
            Err(e) => match waits.next() {
                Some(wait) if e.raw_os_error() == Some(OS_ERROR_BUSY) => {
                    std::thread::sleep(*wait);
                    tries += 1;
                }
                _ => {
                    log::debug!("feature 0x{id:02X} read failed after {tries} tries: {e}");
                    return None;
                }
            },
        }
    }
}

/// "2.17 · Sep 19 2025" from a DualSense firmware report, or empty.
fn firmware_text(mirror: &Mirror) -> String {
    let Some(b) = mirror.firmware.as_ref().filter(|b| b.len() >= 46) else {
        return String::new();
    };
    let date: String = b[1..12]
        .iter()
        .map(|&c| c as char)
        .filter(|c| !c.is_control())
        .collect();
    let upd = u16::from_le_bytes([b[44], b[45]]);
    let mut firmware = format!("{:X}.{:02X}", upd >> 8, upd & 0xFF);
    if !date.trim().is_empty() {
        firmware.push_str(&format!(" · {}", date.trim()));
    }
    firmware
}

/// Reads the feature reports a game asked the virtual DualSense for and the
/// controller didn't give before. Runs on the writer thread, between output
/// reports, so each is tried once with no waits: rumble and haptics keep
/// going, and [`Device::request_mirror`]'s spacing is the retry.
fn read_mirror_again(device: &Device, dev: &HidDevice) {
    let mut mirror = device.mirror.lock().clone();
    if !mirror.fill_missing(|id, len| dev.get_feature(id, len).ok()) {
        log::debug!("{}: missing feature reports still unreadable", device.model.name());
        return;
    }
    let firmware = firmware_text(&mirror);
    if !firmware.is_empty() {
        *device.firmware.lock() = firmware.clone();
    }
    let missing: Vec<String> = [0x05u8, 0x09, 0x20]
        .iter()
        .filter(|&&id| mirror.lacks(id))
        .map(|id| format!("0x{id:02X}"))
        .collect();
    *device.mirror.lock() = mirror;
    log::info!(
        "{}: read its feature reports again for a game, firmware {}{}",
        device.model.name(),
        if firmware.is_empty() { "?" } else { &firmware },
        if missing.is_empty() { String::new() } else { format!(", still missing {}", missing.join(", ")) }
    );
}

fn read_identity(dev: &HidDevice, conn: Connection, model: Model) -> (String, String, Mirror) {
    if model.is_dualshock4() {
        return read_ds4_identity(dev, conn);
    }
    // The calibration read inside also switches a Bluetooth controller from
    // the short 0x01 report to the full 0x31 report.
    let mut mirror = Mirror::read(|id, len| read_feature(dev, id, len));
    mirror.firmware_from_edge = model == Model::DualSenseEdge;
    let firmware = firmware_text(&mirror);
    let mut mac = normalize_mac(&dev.info.serial);
    if mac.is_empty() || conn == Connection::Usb {
        if let Some(b) = &mirror.pairing {
            let m: Vec<String> = (1..7).rev().map(|i| format!("{:02X}", b[i])).collect();
            mac = m.join(":");
        }
    }
    (mac, firmware, mirror)
}

/// MAC from feature `0x12`, firmware from `0xA3`, and one calibration read
/// so a Bluetooth pad switches from the 10-byte report to `0x11`. The
/// DualSense mirror holds only a pairing address, the one the controller's
/// virtual DualShock 4 has, so Steam tells two DualShock 4s apart on
/// virtual DualSenses too. A DualShock 4 calibration block is not a
/// DualSense one, and the virtual pad answers with its neutral calibration.
fn read_ds4_identity(dev: &HidDevice, conn: Connection) -> (String, String, Mirror) {
    let mut mac = normalize_mac(&dev.info.serial);
    if mac.is_empty() || conn == Connection::Usb {
        if let Some(b) = read_feature(dev, 0x12, 16) {
            if b.len() >= 7 && b[0] == 0x12 && b[1..7].iter().any(|x| *x != 0) {
                mac = (1..7)
                    .rev()
                    .map(|i| format!("{:02X}", b[i]))
                    .collect::<Vec<_>>()
                    .join(":");
            }
        }
    }
    let mut firmware = String::new();
    if let Some(b) = read_feature(dev, 0xA3, 49) {
        if b.len() >= 43 && b[0] == 0xA3 {
            let hw = u16::from_le_bytes([b[35], b[36]]);
            let fw = u16::from_le_bytes([b[41], b[42]]);
            firmware = format!("hw {hw:04X} · fw {fw:04X}");
        }
    }
    let (id, len, bluetooth) = if conn == Connection::Bluetooth {
        (0x05, 41, true)
    } else {
        (0x02, 37, false)
    };
    match read_feature(dev, id, len) {
        Some(b) if !ds_proto::ds4::calibration_plausible(&b, bluetooth) => {
            log::info!("DualShock 4 calibration is missing or unusable; gyro uses a software bias");
        }
        None => log::debug!("DualShock 4 calibration read failed"),
        _ => {}
    }
    let mirror = Mirror {
        pairing: crate::virt::usbip::ds4_tag(&mac)
            .map(|t| crate::virt::mirror::pairing_report(&crate::virt::usbip::ds4_mac(t))),
        ..Mirror::default()
    };
    (mac, firmware, mirror)
}

/// How long a controller plugged in over USB waits for its Bluetooth link
/// to go away, so it takes over that link's virtual controller.
const QUICK_CHARGE_WAIT: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_secs(3)
};

/// How long a controller plugged in over USB waits for its Bluetooth side
/// to report cable power before Quick Charge leaves it alone. The pad
/// reports it within a few input reports of the cable going in.
const QUICK_CHARGE_CONFIRM: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_millis(1500)
};

/// Bluetooth HID service class: in the interface path of every Bluetooth
/// HID device, never in a USB one.
const BLUETOOTH_HID_SERVICE: &str = "{00001124-0000-1000-8000-00805f9b34fb}";

/// Whether `usb` is a controller really on a USB cable: not the app's own
/// virtual DualSense (which answers with the physical controller's
/// address, so games keep their settings for it) and not a Bluetooth
/// collection that was taken for a USB one.
fn real_usb(usb: &Device) -> bool {
    usb.conn == Connection::Usb
        && !usb.info.serial.starts_with(crate::virt::usbip::SERIAL_PREFIX)
        && !usb.info.path.to_ascii_lowercase().contains(BLUETOOTH_HID_SERVICE)
}

/// The Bluetooth side says it runs on cable power (charging or full). A
/// controller on a USB cable does; one that only looks alike over another
/// path doesn't. DS4Windows' Quick Charge goes by this alone.
fn on_cable_power(bt: &Device) -> bool {
    matches!(
        bt.live.lock().input.battery_status,
        ds_proto::input::BatteryStatus::Charging | ds_proto::input::BatteryStatus::Full
    )
}

/// The Bluetooth half when `new` and another connection are the same
/// controller (same MAC) on USB and Bluetooth, and the USB half is a real
/// USB connection.
fn quick_charge_twin(devices: &[Arc<Device>], new: &Arc<Device>) -> Option<Arc<Device>> {
    let mac = new.mac.lock().clone();
    crate::platform::bluetooth_address(&mac).filter(|a| *a != 0)?;
    let twin = devices.iter().find(|d| {
        !Arc::ptr_eq(d, new)
            && d.is_bluetooth() != new.is_bluetooth()
            && !d.stop.load(Ordering::Acquire)
            && *d.mac.lock() == mac
    })?;
    let (bt, usb) = if new.is_bluetooth() { (new, twin) } else { (twin, new) };
    real_usb(usb).then(|| bt.clone())
}

/// Quick Charge (Settings, on by default): a controller on Bluetooth that
/// is also plugged in over USB loses its Bluetooth link, so games and the
/// app see one controller, on USB. Checked once per connection from either
/// side, since at startup both can open in either order, and again from
/// the Bluetooth side when it starts reporting cable power. The link is
/// dropped only when both agree: the same address on a real USB
/// connection, and the Bluetooth side on cable power.
fn quick_charge(engine: &Engine, device: &Arc<Device>) {
    if !engine.settings.read().quick_charge {
        return;
    }
    let Some(bt) = quick_charge_twin(&engine.device_list(), device) else {
        return;
    };
    if bt.quick_charge_off.load(Ordering::Acquire) {
        return;
    }
    if !on_cable_power(&bt) {
        // Just plugged in: the Bluetooth side's next reports show the
        // cable. The Bluetooth side checks again by itself later.
        let t0 = Instant::now();
        while !on_cable_power(&bt)
            && t0.elapsed() < QUICK_CHARGE_CONFIRM
            && !Arc::ptr_eq(&bt, device)
            && !device.stop.load(Ordering::Acquire)
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        if !on_cable_power(&bt) {
            if !Arc::ptr_eq(&bt, device) {
                log::info!(
                    "Quick Charge: {} also shows up over USB, but its Bluetooth side reports no cable power; Bluetooth stays",
                    bt.mac.lock()
                );
            }
            return;
        }
    }
    if bt.quick_charge_off.swap(true, Ordering::AcqRel) {
        return;
    }
    let mac = bt.mac.lock().clone();
    log::info!("Quick Charge: {mac} is plugged in over USB; disconnecting its Bluetooth link");
    bt.flight.expect_off.store(true, Ordering::Release);
    if !quick_charge_disconnect(mac) {
        bt.flight.expect_off.store(false, Ordering::Release);
        engine.warn_toast(crate::i18n::tr("Couldn't disconnect Bluetooth"));
        return;
    }
    engine.toast(crate::i18n::tr("Plugged in over USB: Bluetooth disconnected"));
    if Arc::ptr_eq(&bt, device) {
        return;
    }
    // The Bluetooth side lets go of its virtual controller as it closes.
    // This side's virtual thread starts after this returns, so it takes
    // that pad over instead of plugging in a second one.
    let t0 = Instant::now();
    while t0.elapsed() < QUICK_CHARGE_WAIT
        && !device.stop.load(Ordering::Acquire)
        && engine.device_list().iter().any(|d| Arc::ptr_eq(d, &bt))
    {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Quick Charge for a Bluetooth side that opens while its USB twin is
/// already open (at startup both are found together, in either order).
/// Its battery status comes only with its first full input reports, so
/// wait for those, up to [`QUICK_CHARGE_CONFIRM`], and check again before
/// its virtual controller starts. True when its link is being dropped, so
/// no virtual controller is plugged in for it only to go away again.
fn quick_charge_on_open(engine: &Engine, device: &Arc<Device>) -> bool {
    if device.is_bluetooth()
        && engine.settings.read().quick_charge
        && quick_charge_twin(&engine.device_list(), device).is_some()
    {
        let t0 = Instant::now();
        while !device.live.lock().input.full
            && t0.elapsed() < QUICK_CHARGE_CONFIRM
            && !device.stop.load(Ordering::Acquire)
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        quick_charge(engine, device);
    }
    device.flight.expect_off.load(Ordering::Acquire)
}

/// A Bluetooth link lost sooner than this after connecting gets a log line
/// naming other controller apps that are running.
const EARLY_DROP: Duration = Duration::from_secs(10);

/// Programs that can drive or disconnect a DualSense themselves, as
/// lower-case exe names.
const CONTROLLER_APPS: &[&str] = &[
    "ds4windows.exe",
    "dualsensex.exe",
    "dualsensey.exe",
    "steam.exe",
    "rewasdengine.exe",
    "rewasd.exe",
    "x360ce.exe",
    "padforge.exe",
    "joytokey.exe",
];

/// The [`CONTROLLER_APPS`] in `running`, in list order.
fn controller_apps(running: &std::collections::HashSet<String>) -> Vec<&'static str> {
    CONTROLLER_APPS.iter().copied().filter(|a| running.contains(*a)).collect()
}

/// Input with no controller state for this long shows as "No signal".
const QUIET_AFTER: Duration = Duration::from_millis(1500);
/// Log at most this many "No signal" stops per connection.
const QUIET_LOGS: u32 = 20;

/// What the reader got since the last controller state. Issue 63: Game Bar
/// opening put a controller on "No signal" and the log said nothing.
#[derive(Default)]
struct SinceState {
    timeouts: u32,
    errors: u32,
    bad_crc: u32,
    other: u32,
}

impl SinceState {
    fn describe(&self) -> String {
        let parts: Vec<String> = [
            (self.timeouts, "reads timed out"),
            (self.errors, "reads failed"),
            (self.bad_crc, "reports failed the CRC"),
            (self.other, "other packets"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect();
        if parts.is_empty() {
            "nothing read".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Whether Game Bar is up and which program is in front, for the log.
fn quiet_context() -> String {
    format!(
        "Game Bar {}, window in front: {}",
        if crate::virt::gamebar::on_screen() { "on screen" } else { "not on screen" },
        crate::platform::foreground_exe().unwrap_or_else(|| "unknown".to_string())
    )
}

#[cfg(test)]
mod since_state_tests {
    use super::*;

    #[test]
    fn names_only_what_happened() {
        assert_eq!(SinceState::default().describe(), "nothing read");
        let s = SinceState { timeouts: 15, bad_crc: 2, ..Default::default() };
        assert_eq!(s.describe(), "15 reads timed out, 2 reports failed the CRC");
    }
}

fn quick_charge_disconnect(mac: String) -> bool {
    // Tests record the address. A real one would talk to the Bluetooth radio.
    #[cfg(test)]
    {
        QUICK_CHARGE_OFFS.lock().push(mac);
        true
    }
    #[cfg(not(test))]
    {
        crate::platform::bt_disconnect(&mac)
    }
}

#[cfg(test)]
static QUICK_CHARGE_OFFS: Mutex<Vec<String>> = Mutex::new(Vec::new());

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
    let Some(kind) = ds_proto::ds4::identify(info.vid, info.pid) else {
        return;
    };
    let model = kind.model;
    let conn = if model.is_dualshock4() {
        ds_proto::ds4::connection(model, info.input_len)
    } else {
        ds_proto::connection_from_input_len(info.input_len)
    };
    let dev = match HidDevice::open(&info) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("{} ({:?}): open failed: {e}", model.name(), conn);
            engine.note_open_failure(&info.path, &e);
            return;
        }
    };
    let (mac, firmware, mirror) = read_identity(&dev, conn, model);
    let (tx, rx) = bounded::<StreamPacket>(4);
    let (kick_tx, kick_rx) = bounded::<()>(1);
    let (haptics_wake_tx, haptics_wake) = bounded::<()>(1);
    let device = Arc::new(Device {
        id,
        info: info.clone(),
        model,
        has_motion: kind.has_motion,
        has_output: kind.has_output,
        conn,
        mac: Mutex::new(mac.clone()),
        firmware: Mutex::new(firmware.clone()),
        live: Mutex::new(Live::default()),
        stats: Stats::default(),
        link: Mutex::new(Link::Opening),
        overrides: Mutex::new(Overrides::default()),
        feedback: Mutex::new(Feedback::default()),
        toggles: AtomicToggles::default(),
        held_profile: Mutex::new(None),
        stream_tx: tx,
        stream_rx: rx,
        kick_tx,
        kick_rx,
        haptics_wake_tx,
        haptics_wake,
        pcm_mode: AtomicBool::new(false),
        classic_rumble: AtomicBool::new(false),
        usb_haptics: AtomicBool::new(false),
        haptic_level: [AtomicU8::new(0), AtomicU8::new(0)],
        speaker_level: AtomicU8::new(0),
        speaker_route: AtomicU8::new(SPEAKER_UNTOUCHED),
        headset_volume: AtomicU8::new(0),
        ds4_audio: Mutex::new(ds_proto::ds4::AudioOut::Untouched),
        mic_tap: Mutex::new(None),
        microphone: Arc::new(crate::haptics::capture::Capture::default()),
        microphone_source: AtomicU8::new(0),
        microphone_managed: AtomicBool::new(false),
        speaker_lead: AtomicU8::new(crate::haptics::streamer::SPEAKER_LEAD as u8),
        speaker_test_result: Mutex::new(None),
        calibrate_request: AtomicBool::new(false),
        mirror: Mutex::new(mirror),
        mirror_request: AtomicBool::new(false),
        mirror_retry: Mutex::new((None, 0)),
        virt: Default::default(),
        game_audio: Mutex::new(None),
        game_audio_test: Mutex::new(None),
        force_output: AtomicBool::new(true),
        stop: AtomicBool::new(false),
        dead: AtomicBool::new(false),
        quick_charge_off: AtomicBool::new(false),
        flight: Default::default(),
        last_activity: Mutex::new(Instant::now()),
    });
    log::info!(
        "connected {} over {:?}, MAC {}, firmware {}",
        model.name(),
        conn,
        if mac.is_empty() { "?" } else { &mac },
        if firmware.is_empty() { "?" } else { &firmware }
    );
    let connected_at = Instant::now();
    engine.add_device(device.clone());
    quick_charge(&engine, &device);

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
            let dropping = quick_charge_on_open(e, d);
            if dropping {
                // Quick Charge is dropping this link: let it close before
                // a virtual controller would be plugged in for it, so the
                // USB side keeps the only one.
                let t0 = Instant::now();
                while t0.elapsed() < QUICK_CHARGE_WAIT && !d.stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if !(dropping && d.stop.load(Ordering::Acquire)) {
                std::thread::Builder::new()
                    .name(format!("ds-virtual-{id}"))
                    .spawn_scoped(s, move || {
                        let _g = StopOnPanic(d);
                        crate::virt::run(e, d)
                    })
                    .expect("spawn virtual");
            }
            if conn == Connection::Bluetooth && !model.is_dualshock4() {
                std::thread::Builder::new()
                    .name(format!("ds-microphone-{id}"))
                    .spawn_scoped(s, move || {
                        let _g = StopOnPanic(d);
                        d.microphone.run(&d.stop)
                    })
                    .expect("spawn microphone");
                std::thread::Builder::new()
                    .name(format!("ds-haptics-{id}"))
                    .spawn_scoped(s, move || {
                        let _g = StopOnPanic(d);
                        crate::haptics::streamer::run(e, d)
                    })
                    .expect("spawn haptics");
            }
            if conn == Connection::Usb && !model.is_dualshock4() {
                std::thread::Builder::new()
                    .name(format!("ds-usb-haptics-{id}"))
                    .spawn_scoped(s, move || {
                        let _g = StopOnPanic(d);
                        crate::haptics::usb::run(e, d)
                    })
                    .expect("spawn USB haptics");
            }
            if d.ds4_audio_capable() {
                std::thread::Builder::new()
                    .name(format!("ds-ds4-audio-{id}"))
                    .spawn_scoped(s, move || {
                        let _g = StopOnPanic(d);
                        crate::haptics::ds4::run(e, d)
                    })
                    .expect("spawn DualShock 4 audio");
            }
        })
    }));
    if workers.is_err() {
        log::error!(
            "{}: a worker thread failed; reopening the controller",
            model.name()
        );
    }

    let battery = device.live.lock().input.battery_percent;
    device.flight.dump_if_lost(model.name(), battery);
    if conn == Connection::Bluetooth
        && device.flight.lost.load(Ordering::Acquire)
        && !device.flight.expect_off.load(Ordering::Acquire)
        && connected_at.elapsed() < EARLY_DROP
    {
        // Issue 29: links dropped within seconds of connecting, with no
        // cable and nothing in this app asking. Name what else might have.
        let apps = controller_apps(&crate::platform::process_names());
        log::warn!(
            "{} dropped Bluetooth {} ms after connecting; this app didn't ask (Quick Charge {}). Other controller apps running: {}",
            model.name(),
            connected_at.elapsed().as_millis(),
            if engine.settings.read().quick_charge { "on" } else { "off" },
            if apps.is_empty() { "none found".to_string() } else { apps.join(", ") }
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
    *device.last_activity.lock() = Instant::now();
    let mut errors = 0u32;
    let mut check_crc = engine.settings.read().check_input_crc;
    // A DualShock 4 clone often copies the report and not the CRC. Trust the
    // CRC once a packet matches; if the first packets never match, stop
    // checking so the pad still works.
    let mut crc_trusted = false;
    let mut crc_ignoring = false;
    let mut crc_misses = 0u8;
    let mut sensor_ts: Option<u32> = None;
    let mut last_packet = Instant::now();
    let mut last_kind = 0u8;
    let mut since_state = SinceState::default();
    let mut quiet_logs = 0u32;

    while !device.stop.load(Ordering::Acquire) {
        match reader.read(100) {
            Ok(Some(buf)) => {
                errors = 0;
                let gap = last_packet.elapsed();
                last_packet = Instant::now();
                if gap > crate::flight::INPUT_GAP {
                    device.flight.input_gap(gap);
                }
                let parsed = if device.model.is_dualshock4() {
                    let bt_full = matches!(buf.first(), Some(0x11..=0x19));
                    if bt_full && check_crc && !crc_ignoring {
                        match ds_proto::input::parse(buf, device.conn, device.model, true) {
                            Err(ParseError::BadCrc) if !crc_trusted => {
                                crc_misses = crc_misses.saturating_add(1);
                                if crc_misses >= 8 {
                                    crc_ignoring = true;
                                    log::info!(
                                        "DualShock 4 reports don't carry a matching Bluetooth CRC; reading this controller without the check"
                                    );
                                }
                                ds_proto::input::parse(buf, device.conn, device.model, false)
                            }
                            Ok(state) => {
                                crc_trusted = true;
                                Ok(state)
                            }
                            other => other,
                        }
                    } else {
                        ds_proto::input::parse(buf, device.conn, device.model, false)
                    }
                } else {
                    ds_proto::input::parse(buf, device.conn, device.model, check_crc)
                };
                let kind = match &parsed {
                    Err(ParseError::NotState(k)) => *k,
                    _ => 0,
                };
                if kind != last_kind {
                    last_kind = kind;
                    device.flight.input_kind(kind);
                }
                match parsed {
                    Ok(mut state) => {
                        let now = Instant::now();
                        device.stats.reports.fetch_add(1, Ordering::Relaxed);
                        count_since += 1;
                        let silent = now.duration_since(last_input);
                        last_input = now;
                        if !state.full
                            && now.duration_since(last_short_retry) > Duration::from_secs(1)
                        {
                            // Still on the short Bluetooth report: ask again.
                            // Feature 0x05 is the calibration report on both
                            // a DualSense and a Bluetooth DualShock 4, and
                            // reading it is what switches on the full report.
                            last_short_retry = now;
                            let _ = dev.get_feature(0x05, 41);
                        }
                        if is_active(&state, &prev) {
                            *device.last_activity.lock() = now;
                        }
                        let was_quiet = {
                            let mut link = device.link.lock();
                            let quiet = *link == Link::Quiet;
                            if *link != Link::Live {
                                *link = Link::Live;
                            }
                            quiet
                        };
                        if was_quiet && quiet_logs <= QUIET_LOGS {
                            log::info!(
                                "{} input back after {} ms; {}",
                                device.model.name(),
                                silent.as_millis(),
                                quiet_context()
                            );
                        }
                        since_state = SinceState::default();
                        let raw = if device.model.is_dualshock4() {
                            if state.full {
                                state.sensor_ts =
                                    ds_proto::ds4::unwrap_timestamp(sensor_ts, state.sensor_ts);
                                sensor_ts = Some(state.sensor_ts);
                                if !device.has_motion {
                                    state.motion_valid = false;
                                    state.gyro = [0; 3];
                                    state.accel = [0; 3];
                                }
                                Some(ds_proto::ds4::as_dualsense_usb(&state))
                            } else {
                                None
                            }
                        } else {
                            ds_proto::virtual_pad::usb_layout(buf)
                        };
                        pipeline.process(engine, device, &state, &prev, raw.as_ref(), now);
                        prev = state;
                    }
                    Err(ParseError::BadCrc) => {
                        device.stats.crc_errors.fetch_add(1, Ordering::Relaxed);
                        since_state.bad_crc += 1;
                    }
                    Err(ParseError::NotState(2)) => {
                        since_state.other += 1;
                        if let Some(packet) = crate::haptics::capture::Capture::packet(buf) {
                            // A voice call is activity even when no buttons
                            // move. Muted/disabled capture retains idle-off.
                            if device.microphone.recording() {
                                *device.last_activity.lock() = Instant::now();
                            }
                            device.microphone.enqueue(packet);
                            if let Some(tap) = device.mic_tap.lock().as_mut() {
                                // A stuck self-test must not grow memory forever.
                                if tap.len() < 1024 {
                                    tap.push(packet);
                                }
                            }
                        }
                    }
                    Err(_) => since_state.other += 1,
                }
            }
            Ok(None) => since_state.timeouts += 1,
            Err(e) => {
                if hid::is_fatal(&e) {
                    log::info!("input stream closed: {e}");
                    device.flight.lost.store(true, Ordering::Release);
                    device.flight.note(format!("input closed: {e}"));
                    break;
                }
                errors += 1;
                since_state.errors += 1;
                if errors % 50 == 1 {
                    log::warn!("read error (retrying): {e}");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let now = Instant::now();
        if now.duration_since(last_input) > QUIET_AFTER {
            let went_quiet = {
                let mut link = device.link.lock();
                let live = *link == Link::Live;
                if live {
                    *link = Link::Quiet;
                }
                live
            };
            if went_quiet {
                quiet_logs += 1;
                if quiet_logs <= QUIET_LOGS {
                    log::warn!(
                        "{} input stopped: no controller state for {} ms ({}); {}{}",
                        device.model.name(),
                        now.duration_since(last_input).as_millis(),
                        since_state.describe(),
                        quiet_context(),
                        if quiet_logs == QUIET_LOGS { "; further stops on this connection aren't logged" } else { "" }
                    );
                }
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
            // Quick Charge from this side: the cable went in after the USB
            // side had already looked (or that side opened first).
            if device.is_bluetooth()
                && !device.quick_charge_off.load(Ordering::Acquire)
                && engine.settings.read().quick_charge
                && on_cable_power(device)
            {
                quick_charge(engine, device);
            }
            // Idle power-off for Bluetooth controllers.
            let idle_min = engine.settings.read().idle_off_minutes;
            let last_activity = *device.last_activity.lock();
            if idle_min > 0
                && device.is_bluetooth()
                && now.duration_since(last_activity) > Duration::from_secs(idle_min as u64 * 60)
            {
                log::info!("idle for {idle_min} min, turning the controller off");
                device.flight.expect_off.store(true, Ordering::Release);
                *device.last_activity.lock() = now;
                let mac = device.mac.lock().clone();
                crate::platform::bt_disconnect(&mac);
            }
        }
    }
    pipeline.release_all();
    // Its buttons can't be let go any more; the next connection starts on
    // the profile from before the hold.
    engine.cancel_held_profile(device);
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

fn control_report<'a>(
    device: &Device,
    state: &ds_proto::OutputState,
    ds4_audio: ds_proto::ds4::AudioOut,
    buf: &'a mut [u8],
) -> &'a [u8] {
    if device.model.is_dualshock4() {
        ds_proto::ds4::output_report_with_audio(state, device.conn, ds4_audio, buf)
    } else {
        state.build(device.conn, buf)
    }
}

fn writer_loop(engine: &Arc<Engine>, device: &Arc<Device>, dev: &HidDevice) {
    crate::platform::raise_thread_priority(true);
    if !device.has_output {
        log::info!("{} has no lightbar or rumble output", device.model.name());
        while !device.stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(200));
        }
        return;
    }
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
    let mut last_microphone_managed = false;
    let mut last_write = Instant::now() - Duration::from_secs(10);
    let mut next_tick = Instant::now();
    let mut fails = 0u32;
    let mut backoff_until = Instant::now();
    let mut writes_since = 0u32;
    let mut stream_since = 0u32;
    let mut last_rate = Instant::now();
    let mut profile_cache = crate::engine::ProfileCache::default();
    let mut meter: Option<Arc<crate::haptics::audio::Sub>> = None;

    let do_write =
        |w: &mut hid::Writer, data: &[u8], stream: bool, fails: &mut u32, backoff_until: &mut Instant| -> bool {
            let t0 = Instant::now();
            let res = w.write(data, 250);
            device.flight.write(data, stream, res.is_ok(), t0.elapsed());
            match res {
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
                        device.flight.lost.store(true, Ordering::Release);
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
                            true,
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
        if device.mirror_request.swap(false, Ordering::AcqRel) {
            read_mirror_again(device, dev);
        }

        let profile = profile_cache.get(engine, device);
        if profile.lighting.mode == crate::profile::LightMode::Audio {
            let source = profile.haptics.device.as_str();
            if meter.as_ref().map(|s| s.device.as_str()) != Some(source) {
                meter = Some(engine.audio.subscribe_meter(source));
            }
        } else {
            meter = None;
        }
        let loudness = engine.audio.loudness();
        composer.idle_level = {
            let s = engine.settings.read();
            if s.idle_dim && s.idle_off_minutes > 0 && device.is_bluetooth() {
                crate::composer::idle_fade(
                    now.saturating_duration_since(*device.last_activity.lock()),
                    Duration::from_secs(s.idle_off_minutes as u64 * 60),
                )
            } else {
                1.0
            }
        };
        let input = device.live.lock().input;
        let overrides = device.overrides.lock().clone();
        let toggles = device.toggles.load();
        let mut state = {
            let mut fb = device.feedback.lock();
            fb.prune(now);
            composer.compose(
                &profile,
                &input,
                &overrides,
                &fb,
                &toggles,
                pcm || device.usb_haptics.load(Ordering::Acquire),
                loudness,
                now,
            )
        };
        device
            .classic_rumble
            .store(composer.classic_owns(), Ordering::Release);
        let mut microphone_managed = false;
        let route = device.speaker_route.load(Ordering::Acquire);
        if !device.is_bluetooth() && !device.model.is_dualshock4() && route != SPEAKER_UNTOUCHED {
            // A game's speaker sound on a wired DualSense (haptics::usb):
            // the speaker plays only with its path and volume set.
            state.audio = audio_route(route, device.headset_volume.load(Ordering::Acquire));
            state.speaker_preamp = profile.haptics.speaker_amp.preamp();
        }
        if device.is_bluetooth() && !device.model.is_dualshock4() {
            state.audio = audio_route(
                device.speaker_route.load(Ordering::Acquire),
                device.headset_volume.load(Ordering::Acquire),
            );
            state.speaker_preamp = profile.haptics.speaker_amp.preamp();
            state.microphone = device.microphone_output();
            if device.microphone.enabled() {
                // While the microphone is in use, the LED shows its mute
                // (profile or Mute button), whatever a game or mod sends.
                let m = &profile.microphone;
                state.mute_led = (m.muted || (m.mute_button && toggles.mute_engaged)) as u8;
            }
            microphone_managed = state.microphone.is_some();
            if last_microphone_managed && !microphone_managed {
                // Release hardware mute/selection once when capture is disabled.
                state.microphone = Some(ds_proto::output::Microphone {
                    source: ds_proto::output::MicrophoneSource::Auto,
                    muted: false,
                });
            }
        }
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
        let mut wrote = false;
        if due && now >= backoff_until {
            device.force_output.store(false, Ordering::Release);
            let ds4_audio = *device.ds4_audio.lock();
            let report = control_report(device, &state, ds4_audio, &mut buf);
            if do_write(&mut writer, report, false, &mut fails, &mut backoff_until) {
                if ds4_audio == ds_proto::ds4::AudioOut::Off {
                    // Switched off once; later reports are the plain ones again.
                    let mut a = device.ds4_audio.lock();
                    if *a == ds_proto::ds4::AudioOut::Off {
                        *a = ds_proto::ds4::AudioOut::Untouched;
                    }
                }
                last_microphone_managed = microphone_managed;
                last_sent = Some(state);
                last_write = now;
                writes_since += 1;
                wrote = true;
            } else {
                // Make sure the state goes out once the link is back.
                last_sent = None;
            }
        }
        if let Some((seq, raised)) = composer.take_held() {
            // The held rumble level is with the controller once a report
            // carrying it went out, or when the last one sent already had
            // the same motor levels (nothing new to send).
            let carried = last_sent.as_ref().is_some_and(|l| {
                (l.rumble_valid, l.rumble_left, l.rumble_right)
                    == (state.rumble_valid, state.rumble_left, state.rumble_right)
            });
            if carried {
                device.feedback.lock().game.rumble_sent(seq, raised && wrote, now);
            }
        }
        {
            let mut fb = device.feedback.lock();
            if wrote {
                let on = state.rumble_valid && (state.rumble_left > 0 || state.rumble_right > 0);
                fb.game.motors_written(on, now);
            }
            // A pulse the game stopped early stops on the controller once
            // its PULSE_MIN is up, not at the tick after.
            if let Some(end) = fb.game.stretch_end(now) {
                next_tick = next_tick.min(end);
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

    // Stop the microphone stream a speaker stream started, before anything
    // unhides the controller: other readers (Steam) take its microphone
    // packets for input. The haptics thread's own mic-off goes through the
    // stream channel, which nothing reads once this loop has ended.
    if device.is_bluetooth() && !device.model.is_dualshock4() {
        let mut counters = ds_proto::stream::StreamCounters::new();
        for _ in 0..2 {
            let report = ds_proto::stream::build_mic_off(&mut counters, &mut buf).to_vec();
            let _ = writer.write(&report, 100);
        }
    }

    // Leave the controller in a calm state: motors off, triggers released.
    let calm = ds_proto::OutputState {
        lightbar: last_sent.map(|s| s.lightbar).unwrap_or([0, 0, 64]),
        microphone: (last_microphone_managed || device.microphone_managed.load(Ordering::Acquire)).then_some(
            ds_proto::output::Microphone {
                source: ds_proto::output::MicrophoneSource::Auto,
                muted: false,
            },
        ),
        ..Default::default()
    };
    let ds4_audio = match *device.ds4_audio.lock() {
        ds_proto::ds4::AudioOut::Untouched => ds_proto::ds4::AudioOut::Untouched,
        _ => ds_proto::ds4::AudioOut::Off,
    };
    let report = control_report(device, &calm, ds4_audio, &mut buf).to_vec();
    let _ = writer.write(&report, 100);
}

#[cfg(test)]
impl Device {
    /// A controller with no HID handle, for mapping tests.
    pub(crate) fn for_test(conn: Connection, mac: &str) -> Arc<Self> {
        Self::for_test_at(conn, mac, "", mac)
    }

    /// [`Device::for_test`] with a HID interface path and serial.
    pub(crate) fn for_test_at(conn: Connection, mac: &str, path: &str, serial: &str) -> Arc<Self> {
        let (stream_tx, stream_rx) = bounded::<StreamPacket>(1);
        let (kick_tx, kick_rx) = bounded::<()>(1);
        let (haptics_wake_tx, haptics_wake) = bounded::<()>(1);
        Arc::new(Device {
            id: 1,
            info: DeviceInfo {
                path: path.to_string(),
                vid: ds_proto::SONY_VID,
                pid: ds_proto::DUALSENSE_PID,
                input_len: if conn == Connection::Bluetooth {
                    ds_proto::BT_INPUT_LEN
                } else {
                    64
                },
                output_len: ds_proto::USB_OUTPUT_LEN,
                feature_len: 64,
                usage_page: 0x01,
                usage: 0x05,
                serial: serial.to_string(),
                product: "DualSense".into(),
            },
            model: Model::DualSense,
            has_motion: true,
            has_output: true,
            conn,
            mac: Mutex::new(mac.to_string()),
            firmware: Mutex::new(String::new()),
            live: Mutex::new(Live::default()),
            stats: Stats::default(),
            link: Mutex::new(Link::Live),
            overrides: Mutex::new(Overrides::default()),
            feedback: Mutex::new(Feedback::default()),
            toggles: AtomicToggles::default(),
            held_profile: Mutex::new(None),
            stream_tx,
            stream_rx,
            kick_tx,
            kick_rx,
            haptics_wake_tx,
            haptics_wake,
            pcm_mode: AtomicBool::new(false),
            classic_rumble: AtomicBool::new(false),
            usb_haptics: AtomicBool::new(false),
            haptic_level: [AtomicU8::new(0), AtomicU8::new(0)],
            speaker_level: AtomicU8::new(0),
            speaker_route: AtomicU8::new(SPEAKER_UNTOUCHED),
            headset_volume: AtomicU8::new(0),
            ds4_audio: Mutex::new(ds_proto::ds4::AudioOut::Untouched),
            mic_tap: Mutex::new(None),
            microphone: Arc::new(crate::haptics::capture::Capture::default()),
            microphone_source: AtomicU8::new(0),
            microphone_managed: AtomicBool::new(false),
            speaker_lead: AtomicU8::new(0),
            speaker_test_result: Mutex::new(None),
            calibrate_request: AtomicBool::new(false),
            mirror: Mutex::new(Mirror::default()),
            mirror_request: AtomicBool::new(false),
            mirror_retry: Mutex::new((None, 0)),
            virt: Default::default(),
            game_audio: Mutex::new(None),
            game_audio_test: Mutex::new(None),
            force_output: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            dead: AtomicBool::new(false),
            quick_charge_off: AtomicBool::new(false),
            flight: Default::default(),
            last_activity: Mutex::new(Instant::now()),
        })
    }
}

/// See `Device::begin_game_audio_test`.
struct GameAudioTest {
    src: Arc<dyn crate::haptics::game::PcmSource>,
    set_aside: Option<Arc<dyn crate::haptics::game::PcmSource>>,
}

fn same_source(a: &Arc<dyn crate::haptics::game::PcmSource>, b: &Arc<dyn crate::haptics::game::PcmSource>) -> bool {
    Arc::as_ptr(a) as *const () == Arc::as_ptr(b) as *const ()
}

#[cfg(test)]
mod ds4_audio_tests {
    use super::*;

    #[test]
    fn audio_fields_start_untouched_and_only_a_dualshock4_can_use_them() {
        let d = Device::for_test(Connection::Bluetooth, "");
        assert_eq!(*d.ds4_audio.lock(), ds_proto::ds4::AudioOut::Untouched);
        assert!(!d.ds4_audio_capable(), "a DualSense has its own speaker path");
    }
}

#[cfg(test)]
mod microphone_tests {
    use super::*;
    use ds_proto::output::{Microphone, MicrophoneSource};

    #[test]
    fn atomic_toggles_round_trip_every_combination() {
        let at = AtomicToggles::default();
        assert_eq!(at.load(), Toggles::default());
        for gyro in [None, Some(false), Some(true)] {
            for touchpad in [None, Some(false), Some(true)] {
                for mute_engaged in [false, true] {
                    for gyro_toggled_on in [false, true] {
                        let want = Toggles {
                            gyro,
                            touchpad,
                            mute_engaged,
                            gyro_toggled_on,
                        };
                        at.update(|t| *t = want);
                        assert_eq!(at.load(), want);
                    }
                }
            }
        }
    }

    #[test]
    fn a_profile_without_a_usable_microphone_changes_nothing() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let mut mic = crate::profile::Microphone {
            muted: true,
            source: crate::profile::MicrophoneSource::Headset,
            gain_percent: 50,
            ..Default::default()
        };
        d.toggles.update(|t| t.mute_engaged = true);
        d.apply_microphone(&mic);
        // On, but with no virtual DualSense to record from.
        mic.enabled = true;
        d.apply_microphone(&mic);
        assert_eq!(d.microphone_output(), None);
        assert!(!d.microphone.enabled() && !d.microphone.recording());
        assert_eq!(d.speaker_route.load(Ordering::Acquire), SPEAKER_UNTOUCHED);
    }

    #[test]
    fn diagnostics_do_not_claim_microphone_output_without_capture() {
        let d = Device::for_test(Connection::Bluetooth, "");
        *d.mic_tap.lock() = Some(Vec::new());
        assert_eq!(d.microphone_output(), None);
        d.mic_tap.lock().take();
        d.set_microphone(false, true, 100, crate::profile::MicrophoneSource::Auto);
        assert_eq!(d.microphone_output(), None);
        d.set_microphone(true, false, 100, crate::profile::MicrophoneSource::BuiltIn);
        assert!(d.microphone_output().is_some());
        d.set_microphone(false, false, 100, crate::profile::MicrophoneSource::Auto);
        assert_eq!(d.microphone_output(), None);
        assert!(!d.microphone_managed.load(Ordering::Acquire));
    }

    #[test]
    fn speaker_test_temporarily_unmutes_internal_mic_then_restores_capture() {
        let d = Device::for_test(Connection::Bluetooth, "");
        assert_eq!(d.microphone_output(), None);
        d.set_microphone(true, true, 100, crate::profile::MicrophoneSource::Headset);
        let selected = Some(Microphone {
            source: MicrophoneSource::Headset,
            muted: true,
        });
        assert_eq!(d.microphone_output(), selected);
        *d.mic_tap.lock() = Some(Vec::new());
        assert_eq!(
            d.microphone_output(),
            Some(Microphone {
                source: MicrophoneSource::BuiltIn,
                muted: false,
            })
        );
        d.mic_tap.lock().take();
        assert_eq!(d.microphone_output(), selected);
        d.set_microphone(false, false, 100, crate::profile::MicrophoneSource::Headset);
        assert_eq!(d.microphone_output(), None);
        assert_eq!(d.speaker_route.load(Ordering::Acquire), SPEAKER_MUTED);
    }
}

#[cfg(test)]
mod quick_charge_tests {
    use super::*;

    struct Rig {
        engine: Arc<Engine>,
        dir: std::path::PathBuf,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rig(quick_charge: bool, devices: &[&Arc<Device>]) -> Rig {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dsz-quick-charge-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let engine = Engine::new(dir.clone());
        assert!(engine.settings.read().quick_charge, "a new settings folder starts with it on");
        engine.settings.write().quick_charge = quick_charge;
        engine.devices.write().extend(devices.iter().map(|d| (*d).clone()));
        Rig { engine, dir }
    }

    fn offs(mac: &str) -> usize {
        QUICK_CHARGE_OFFS.lock().iter().filter(|m| *m == mac).count()
    }

    /// What the Bluetooth side reports once a cable is in.
    fn on_cable(bt: &Device) {
        bt.live.lock().input.battery_status = ds_proto::input::BatteryStatus::Charging;
    }
    #[test]
    fn turned_off_the_same_controller_on_usb_and_bluetooth_stays_connected() {
        let mac = "A0-00-00-00-00-01";
        let bt = Device::for_test(Connection::Bluetooth, mac);
        let usb = Device::for_test(Connection::Usb, mac);
        let r = rig(false, &[&bt, &usb]);
        on_cable(&bt);
        quick_charge(&r.engine, &usb);
        quick_charge(&r.engine, &bt);
        assert_eq!(offs(mac), 0, "no Bluetooth disconnect while Quick Charge is off");
        assert!(!bt.flight.expect_off.load(Ordering::Acquire));
        assert!(!bt.quick_charge_off.load(Ordering::Acquire));
        assert!(r.engine.toasts.lock().is_empty());
        assert_eq!(r.engine.device_list().len(), 2);
    }

    #[test]
    fn usb_plugged_in_drops_the_bluetooth_link_once() {
        let mac = "A0-00-00-00-00-02";
        let bt = Device::for_test(Connection::Bluetooth, mac);
        let usb = Device::for_test(Connection::Usb, mac);
        let r = rig(true, &[&bt, &usb]);
        on_cable(&bt);
        quick_charge(&r.engine, &usb);
        assert_eq!(offs(mac), 1);
        assert!(bt.flight.expect_off.load(Ordering::Acquire), "not logged as a dropped link");
        assert!(!usb.flight.expect_off.load(Ordering::Acquire));
        // The Bluetooth side checking too (both opened at startup) asks once.
        quick_charge(&r.engine, &bt);
        quick_charge(&r.engine, &usb);
        assert_eq!(offs(mac), 1);
    }

    #[test]
    fn bluetooth_connecting_while_plugged_in_is_dropped() {
        let mac = "A0-00-00-00-00-03";
        let usb = Device::for_test(Connection::Usb, mac);
        let bt = Device::for_test(Connection::Bluetooth, mac);
        let r = rig(true, &[&usb, &bt]);
        on_cable(&bt);
        quick_charge(&r.engine, &bt);
        assert_eq!(offs(mac), 1);
        assert!(bt.flight.expect_off.load(Ordering::Acquire));
    }

    /// At startup the Bluetooth side can open second, before any of its
    /// input has been read: its battery status is still the default
    /// (discharging). It looks again once its full reports are in, before
    /// its virtual controller starts, so no second pad is plugged in.
    #[test]
    fn bluetooth_opening_second_checks_again_before_its_virtual_controller() {
        let mac = "A0-00-00-00-00-0C";
        let usb = Device::for_test(Connection::Usb, mac);
        let bt = Device::for_test(Connection::Bluetooth, mac);
        let r = rig(true, &[&usb, &bt]);
        quick_charge(&r.engine, &bt);
        assert_eq!(offs(mac), 0, "no input read yet");
        {
            let mut live = bt.live.lock();
            live.input.full = true;
            live.input.battery_status = ds_proto::input::BatteryStatus::Charging;
        }
        assert!(quick_charge_on_open(&r.engine, &bt), "dropping: no virtual controller for it");
        assert_eq!(offs(mac), 1);

        // On battery it keeps its link and gets its virtual controller.
        let mac = "A0-00-00-00-00-0D";
        let usb = Device::for_test(Connection::Usb, mac);
        let bt = Device::for_test(Connection::Bluetooth, mac);
        bt.live.lock().input.full = true;
        let r = rig(true, &[&usb, &bt]);
        assert!(!quick_charge_on_open(&r.engine, &bt));
        assert_eq!(offs(mac), 0);
        // A USB side, or Quick Charge off, never waits or drops.
        assert!(!quick_charge_on_open(&r.engine, &usb));
        let r = rig(false, &[&usb, &bt]);
        on_cable(&bt);
        assert!(!quick_charge_on_open(&r.engine, &bt));
        assert_eq!(offs(mac), 0);
    }

    #[test]
    fn only_the_same_controller_on_the_other_transport_counts() {
        let a = "A0-00-00-00-00-04";
        let b = "A0-00-00-00-00-05";
        let bt = Device::for_test(Connection::Bluetooth, a);
        let other_usb = Device::for_test(Connection::Usb, b);
        let second_bt = Device::for_test(Connection::Bluetooth, a);
        on_cable(&bt);
        on_cable(&second_bt);
        let r = rig(true, &[&bt, &other_usb, &second_bt]);
        quick_charge(&r.engine, &other_usb);
        quick_charge(&r.engine, &second_bt);
        assert_eq!(offs(a) + offs(b), 0, "another controller, or Bluetooth twice, is left alone");

        // No usable address: nothing to match or disconnect.
        for mac in ["", "00-00-00-00-00-00"] {
            let bt = Device::for_test(Connection::Bluetooth, mac);
            let usb = Device::for_test(Connection::Usb, mac);
            on_cable(&bt);
            let r = rig(true, &[&bt, &usb]);
            quick_charge(&r.engine, &usb);
            assert_eq!(offs(mac), 0);
            assert!(!bt.flight.expect_off.load(Ordering::Acquire));
        }

        // A Bluetooth connection already closing is not asked again.
        let c = "A0-00-00-00-00-06";
        let bt = Device::for_test(Connection::Bluetooth, c);
        bt.stop.store(true, Ordering::Release);
        on_cable(&bt);
        let usb = Device::for_test(Connection::Usb, c);
        let r = rig(true, &[&bt, &usb]);
        quick_charge(&r.engine, &usb);
        assert_eq!(offs(c), 0);
    }

    #[test]
    fn early_drops_name_other_controller_apps() {
        let running: std::collections::HashSet<String> =
            ["explorer.exe", "steam.exe", "ds4windows.exe"].iter().map(|s| s.to_string()).collect();
        assert_eq!(controller_apps(&running), vec!["ds4windows.exe", "steam.exe"]);
        assert!(controller_apps(&Default::default()).is_empty());
    }

    /// Issue 29: Bluetooth dropped with no cable plugged in. Whatever else
    /// shows the same address, a controller running on its battery keeps
    /// its Bluetooth link.
    #[test]
    fn no_cable_power_keeps_bluetooth_until_the_pad_reports_it() {
        let mac = "A0-00-00-00-00-07";
        let bt = Device::for_test(Connection::Bluetooth, mac);
        let usb = Device::for_test(Connection::Usb, mac);
        let r = rig(true, &[&bt, &usb]);
        quick_charge(&r.engine, &usb);
        quick_charge(&r.engine, &bt);
        assert_eq!(offs(mac), 0, "on battery: not plugged in, whatever else has its address");
        assert!(!bt.flight.expect_off.load(Ordering::Acquire));
        assert!(!bt.quick_charge_off.load(Ordering::Acquire), "free to drop once the cable shows");
        // The reader's once-a-second check from the Bluetooth side.
        on_cable(&bt);
        quick_charge(&r.engine, &bt);
        assert_eq!(offs(mac), 1);
        quick_charge(&r.engine, &bt);
        assert_eq!(offs(mac), 1, "asked once");
        // Full counts as cable power too; discharging after the drop changes nothing.
        let mac2 = "A0-00-00-00-00-08";
        let bt2 = Device::for_test(Connection::Bluetooth, mac2);
        bt2.live.lock().input.battery_status = ds_proto::input::BatteryStatus::Full;
        let usb2 = Device::for_test(Connection::Usb, mac2);
        let r2 = rig(true, &[&bt2, &usb2]);
        quick_charge(&r2.engine, &usb2);
        assert_eq!(offs(mac2), 1);
    }

    #[test]
    fn only_a_real_usb_connection_counts_as_plugged_in() {
        // The app's own virtual DualSense answers with the physical
        // controller's address; even if it were ever opened as a
        // controller it is not a cable.
        let mac = "A0-00-00-00-00-09";
        let bt = Device::for_test(Connection::Bluetooth, mac);
        on_cable(&bt);
        let ours = Device::for_test_at(
            Connection::Usb,
            mac,
            r"\\?\hid#vid_054c&pid_0ce6&mi_03#8&1&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
            "DSZV1",
        );
        let r = rig(true, &[&bt, &ours]);
        quick_charge(&r.engine, &ours);
        quick_charge(&r.engine, &bt);
        assert_eq!(offs(mac), 0, "our virtual pad");

        // A Bluetooth collection taken for USB (any input length but the
        // Bluetooth one reads as USB).
        let mac = "A0-00-00-00-00-0A";
        let bt = Device::for_test(Connection::Bluetooth, mac);
        on_cable(&bt);
        let bt_as_usb = Device::for_test_at(
            Connection::Usb,
            mac,
            r"\\?\HID#{00001124-0000-1000-8000-00805F9B34FB}_VID&0002054C_PID&0CE6#9&2&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
            mac,
        );
        let r = rig(true, &[&bt, &bt_as_usb]);
        quick_charge(&r.engine, &bt_as_usb);
        quick_charge(&r.engine, &bt);
        assert_eq!(offs(mac), 0, "a Bluetooth path");

        // A real USB path still works.
        let mac = "A0-00-00-00-00-0B";
        let bt = Device::for_test(Connection::Bluetooth, mac);
        on_cable(&bt);
        let usb = Device::for_test_at(
            Connection::Usb,
            mac,
            r"\\?\hid#vid_054c&pid_0df2&mi_03#8&2a&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}",
            "",
        );
        let r = rig(true, &[&bt, &usb]);
        quick_charge(&r.engine, &usb);
        assert_eq!(offs(mac), 1);
    }
}

#[cfg(test)]
mod game_audio_test_tests {
    use super::*;
    use crate::haptics::game::{PcmSource, TestSignal, TestSource};

    fn test_signal() -> Arc<dyn PcmSource> {
        Arc::new(TestSource::new(Duration::from_secs(3), TestSignal::Actuators))
    }

    fn holds(d: &Device, src: &Arc<dyn PcmSource>) -> bool {
        d.game_audio.lock().as_ref().is_some_and(|x| same_source(x, src))
    }

    #[test]
    fn plays_with_no_virtual_dualsense() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let t = test_signal();
        assert!(d.begin_game_audio_test(t.clone()));
        assert!(holds(&d, &t));
        assert!(d.game_audio_busy());
        d.end_game_audio_test(&t);
        assert!(d.game_audio.lock().is_none());
        assert!(!d.game_audio_busy());
    }

    #[test]
    fn sets_an_idle_virtual_dualsense_aside_and_puts_it_back() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let pad = test_signal();
        *d.game_audio.lock() = Some(pad.clone());
        let t = test_signal();
        assert!(d.begin_game_audio_test(t.clone()));
        assert!(holds(&d, &t));
        // A second test waits for the first.
        assert!(!d.begin_game_audio_test(test_signal()));
        d.end_game_audio_test(&t);
        assert!(holds(&d, &pad));
        assert!(!d.game_audio_busy());
    }

    #[test]
    fn leaves_a_game_playing_alone() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let pad = test_signal();
        *d.game_audio.lock() = Some(pad.clone());
        d.virt.status.lock().game_audio_open = true;
        assert!(d.game_audio_busy());
        let t = test_signal();
        assert!(!d.begin_game_audio_test(t.clone()));
        assert!(holds(&d, &pad));
        d.end_game_audio_test(&t);
        assert!(holds(&d, &pad));
    }

    #[test]
    fn a_new_virtual_pad_during_the_test_keeps_its_stream() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let old = test_signal();
        *d.game_audio.lock() = Some(old.clone());
        let t = test_signal();
        assert!(d.begin_game_audio_test(t.clone()));
        let new = test_signal();
        *d.game_audio.lock() = Some(new.clone());
        d.end_game_audio_test(&t);
        assert!(holds(&d, &new));
        assert!(!d.game_audio_busy());
    }

    #[test]
    fn a_virtual_pad_removed_during_the_test_stays_removed() {
        let d = Device::for_test(Connection::Bluetooth, "");
        *d.game_audio.lock() = Some(test_signal());
        let t = test_signal();
        assert!(d.begin_game_audio_test(t.clone()));
        *d.game_audio.lock() = None;
        d.end_game_audio_test(&t);
        assert!(d.game_audio.lock().is_none());
    }
}

#[cfg(test)]
mod rumble_pulse_tests {
    //! Issue 43: GTA V pulses its rumble, a level in one report and zero in
    //! the next. A level the game set must reach the controller in at least
    //! one report, even when its zero comes before the writer's next report.
    use super::*;
    use crate::composer::Composer;
    use crate::profile::Profile;
    use ds_proto::OutputState;

    fn compose(c: &mut Composer, d: &Device, p: &Profile, pcm: bool, now: Instant) -> OutputState {
        let fb = d.feedback.lock();
        c.compose(
            p,
            &InputState::default(),
            &Overrides::default(),
            &fb,
            &Toggles::default(),
            pcm,
            0.0,
            now,
        )
    }

    /// A game's USB output report to the virtual DualSense: classic rumble
    /// (heavy, light) with the rumble flags set, as GTA V sends it.
    fn ds_rumble_report(heavy: u8, light: u8) -> Vec<u8> {
        let mut r = vec![0u8; ds_proto::virtual_pad::DS_USB_OUTPUT_LEN];
        r[0] = 0x02;
        r[1] = 0x03;
        r[2] = 0x40;
        r[3] = light;
        r[4] = heavy;
        r
    }

    #[test]
    fn a_pulse_cleared_before_the_next_report_still_goes_out() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let p = Profile::default();
        let mut c = Composer::new();
        let now = Instant::now();
        let idle = compose(&mut c, &d, &p, false, now);
        assert_eq!((idle.rumble_left, idle.rumble_right), (0, 0));
        // An Xbox 360 or DualShock 4 virtual pad: on, then off, both
        // between two of the writer's reports.
        d.set_game_rumble(111, 0);
        d.set_game_rumble(0, 0);
        let o = compose(&mut c, &d, &p, false, now + Duration::from_millis(4));
        assert!(o.rumble_valid);
        assert_eq!((o.rumble_left, o.rumble_right), (111, 0), "the pulse was lost");
    }

    #[test]
    fn a_virtual_dualsense_pulse_survives_classic_rumble_always_with_the_speaker_stream() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let mut p = Profile::default();
        p.rumble.always_classic = true;
        let mut c = Composer::new();
        let now = Instant::now();
        // The speaker stream runs (pcm), nothing rumbles: the stream has the
        // actuators.
        assert!(!compose(&mut c, &d, &p, true, now).rumble_valid);
        d.apply_game_output(&ds_rumble_report(111, 0));
        d.apply_game_output(&ds_rumble_report(0, 0));
        let o = compose(&mut c, &d, &p, true, now + Duration::from_millis(8));
        assert!(o.rumble_valid);
        assert_eq!((o.rumble_left, o.rumble_right), (111, 0), "the pulse was lost");
        assert!(c.classic_owns());
    }

    #[test]
    fn a_game_that_never_rumbles_gets_the_same_report_bytes() {
        // Reports that set no rumble leave every control report byte as
        // it was, in every mode.
        let bytes = |o: &OutputState| {
            let mut b = [0u8; 128];
            o.build(Connection::Bluetooth, &mut b).to_vec()
        };
        let now = Instant::now();
        for always_classic in [false, true] {
            for pcm in [false, true] {
                let mut p = Profile::default();
                p.rumble.always_classic = always_classic;
                let quiet = Device::for_test(Connection::Bluetooth, "");
                let busy = Device::for_test(Connection::Bluetooth, "");
                // A report with no rumble flags, then zero rumble.
                let mut plain = vec![0u8; ds_proto::virtual_pad::DS_USB_OUTPUT_LEN];
                plain[0] = 0x02;
                busy.apply_game_output(&plain);
                busy.set_game_rumble(0, 0);
                let (mut a, mut b) = (Composer::new(), Composer::new());
                for ms in [0u64, 4, 8, 16, 200] {
                    let t = now + Duration::from_millis(ms);
                    let x = compose(&mut a, &quiet, &p, pcm, t);
                    let y = compose(&mut b, &busy, &p, pcm, t);
                    assert_eq!(bytes(&x), bytes(&y), "classic {always_classic}, pcm {pcm}, {ms} ms");
                }
            }
        }
    }

    /// One writer step: compose, "send", and let the held level go as the
    /// writer does once the report is out.
    fn send(c: &mut Composer, d: &Device, p: &Profile, pcm: bool, now: Instant) -> (bool, u8, u8) {
        let o = compose(c, d, p, pcm, now);
        if let Some((seq, raised)) = c.take_held() {
            d.feedback.lock().game.rumble_sent(seq, raised, now);
        }
        (o.rumble_valid, o.rumble_left, o.rumble_right)
    }

    #[test]
    fn a_held_pulse_goes_out_once_then_the_zero_follows() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let p = Profile::default();
        let mut c = Composer::new();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        d.set_game_rumble(111, 0);
        d.set_game_rumble(0, 0);
        assert_eq!(send(&mut c, &d, &p, false, at(4)), (true, 111, 0));
        assert!(d.feedback.lock().game.held.is_none());
        assert_eq!(d.feedback.lock().game.rumble_saved, 1);
        // Kept on for PULSE_MIN from that report, then the game's zero.
        let min = PULSE_MIN.as_millis() as u64;
        assert_eq!(send(&mut c, &d, &p, false, at(8)), (true, 111, 0));
        assert_eq!(send(&mut c, &d, &p, false, at(4 + min - 1)), (true, 111, 0));
        assert_eq!(send(&mut c, &d, &p, false, at(4 + min)), (true, 0, 0));
    }

    #[test]
    fn a_short_pulse_stays_on_the_controller_for_pulse_min() {
        let mut g = GameState::default();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        let min = PULSE_MIN.as_millis() as u64;
        // The game's 0.2 ms pulse; a report carries it 3 ms later.
        g.set_rumble(111, 0, at(0));
        g.set_rumble(0, 0, at(0) + Duration::from_micros(200));
        let (level, held) = g.rumble_to_send(at(3));
        assert_eq!(level, (111, 0));
        g.rumble_sent(held.unwrap().0, true, at(3));
        // The full PULSE_MIN counts from that report, not from the pulse.
        assert_eq!(g.stretch_end(at(3)), Some(at(3 + min)));
        assert_eq!(g.rumble_to_send(at(3 + min - 1)), ((111, 0), None));
        assert_eq!(g.rumble_to_send(at(3 + min)), ((0, 0), None));
        assert_eq!(g.stretch_end(at(3 + min)), None);
        assert_eq!(g.rumble_stretched, 1);
    }

    #[test]
    fn a_pulse_longer_than_pulse_min_stops_at_once() {
        let mut g = GameState::default();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        let min = PULSE_MIN.as_millis() as u64;
        g.set_rumble(150, 0, at(0));
        let (_, held) = g.rumble_to_send(at(0));
        g.rumble_sent(held.unwrap().0, false, at(0));
        g.set_rumble(0, 0, at(min + 5));
        assert_eq!(g.rumble_to_send(at(min + 5)), ((0, 0), None));
        assert_eq!(g.stretch_end(at(min + 5)), None);
        assert_eq!(g.rumble_stretched, 0);
    }

    #[test]
    fn a_new_pulse_during_a_stretch_takes_over_at_once() {
        let mut g = GameState::default();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        g.set_rumble(200, 0, at(0));
        let (_, held) = g.rumble_to_send(at(0));
        g.rumble_sent(held.unwrap().0, true, at(0));
        g.set_rumble(0, 0, at(1));
        // A weaker pulse 5 ms later plays as it is, then gets its own
        // PULSE_MIN from the report that carries it.
        g.set_rumble(60, 0, at(5));
        g.set_rumble(0, 0, at(6));
        let (level, held) = g.rumble_to_send(at(8));
        assert_eq!(level, (60, 0));
        g.rumble_sent(held.unwrap().0, false, at(8));
        assert_eq!(g.rumble_to_send(at(20)), ((60, 0), None));
        assert_eq!(g.stretch_end(at(20)), Some(at(8) + PULSE_MIN));
        assert_eq!(g.rumble_stretched, 2);
    }

    #[test]
    fn the_session_log_times_the_pattern_and_what_the_controller_played() {
        let mut g = GameState::default();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        for (start, level) in [(0u64, (111, 0)), (90, (0, 70)), (130, (120, 0))] {
            g.set_rumble(level.0, level.1, at(start));
            g.set_rumble(0, 0, at(start + 1));
        }
        assert_eq!(g.rumble_gaps, 2);
        assert_eq!(g.rumble_gap_min, Some(Duration::from_millis(40)));
        assert_eq!(g.rumble_gaps_short, 1);
        assert_eq!((g.rumble_on_heavy, g.rumble_on_light), (2, 1));
        assert_eq!(g.rumble_peak, (120, 70));
        g.motors_written(true, at(0));
        g.motors_written(true, at(8));
        g.motors_written(false, at(16));
        g.motors_written(false, at(24));
        g.motors_written(true, at(90));
        g.motors_written(false, at(110));
        assert_eq!(g.out_runs, 2);
        assert_eq!(g.out_run_min, Some(Duration::from_millis(16)));
        assert_eq!(g.out_run_max, Duration::from_millis(20));
    }

    #[test]
    fn steady_rumble_and_a_carried_stop_are_unchanged() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let p = Profile::default();
        let mut c = Composer::new();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        // Steady: the game repeats its level; every report has it.
        for ms in (0..80).step_by(8) {
            d.set_game_rumble(150, 40);
            assert_eq!(send(&mut c, &d, &p, false, at(ms)), (true, 150, 40), "{ms}");
        }
        // Stopped after a report carried the level: the zero goes out in
        // the very next report, no longer on than before.
        d.set_game_rumble(0, 0);
        assert_eq!(send(&mut c, &d, &p, false, at(84)), (true, 0, 0));
        // Lowered after a report carried the level: the lower level at once.
        d.set_game_rumble(200, 0);
        assert_eq!(send(&mut c, &d, &p, false, at(88)), (true, 200, 0));
        d.set_game_rumble(60, 0);
        assert_eq!(send(&mut c, &d, &p, false, at(92)), (true, 60, 0));
        assert_eq!(d.feedback.lock().game.rumble_saved, 0);
    }

    #[test]
    fn a_higher_level_cut_to_a_lower_one_before_a_report_goes_out_once() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let p = Profile::default();
        let mut c = Composer::new();
        let t = Instant::now();
        d.set_game_rumble(200, 10);
        d.set_game_rumble(80, 90);
        // Each motor's highest level since the last report, once.
        assert_eq!(send(&mut c, &d, &p, false, t), (true, 200, 90));
        assert_eq!(send(&mut c, &d, &p, false, t + Duration::from_millis(8)), (true, 80, 90));
    }

    #[test]
    fn a_newer_pulse_is_not_let_go_by_an_older_report() {
        let mut g = GameState::default();
        let t = Instant::now();
        g.set_rumble(111, 0, t);
        let (_, first) = g.rumble_to_send(t);
        // Another pulse comes in while that report is on its way.
        g.set_rumble(0, 0, t);
        g.set_rumble(90, 0, t + Duration::from_millis(1));
        g.set_rumble(0, 0, t + Duration::from_millis(2));
        let (seq, _) = first.unwrap();
        g.rumble_sent(seq, true, t);
        assert_eq!(g.rumble_to_send(t + Duration::from_millis(3)).0, (111, 0), "merged and still held");
        let (_, second) = g.rumble_to_send(t + Duration::from_millis(3));
        g.rumble_sent(second.unwrap().0, true, t + Duration::from_millis(3));
        // Let go; the last pulse then plays out its PULSE_MIN.
        assert_eq!(g.rumble_to_send(t + Duration::from_millis(4)), ((90, 0), None));
        assert_eq!(g.rumble_to_send(t + Duration::from_millis(3) + PULSE_MIN), ((0, 0), None));
    }

    #[test]
    fn a_held_level_nothing_took_is_dropped_not_played_late() {
        let mut g = GameState::default();
        let t = Instant::now();
        g.set_rumble(111, 0, t);
        g.set_rumble(0, 0, t);
        assert_eq!(g.rumble_to_send(t + PULSE_STALE - Duration::from_millis(1)).0, (111, 0));
        assert_eq!(g.rumble_to_send(t + PULSE_STALE), ((0, 0), None));
    }

    #[test]
    fn the_session_log_counts_merged_late_and_uncarried_pulses() {
        let mut g = GameState::default();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        // Steady rumble the game repeats is not a merge.
        for ms in 0..3 {
            g.set_rumble(150, 0, at(ms));
        }
        assert_eq!(g.rumble_merged, 0);
        // Two pulses before a report: one merge, carried 12 ms after the first.
        g.set_rumble(0, 0, at(3));
        g.set_rumble(90, 0, at(4));
        g.set_rumble(0, 0, at(5));
        assert_eq!(g.rumble_merged, 1);
        let (_, held) = g.rumble_to_send(at(12));
        g.rumble_sent(held.unwrap().0, true, at(12));
        assert_eq!((g.rumble_carried, g.rumble_wait_max, g.rumble_waits_long), (1, Duration::from_millis(12), 1));
        // A pulse no report took, then the next one: one never carried.
        g.set_rumble(80, 0, at(20));
        g.set_rumble(0, 0, at(21));
        g.set_rumble(80, 0, at(20) + PULSE_STALE);
        assert_eq!((g.rumble_uncarried, g.rumble_merged), (1, 1));
    }

    #[test]
    fn while_the_stream_plays_rumble_the_control_report_is_unchanged() {
        // Classic rumble always off and haptics streaming: rumble plays on
        // the stream (the streamer takes the held level), and the control
        // report is byte for byte what it was.
        let bytes = |o: &OutputState| {
            let mut b = [0u8; 128];
            o.build(Connection::Bluetooth, &mut b).to_vec()
        };
        let d = Device::for_test(Connection::Bluetooth, "");
        let p = Profile::default();
        let t = Instant::now();
        let mut c = Composer::new();
        d.set_game_rumble(111, 0);
        d.set_game_rumble(0, 0);
        let got = compose(&mut c, &d, &p, true, t);
        assert!(c.take_held().is_none());
        let want = Composer::new().compose(
            &p,
            &InputState::default(),
            &Overrides::default(),
            &Feedback::default(),
            &Toggles::default(),
            true,
            0.0,
            t,
        );
        assert_eq!(bytes(&got), bytes(&want));
        assert!(d.feedback.lock().game.held.is_some(), "left for the stream");
    }

    #[test]
    fn pulses_keep_the_classic_handover() {
        // Classic rumble always with the speaker stream: a held pulse takes
        // the actuators, the zero follows, and CLASSIC_HOLD after the pulse
        // they go back to the stream.
        let d = Device::for_test(Connection::Bluetooth, "");
        let mut p = Profile::default();
        p.rumble.always_classic = true;
        let mut c = Composer::new();
        let t = Instant::now();
        let at = |ms: u64| t + Duration::from_millis(ms);
        d.set_game_rumble(111, 0);
        d.set_game_rumble(0, 0);
        assert_eq!(send(&mut c, &d, &p, true, at(0)), (true, 111, 0));
        assert!(c.classic_owns());
        assert_eq!(send(&mut c, &d, &p, true, at(16)), (true, 0, 0));
        assert!(c.classic_owns());
        let back = crate::composer::CLASSIC_HOLD.as_millis() as u64;
        assert_eq!(send(&mut c, &d, &p, true, at(back)), (false, 0, 0));
        assert!(!c.classic_owns());
    }

}
