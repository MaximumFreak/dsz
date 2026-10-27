//! Per-report input processing on the reader thread: gyro mouse, touchpad
//! mouse, stick modes, and button mappings. Everything here runs inline
//! with the HID read, so a report turns into cursor motion with no queue
//! in between.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ds_proto::input::{Button, TouchPoint};
use ds_proto::InputState;

use crate::device::Device;
use crate::engine::{Engine, ProfileCache};
use crate::inputsim as sim;
use crate::profile::*;

// ------------------------------------------------------------------ gyro

const COUNTS_PER_DPS: f32 = 16.0;

struct Gyro {
    bias: [f32; 3],
    calibrated: bool,
    still_since: Option<Instant>,
    still_sum: [f64; 3],
    still_n: u32,
    manual_until: Option<Instant>,
    last_ts: Option<u32>,
    last_at: Option<Instant>,
    smooth: [f32; 2],
    rem: [f32; 2],
}

impl Gyro {
    fn new() -> Self {
        Gyro {
            bias: [0.0; 3],
            calibrated: false,
            still_since: None,
            still_sum: [0.0; 3],
            still_n: 0,
            manual_until: None,
            last_ts: None,
            last_at: None,
            smooth: [0.0; 2],
            rem: [0.0; 2],
        }
    }

    fn dt(&mut self, s: &InputState, now: Instant) -> f32 {
        let wall = self.last_at.map(|t| now.duration_since(t).as_secs_f32());
        self.last_at = Some(now);
        let sensor = self
            .last_ts
            .map(|t| s.sensor_ts.wrapping_sub(t) as f32 / 3_000_000.0);
        self.last_ts = Some(s.sensor_ts);
        // The sensor clock, unless a gap or wrap makes it implausible; then
        // the wall clock, bounded to one to four nominal report periods.
        match (sensor, wall) {
            (Some(d), _) if (0.001..=0.05).contains(&d) => d,
            (_, Some(w)) => w.clamp(0.001, 0.016),
            _ => 1.0 / 250.0,
        }
    }

    /// Track stillness and refine the bias. Returns calibrated deg/s.
    fn update_bias(&mut self, s: &InputState, now: Instant) -> [f32; 3] {
        let raw = [s.gyro[0] as f32, s.gyro[1] as f32, s.gyro[2] as f32];
        let acc = ((s.accel[0] as f32).powi(2)
            + (s.accel[1] as f32).powi(2)
            + (s.accel[2] as f32).powi(2))
        .sqrt();

        if let Some(until) = self.manual_until {
            for k in 0..3 {
                self.still_sum[k] += raw[k] as f64;
            }
            self.still_n += 1;
            if now >= until {
                if self.still_n > 50 {
                    for k in 0..3 {
                        self.bias[k] = (self.still_sum[k] / self.still_n as f64) as f32;
                    }
                    self.calibrated = true;
                    log::info!("gyro calibrated: bias {:?}", self.bias);
                }
                self.manual_until = None;
                self.reset_still();
            }
        } else {
            let rel = [
                raw[0] - self.bias[0],
                raw[1] - self.bias[1],
                raw[2] - self.bias[2],
            ];
            let quiet_limit = if self.calibrated { 2.5 } else { 10.0 } * COUNTS_PER_DPS;
            let quiet = rel.iter().all(|v| v.abs() < quiet_limit) && (acc - 8192.0).abs() < 900.0;
            if quiet {
                if self.still_since.is_none() {
                    self.still_since = Some(now);
                }
                for k in 0..3 {
                    self.still_sum[k] += raw[k] as f64;
                }
                self.still_n += 1;
                if now.duration_since(self.still_since.unwrap()) > Duration::from_millis(1500)
                    && self.still_n > 100
                {
                    let avg: Vec<f32> = (0..3)
                        .map(|k| (self.still_sum[k] / self.still_n as f64) as f32)
                        .collect();
                    for k in 0..3 {
                        self.bias[k] = if self.calibrated {
                            self.bias[k] * 0.6 + avg[k] * 0.4
                        } else {
                            avg[k]
                        };
                    }
                    if !self.calibrated {
                        log::debug!("gyro auto-calibrated: bias {:?}", self.bias);
                    }
                    self.calibrated = true;
                    self.reset_still();
                }
            } else {
                self.reset_still();
            }
        }
        [
            (raw[0] - self.bias[0]) / COUNTS_PER_DPS,
            (raw[1] - self.bias[1]) / COUNTS_PER_DPS,
            (raw[2] - self.bias[2]) / COUNTS_PER_DPS,
        ]
    }

    fn reset_still(&mut self) {
        self.still_since = None;
        self.still_sum = [0.0; 3];
        self.still_n = 0;
    }

    fn mouse(&mut self, g: &crate::profile::Gyro, dps: [f32; 3], dt: f32) {
        let (pitch, yaw, roll) = (dps[0], dps[1], dps[2]);
        let mut h = match g.axis {
            GyroAxis::Yaw => -yaw,
            GyroAxis::Roll => -roll,
            GyroAxis::Combined => -(yaw + roll),
        };
        let mut v = -pitch;
        if g.invert_x {
            h = -h;
        }
        if g.invert_y {
            v = -v;
        }
        let speed = (h * h + v * v).sqrt();
        // Soft deadzone: shrink tiny motion instead of chopping it.
        if g.deadzone > 0.0 && speed < g.deadzone {
            let k = speed / g.deadzone;
            h *= k;
            v *= k;
        }
        // Tiered smoothing: smooth slow motion, pass fast motion raw.
        if g.smoothing > 0.0 {
            let lo = g.smoothing * 6.0;
            let hi = lo * 2.0 + 1.0;
            let blend = ((speed - lo) / (hi - lo)).clamp(0.0, 1.0);
            let a = 0.3;
            self.smooth[0] += (h - self.smooth[0]) * a;
            self.smooth[1] += (v - self.smooth[1]) * a;
            h = h * blend + self.smooth[0] * (1.0 - blend);
            v = v * blend + self.smooth[1] * (1.0 - blend);
        }
        let gain = 1.0 + g.acceleration * ((speed - 40.0) / 300.0).clamp(0.0, 1.0) * 2.0;
        let sens = g.sensitivity * gain;
        let px = h * dt * sens + self.rem[0];
        let py = v * dt * sens * g.vertical_ratio + self.rem[1];
        let ix = px.trunc();
        let iy = py.trunc();
        self.rem = [px - ix, py - iy];
        sim::mouse_move(ix as i32, iy as i32);
    }
}

// ------------------------------------------------------------------ touchpad

/// Cursor movement waits this long after a finger lands, so a landing
/// finger doesn't jolt the cursor.
const TOUCH_SETTLE: Duration = Duration::from_millis(60);
/// Cursor pixels per touchpad unit at speed 1 (the pad is 1920 units wide).
const TOUCH_GAIN: f32 = 0.9;
/// Wheel units (120 per notch) per touchpad unit of two-finger travel: a
/// notch per 30 units, so a swipe down the whole pad is 36 notches.
const SCROLL_GAIN: f32 = 4.0;
/// A touch shorter than this, that moved less than `TAP_TRAVEL`, is a tap.
const TAP_TIME: Duration = Duration::from_millis(220);
const TAP_TRAVEL: f32 = 45.0;
/// A finger rolls when it pushes the pad down or lets it up. Movement this
/// soon after a press or release is held back, and dropped unless it grows
/// past `TAP_TRAVEL` into a real drag.
const CLICK_STILL: Duration = Duration::from_millis(150);

/// Cursor movement held back while it might still be a tap or a click's
/// finger roll: applied once it is clearly deliberate, dropped otherwise.
struct Held {
    until: Instant,
    /// Apply what was held when time runs out (a slow, deliberate move),
    /// instead of dropping it (the roll of a click).
    keep_on_expiry: bool,
    px: [f32; 2],
    travel: f32,
}

struct Touch {
    prev: [TouchPoint; 2],
    down_at: Option<Instant>,
    settle_until: Option<Instant>,
    held: Option<Held>,
    fingers: u8,
    travel: f32,
    max_fingers: u8,
    clicked: bool,
    rem: [f32; 2],
    scroll_rem: [f32; 2],
    click_button: Option<MouseButton>,
    pending_up: Option<(MouseButton, Instant)>,
}

impl Touch {
    fn new() -> Self {
        Touch {
            prev: [TouchPoint::default(); 2],
            down_at: None,
            settle_until: None,
            held: None,
            fingers: 0,
            travel: 0.0,
            max_fingers: 0,
            clicked: false,
            rem: [0.0; 2],
            scroll_rem: [0.0; 2],
            click_button: None,
            pending_up: None,
        }
    }

    fn release(&mut self) {
        if let Some(b) = self.click_button.take() {
            sim::mouse_button(b, false);
        }
        if let Some((b, _)) = self.pending_up.take() {
            sim::mouse_button(b, false);
        }
        self.down_at = None;
        self.held = None;
        self.fingers = 0;
    }

    fn hold(&mut self, until: Instant, keep_on_expiry: bool) {
        let h = self.held.get_or_insert(Held {
            until,
            keep_on_expiry,
            px: [0.0; 2],
            travel: 0.0,
        });
        h.until = h.until.max(until);
        h.keep_on_expiry &= keep_on_expiry;
    }

    /// Move the cursor by `px`, carrying sub-pixel remainders.
    fn move_cursor(&mut self, px: [f32; 2]) {
        let x = px[0] + self.rem[0];
        let y = px[1] + self.rem[1];
        let (ix, iy) = (x.trunc(), y.trunc());
        self.rem = [x - ix, y - iy];
        if ix != 0.0 || iy != 0.0 {
            sim::mouse_move(ix as i32, iy as i32);
        }
    }

    fn process(
        &mut self,
        t: &crate::profile::Touchpad,
        s: &InputState,
        prev: &InputState,
        now: Instant,
    ) {
        if let Some((b, at)) = self.pending_up {
            if now >= at {
                sim::mouse_button(b, false);
                self.pending_up = None;
            }
        }
        let cur = s.touch;
        let fingers = cur[0].active as u8 + cur[1].active as u8;

        // Contact tracking for taps. Until it's clearly not a tap, the
        // cursor stays put.
        if fingers > 0 && self.down_at.is_none() {
            self.down_at = Some(now);
            self.travel = 0.0;
            self.max_fingers = 0;
            self.clicked = false;
            self.held = None;
            self.hold(now + TAP_TIME, true);
        }
        self.max_fingers = self.max_fingers.max(fingers);
        // A finger landing or lifting restarts the settle time, so the
        // cursor doesn't jump when a second finger joins or a scroll ends.
        if fingers != self.fingers {
            self.settle_until = (fingers > 0).then(|| now + TOUCH_SETTLE);
            self.rem = [0.0; 2];
            self.fingers = fingers;
        }
        let settled = self.settle_until.is_none_or(|t| now >= t);

        // Physical click: decided by the fingers on the pad as it goes down.
        // Two fingers is a right click (and so is the right half, if set);
        // the button stays held while the pad is, so a press-and-slide drags.
        let click = s.buttons.has(Button::Touchpad);
        let was = prev.buttons.has(Button::Touchpad);
        if t.click_to_click && click != was {
            self.hold(now + CLICK_STILL, false);
            if click {
                let two = fingers >= 2 || self.prev.iter().all(|f| f.active);
                let x = cur
                    .iter()
                    .chain(self.prev.iter())
                    .find(|f| f.active)
                    .map(|f| f.x)
                    .unwrap_or(0);
                let b = if two || (t.right_half_right_click && x >= 960) {
                    MouseButton::Right
                } else {
                    MouseButton::Left
                };
                sim::mouse_button(b, true);
                self.click_button = Some(b);
                self.clicked = true;
            } else if let Some(b) = self.click_button.take() {
                sim::mouse_button(b, false);
            }
        }

        // Fingers that were down in the last report too, in either slot.
        let moved = |k: usize| -> Option<(f32, f32)> {
            let (c, p) = (cur[k], self.prev[k]);
            (c.active && p.active && c.id == p.id)
                .then(|| (c.x as f32 - p.x as f32, c.y as f32 - p.y as f32))
        };
        match (moved(0), moved(1)) {
            (Some((dx0, dy0)), Some((dx1, dy1))) => {
                self.travel += (dx0 * dx0 + dy0 * dy0).sqrt();
                if settled && t.two_finger_scroll {
                    let (mx, my) = ((dx0 + dx1) / 2.0, (dy0 + dy1) / 2.0);
                    // Fingers up scroll up, like a wheel turned away;
                    // natural scrolling moves the page with the fingers.
                    let dir = if t.natural_scroll { -1.0 } else { 1.0 };
                    let g = SCROLL_GAIN * t.scroll_speed * dir;
                    self.scroll_rem[0] += -my * g;
                    self.scroll_rem[1] += mx * g;
                    for (i, horiz) in [(0usize, false), (1, true)] {
                        let w = self.scroll_rem[i].trunc();
                        if w != 0.0 {
                            sim::wheel(w as i32, horiz);
                            self.scroll_rem[i] -= w;
                        }
                    }
                }
            }
            (Some((dx, dy)), None) | (None, Some((dx, dy))) if fingers == 1 => {
                let d = (dx * dx + dy * dy).sqrt();
                self.travel += d;
                if settled {
                    let gain = t.sensitivity
                        * TOUCH_GAIN
                        * (1.0 + t.acceleration * (d / 25.0).min(1.5));
                    let px = [dx * gain, dy * gain];
                    match self.held.as_mut() {
                        Some(h) => {
                            h.px[0] += px[0];
                            h.px[1] += px[1];
                            h.travel += d;
                            if h.travel > TAP_TRAVEL {
                                // Clearly a swipe or a drag: catch up.
                                let px = h.px;
                                self.held = None;
                                self.move_cursor(px);
                            }
                        }
                        None => self.move_cursor(px),
                    }
                }
            }
            _ => self.rem = [0.0; 2],
        }
        if let Some(h) = &self.held {
            if now >= h.until {
                let (keep, px) = (h.keep_on_expiry, h.px);
                self.held = None;
                if keep && fingers == 1 {
                    self.move_cursor(px);
                }
            }
        }

        // Lift: whatever was held back was a tap or a click's roll. A quick
        // light touch is a tap: one finger left click, two right click.
        if fingers == 0 {
            self.held = None;
            if let Some(at) = self.down_at.take() {
                let dur = now.duration_since(at);
                if t.tap_to_click && !self.clicked && dur < TAP_TIME && self.travel < TAP_TRAVEL {
                    let b = if self.max_fingers >= 2 {
                        MouseButton::Right
                    } else {
                        MouseButton::Left
                    };
                    sim::mouse_button(b, true);
                    self.pending_up = Some((b, now + Duration::from_millis(25)));
                }
            }
        }
        self.prev = cur;
    }
}

// ------------------------------------------------------------------ sticks

#[derive(Default)]
struct Stick {
    rem: [f32; 2],
    keys: [u16; 4],
}

impl Stick {
    fn release(&mut self) {
        for k in self.keys.iter_mut() {
            if *k != 0 {
                sim::key(*k, false);
                *k = 0;
            }
        }
    }

    fn process(&mut self, st: &StickSettings, mode: StickMode, x: u8, y: u8, dt: f32) {
        let fx = ((x as f32 - 128.0) / 127.0).clamp(-1.0, 1.0);
        let fy = ((y as f32 - 128.0) / 127.0).clamp(-1.0, 1.0);
        let mag = (fx * fx + fy * fy).sqrt().min(1.0);
        let dz = st.deadzone.clamp(0.0, 0.9);
        let (nx, ny, m) = if mag <= dz || mag == 0.0 {
            (0.0, 0.0, 0.0)
        } else {
            let m = ((mag - dz) / (1.0 - dz)).powf(st.curve.max(0.2));
            (fx / mag, fy / mag, m)
        };
        match mode {
            StickMode::Passthrough => self.release(),
            StickMode::Mouse => {
                self.release();
                let px = nx * m * st.mouse_speed * dt + self.rem[0];
                let py = ny * m * st.mouse_speed * dt + self.rem[1];
                let (ix, iy) = (px.trunc(), py.trunc());
                self.rem = if m == 0.0 {
                    [0.0; 2]
                } else {
                    [px - ix, py - iy]
                };
                sim::mouse_move(ix as i32, iy as i32);
            }
            StickMode::Scroll => {
                self.release();
                let v = -ny * m * st.scroll_speed * 1500.0 * dt + self.rem[0];
                let h = nx * m * st.scroll_speed * 1500.0 * dt + self.rem[1];
                let (iv, ih) = (v.trunc(), h.trunc());
                self.rem = if m == 0.0 { [0.0; 2] } else { [v - iv, h - ih] };
                sim::wheel(iv as i32, false);
                sim::wheel(ih as i32, true);
            }
            StickMode::Wasd | StickMode::Arrows => {
                let keys: [u16; 4] = if mode == StickMode::Wasd {
                    [0x57, 0x53, 0x41, 0x44]
                } else {
                    [0x26, 0x28, 0x25, 0x27]
                };
                let thr = st.key_threshold.clamp(0.1, 0.95);
                let want = [fy < -thr, fy > thr, fx < -thr, fx > thr];
                for i in 0..4 {
                    let held = self.keys[i] != 0;
                    if want[i] && !held {
                        sim::key(keys[i], true);
                        self.keys[i] = keys[i];
                    } else if !want[i] && held {
                        sim::key(self.keys[i], false);
                        self.keys[i] = 0;
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ mappings

#[derive(Default, Clone)]
struct MapRt {
    out_down: bool,
    toggled: bool,
    press_at: Option<Instant>,
    long_fired: bool,
    active: bool,
    last_press: Option<Instant>,
    turbo: bool,
    turbo_next: Option<Instant>,
    tap_up_at: Option<Instant>,
    /// Buttons a muted combo hid; each stays hidden until it is released.
    mute_latch: u32,
}

fn holdable(a: &Action) -> bool {
    matches!(
        a,
        Action::Key { .. }
            | Action::Mouse { .. }
            | Action::Media { .. }
            | Action::Gamepad { .. }
            | Action::Xbox { .. }
    )
}

fn action_down(engine: &Engine, device: &Device, a: &Action) {
    match a {
        Action::None => {}
        Action::Key {
            vk,
            ctrl,
            shift,
            alt,
            win,
        } => {
            if *win {
                sim::key(0x5B, true);
            }
            if *ctrl {
                sim::key(0xA2, true);
            }
            if *alt {
                sim::key(0xA4, true);
            }
            if *shift {
                sim::key(0xA0, true);
            }
            sim::key(*vk, true);
        }
        Action::Mouse { button } => sim::mouse_button(*button, true),
        Action::ScrollUp => sim::wheel(120, false),
        Action::ScrollDown => sim::wheel(-120, false),
        Action::Media { key } => sim::key(key.vk(), true),
        Action::NextProfile => engine.cycle_profile(device, 1),
        Action::PreviousProfile => engine.cycle_profile(device, -1),
        Action::ToggleGyroMouse => {
            let mut t = device.toggles.lock();
            let on = !t.gyro_toggled_on;
            t.gyro_toggled_on = on;
            t.gyro = Some(on);
            drop(t);
            engine.toast(if on {
                crate::i18n::tr("Gyro mouse on")
            } else {
                crate::i18n::tr("Gyro mouse off")
            });
        }
        Action::ToggleTouchpadMouse => {
            let live = device.live.lock().touch_mouse_active;
            device.toggles.lock().touchpad = Some(!live);
            engine.toast(if live {
                crate::i18n::tr("Touchpad mouse off")
            } else {
                crate::i18n::tr("Touchpad mouse on")
            });
        }
        Action::Profile { name } => {
            if engine.profiles.exists(name) {
                engine.set_device_profile(device, name);
                engine.toast(crate::localized_format!("Profile: {name}", "Perfil: {name}", "Perfil: {name}"));
                device.identify_light();
            }
        }
        // Read from the mapping state when the pad is built.
        Action::Gamepad { .. } | Action::Xbox { .. } => {}
        // Started by the mapping, which knows the buttons to watch.
        Action::HoldProfile { .. } => {}
        Action::TurnOffController => turn_off_controller(engine, device),
        Action::Run { path } => {
            if !path.trim().is_empty() {
                let p = path.trim().to_string();
                std::thread::spawn(move || {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", &p])
                        .spawn();
                });
            }
        }
    }
}

fn action_up(a: &Action) {
    match a {
        Action::Key {
            vk,
            ctrl,
            shift,
            alt,
            win,
        } => {
            sim::key(*vk, false);
            if *shift {
                sim::key(0xA0, false);
            }
            if *alt {
                sim::key(0xA4, false);
            }
            if *ctrl {
                sim::key(0xA2, false);
            }
            if *win {
                sim::key(0x5B, false);
            }
        }
        Action::Mouse { button } => sim::mouse_button(*button, false),
        Action::Media { key } => sim::key(key.vk(), false),
        _ => {}
    }
}

/// Use the mapping's profile while its buttons stay down.
fn hold_profile(engine: &Engine, device: &Device, m: &Mapping, name: &str, then: &str) {
    let mut buttons = 0u32;
    let mut hide = 0u32;
    for b in m.buttons() {
        buttons |= b.bit();
        if m.mute {
            hide |= b.bit();
            if b.is_touchpad_zone() {
                hide |= Button::Touchpad.bit();
            }
        }
    }
    let then = then.trim();
    engine.hold_profile(
        device,
        crate::engine::HeldProfile {
            name: name.to_string(),
            buttons,
            hide,
            then: (!then.is_empty()).then(|| then.to_string()),
        },
    );
}

/// Power the controller off by dropping its Bluetooth link. The mapping
/// fires this once per gesture, the same way a profile switch fires once.
fn turn_off_controller(engine: &Engine, device: &Device) {
    if !device.is_bluetooth() {
        engine.warn_toast(crate::i18n::tr("Turn off works over Bluetooth"));
        return;
    }
    let mac = device.mac.lock().clone();
    if crate::platform::bluetooth_address(&mac).is_none() {
        engine.warn_toast(crate::i18n::tr("No Bluetooth address, so the controller stays on"));
        return;
    }
    log::info!("turning the controller off");
    device.flight.expect_off.store(true, Ordering::Release);
    engine.toast(crate::i18n::tr("Turning the controller off"));
    // On the input thread, like idle shutoff: the radio call is short, and
    // the controller is going away anyway.
    if !disconnect_bluetooth(mac) {
        log::warn!("Bluetooth disconnect failed");
        device.flight.expect_off.store(false, Ordering::Release);
        engine.warn_toast(crate::i18n::tr("Couldn't turn the controller off"));
    }
}

fn disconnect_bluetooth(mac: String) -> bool {
    // Tests record the address. A real one would talk to the Bluetooth radio.
    #[cfg(test)]
    {
        TURN_OFFS.lock().push(mac);
        true
    }
    #[cfg(not(test))]
    {
        crate::platform::bt_disconnect(&mac)
    }
}

#[cfg(test)]
static TURN_OFFS: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());

// ------------------------------------------------------------------ pipeline

pub struct Pipeline {
    cache: ProfileCache,
    /// The profile the mapping state was built for.
    built_for: Option<Arc<Profile>>,
    gyro: Gyro,
    touch: Touch,
    sticks: [Stick; 2],
    maps: Vec<MapRt>,
    map_actions: Vec<Action>,
    /// Mapping indexes, combos first, so a combo that completes wins.
    map_order: Vec<usize>,
    gyro_toggle: bool,
    last_at: Option<Instant>,
    gyro_was_active: bool,
    /// Buttons last pressed on the helper Xbox pad.
    helper_sent: u32,
    /// The parked Xbox pad had the whole pad, for Game Bar.
    to_gamebar: bool,
    /// Touchpad zone held on the previous report, if the click was placed.
    touch_zone: Option<Button>,
    /// Buttons a held profile hid from the game; each stays hidden until it
    /// is let go, also after the hold ends, like a muted combo.
    held_hide: u32,
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline {
            cache: ProfileCache::default(),
            built_for: None,
            gyro: Gyro::new(),
            touch: Touch::new(),
            sticks: [Stick::default(), Stick::default()],
            maps: Vec::new(),
            map_actions: Vec::new(),
            map_order: Vec::new(),
            gyro_toggle: false,
            last_at: None,
            gyro_was_active: false,
            helper_sent: 0,
            to_gamebar: false,
            touch_zone: None,
            held_hide: 0,
        }
    }

    /// Set Touchpad Left / Center / Right from the finger on the pad.
    ///
    /// The plain Touchpad button is left set, so a mapping on it still sees
    /// every click. The zone is remembered so the next report can tell a
    /// hold from a new press. A click that never had a finger is not given a
    /// zone; once it has one, that zone stays until the pad comes up or a
    /// finger reports a different third.
    fn apply_touchpad_zones(&mut self, s: &mut InputState, prev: &mut InputState) {
        let fresh = if s.buttons.has(Button::Touchpad) {
            ds_proto::input::touchpad_click_zone(&s.touch, &prev.touch)
        } else {
            None
        };
        let zone = if s.buttons.has(Button::Touchpad) {
            fresh.or(self.touch_zone)
        } else {
            None
        };
        if let Some(z) = zone {
            s.buttons.set(z, true);
        }
        if let Some(z) = self.touch_zone {
            prev.buttons.set(z, true);
        }
        self.touch_zone = zone;
    }

    pub fn release_all(&mut self) {
        self.touch.release();
        for s in self.sticks.iter_mut() {
            s.release();
        }
        for (rt, a) in self.maps.iter_mut().zip(self.map_actions.iter()) {
            if rt.out_down {
                action_up(a);
                rt.out_down = false;
            }
        }
    }

    pub fn process(
        &mut self,
        engine: &Arc<Engine>,
        device: &Arc<Device>,
        s: &InputState,
        prev: &InputState,
        raw: Option<&[u8; ds_proto::virtual_pad::DS_USB_INPUT_LEN]>,
        now: Instant,
    ) {
        let (mut s, mut prev) = (*s, *prev);
        self.apply_touchpad_zones(&mut s, &mut prev);

        // A held profile ends as soon as one of its buttons comes up, before
        // this report picks its profile, so the release already uses the
        // profile from before the hold.
        let held = device.held_profile.lock().as_ref().map(|h| h.buttons);
        if held.is_some_and(|b| s.buttons.0 & b != b) {
            engine.release_held_profile(device);
        }

        let profile = self.cache.get(engine, device);
        let changed = match &self.built_for {
            Some(b) if Arc::ptr_eq(b, &profile) => false,
            // Another profile, or an edit: rebuild only if the mappings
            // differ, so dragging a lightbar slider doesn't let go of a key
            // a mapping is holding or reset a toggle.
            Some(b) => b.mappings != profile.mappings,
            None => true,
        };
        self.built_for = Some(profile.clone());
        if changed {
            // Mappings changed: drop anything still held by the old ones.
            self.release_all();
            self.maps = vec![MapRt::default(); profile.mappings.len()];
            self.map_actions = profile.mappings.iter().map(|m| m.action.clone()).collect();
            let combos = (0..profile.mappings.len()).filter(|&i| profile.mappings[i].is_combo());
            let solos = (0..profile.mappings.len()).filter(|&i| !profile.mappings[i].is_combo());
            self.map_order = combos.chain(solos).collect();
        }

        let dt = self
            .last_at
            .map(|t| now.duration_since(t).as_secs_f32())
            .unwrap_or(0.004)
            .clamp(0.001, 0.03);
        self.last_at = Some(now);

        let toggles = *device.toggles.lock();

        // Gyro
        if device.calibrate_request.swap(false, Ordering::AcqRel) {
            self.gyro.manual_until = Some(now + Duration::from_millis(2000));
            self.gyro.reset_still();
        }
        let mut dps = [0.0; 3];
        let mut gyro_active = false;
        if s.full && s.motion_valid {
            let gdt = self.gyro.dt(&s, now);
            dps = self.gyro.update_bias(&s, now);
            let mode = toggles
                .gyro
                .map(|on| if on { GyroMode::Mouse } else { GyroMode::Off })
                .unwrap_or(profile.gyro.mode);
            if mode == GyroMode::Mouse {
                gyro_active = match profile.gyro.activation {
                    GyroActivation::Always => true,
                    GyroActivation::WhileHeld(b) => s.buttons.has(b),
                    GyroActivation::WhileNotHeld(b) => !s.buttons.has(b),
                    GyroActivation::WhileTouched(z) => ds_proto::input::touch_zone_held(&s.touch, z),
                    GyroActivation::Toggle(b) => {
                        if s.buttons.has(b) && !prev.buttons.has(b) {
                            self.gyro_toggle = !self.gyro_toggle;
                        }
                        self.gyro_toggle
                    }
                };
                // An explicit toggle-on from a mapping means "always".
                if toggles.gyro == Some(true) {
                    gyro_active = true;
                }
                if self.gyro.manual_until.is_some() {
                    gyro_active = false;
                }
                if gyro_active {
                    if !self.gyro_was_active {
                        self.gyro.rem = [0.0; 2];
                        self.gyro.smooth = [0.0; 2];
                    }
                    self.gyro.mouse(&profile.gyro, dps, gdt);
                }
            }
        }
        self.gyro_was_active = gyro_active;

        // Touchpad
        let touch_mode = toggles
            .touchpad
            .map(|on| {
                if on {
                    TouchMode::Mouse
                } else {
                    TouchMode::Passthrough
                }
            })
            .unwrap_or(profile.touchpad.mode);
        let touch_active = touch_mode == TouchMode::Mouse && s.full;
        if touch_active {
            self.touch.process(&profile.touchpad, &s, &prev, now);
        } else {
            self.touch.release();
            self.touch.prev = s.touch;
        }

        // Sticks
        let lm = profile.sticks.left.mode;
        let rm = profile.sticks.right.mode;
        self.sticks[0].process(&profile.sticks.left, lm, s.lx, s.ly, dt);
        self.sticks[1].process(&profile.sticks.right, rm, s.rx, s.ry, dt);

        // Mute toggle
        if s.buttons.has(Button::Mute)
            && !prev.buttons.has(Button::Mute)
            && (profile.lighting.mute_led == MuteLed::Toggle
                || (device.microphone.enabled() && profile.microphone.mute_button))
        {
            let mut t = device.toggles.lock();
            t.mute_engaged = !t.mute_engaged;
            drop(t);
            // Gate capture immediately: the idle media clock can otherwise
            // sleep for 100 ms after the Mute LED has already changed.
            if device.microphone.enabled() {
                let pad_profile = engine.unheld_profile(device);
                device.apply_microphone(&pad_profile.as_deref().unwrap_or(&profile).microphone);
            }
            device.kick();
        }

        // Classic-rumble click feedback when PCM haptics are not available.
        if profile.haptics.buttons.enabled && !device.pcm_mode.load(Ordering::Acquire) {
            let pressed = s.buttons.pressed_since(prev.buttons);
            // Zone clicks share the Touchpad press; a finger sliding to
            // another third is not a new click.
            let pressed = ds_proto::Buttons(
                pressed.0 & !(Button::L2.bit() | Button::R2.bit() | Button::TOUCHPAD_ZONE_BITS),
            );
            if !pressed.is_empty() {
                let amp = (profile.haptics.buttons.intensity.clamp(0.0, 1.0) * 255.0) as u8;
                device.test_rumble(0, amp, profile.haptics.buttons.duration_ms.max(30.0) as u64);
            }
        }

        // Mappings
        let (mut suppress, inject, helper) = self.mappings(engine, device, &profile, &s, &prev, now);
        let held_hide = device.held_profile.lock().as_ref().map_or(0, |h| h.hide);
        self.held_hide = (self.held_hide | held_hide) & s.buttons.0;
        suppress |= self.held_hide;

        // Game Bar reads only Xbox pads. While its overlay is open, the
        // parked Xbox pad (which games can't see) gets the whole pad, and
        // the game gets an idle one, so a press meant for Game Bar does
        // nothing in the game and Game Bar never sees it twice, should it
        // read DualSense pads again.
        let active = device.virt.active();
        let to_gamebar = active != Some(VirtualKind::Xbox360)
            && device.virt.has_parked()
            && crate::virt::gamebar::open();

        // Virtual controller
        if active.is_some() || to_gamebar {
            let pad = virtual_pad(&profile, &s, suppress, inject, lm, rm, touch_mode);
            if active.is_some() {
                let touch = touch_mode != TouchMode::Mouse || profile.virtual_out.touch_passthrough;
                if to_gamebar {
                    device.virt.submit(&Default::default(), still(raw).as_ref(), false);
                } else {
                    device.virt.submit(&pad, raw, touch);
                }
            }
            if to_gamebar {
                let mut p = pad;
                let h = crate::virt::companion::pad(helper);
                p.buttons.0 |= h.buttons.0;
                p.l2 = p.l2.max(h.l2);
                p.r2 = p.r2.max(h.r2);
                device.virt.submit_parked(&p);
            }
        }
        // Otherwise Xbox buttons go to the parked Xbox pad, else to the
        // helper pad; once after Game Bar closes, to let go of the rest.
        if !to_gamebar
            && (helper != 0 || self.helper_sent != 0 || self.to_gamebar)
            && !device.virt.submit_parked(&crate::virt::companion::pad(helper))
        {
            engine.companion.set(device.id, helper);
        }
        self.helper_sent = helper;
        self.to_gamebar = to_gamebar;

        engine.dsu.publish(device, &s, dps);

        // Publish for the UI and the haptics thread.
        let mut live = device.live.lock();
        live.input = s;
        live.gyro_dps = dps;
        live.gyro_bias = [
            self.gyro.bias[0] / COUNTS_PER_DPS,
            self.gyro.bias[1] / COUNTS_PER_DPS,
            self.gyro.bias[2] / COUNTS_PER_DPS,
        ];
        live.calibrating = self.gyro.manual_until.is_some();
        live.gyro_active = gyro_active;
        live.touch_mouse_active = touch_active;
    }

    /// Run the mappings. Returns the buttons to hide from the virtual
    /// controller, the virtual buttons mappings are pressing, and the Xbox
    /// buttons they press with no virtual Xbox pad of this controller's own.
    fn mappings(
        &mut self,
        engine: &Engine,
        device: &Device,
        profile: &Profile,
        s: &InputState,
        prev: &InputState,
        now: Instant,
    ) -> (u32, u32, u32) {
        let mut suppress = 0u32;
        // Buttons inside a combo that is held right now: their solo long
        // presses and double taps don't fire.
        let mut in_combo = 0u32;
        for m in profile
            .mappings
            .iter()
            .filter(|m| m.enabled && m.is_combo())
        {
            if m.buttons().all(|b| s.buttons.has(b)) {
                for b in m.buttons() {
                    in_combo |= b.bit();
                }
            }
        }
        for k in 0..self.map_order.len() {
            let i = self.map_order[k];
            let Some(m) = profile.mappings.get(i) else {
                continue;
            };
            let Some(rt) = self.maps.get_mut(i) else {
                continue;
            };
            if !m.enabled {
                continue;
            }
            let combo = m.is_combo();
            let down = m.buttons().all(|b| s.buttons.has(b));
            let was = m.buttons().all(|b| prev.buttons.has(b));
            let pressed = down && !was;
            let released = !down && was;

            if m.mute {
                if !combo {
                    if m.button.is_touchpad_zone() {
                        // The game has one touchpad button. Hiding a zone
                        // hides that click while the finger is in the zone.
                        if down {
                            suppress |= Button::Touchpad.bit();
                        }
                    } else {
                        suppress |= m.button.bit();
                    }
                } else if down {
                    for b in m.buttons() {
                        rt.mute_latch |= b.bit();
                        if b.is_touchpad_zone() {
                            rt.mute_latch |= Button::Touchpad.bit();
                        }
                    }
                }
            }
            if combo {
                rt.mute_latch &= s.buttons.0;
                suppress |= rt.mute_latch;
            }
            if matches!(m.action, Action::None) {
                continue;
            }
            if !combo && in_combo & m.button.bit() != 0 {
                // Held as part of a combo: no long press for the solo.
                rt.long_fired = true;
                rt.last_press = None;
            }

            // Pending tap release.
            if let Some(at) = rt.tap_up_at {
                if now >= at {
                    action_up(&m.action);
                    rt.out_down = false;
                    rt.tap_up_at = None;
                }
            }

            if pressed {
                rt.press_at = Some(now);
                rt.long_fired = false;
            }
            let (start, end) = match m.gesture {
                Gesture::Press => (pressed, released),
                Gesture::LongPress => {
                    let mut start = false;
                    if down && !rt.long_fired {
                        if let Some(t) = rt.press_at {
                            if now.duration_since(t)
                                >= Duration::from_millis(m.long_ms.clamp(100, 5000) as u64)
                            {
                                rt.long_fired = true;
                                start = true;
                            }
                        }
                    }
                    (start, released && rt.active)
                }
                Gesture::DoubleTap => {
                    let mut start = false;
                    if pressed {
                        let win = Duration::from_millis(m.double_ms.clamp(100, 1000) as u64);
                        if rt
                            .last_press
                            .map(|t| now.duration_since(t) <= win)
                            .unwrap_or(false)
                        {
                            start = true;
                            rt.last_press = None;
                        } else {
                            rt.last_press = Some(now);
                        }
                    }
                    (start, released && rt.active)
                }
            };

            if start {
                rt.active = true;
                if let Action::HoldProfile { name, then } = &m.action {
                    // Held only as long as the buttons are: the behavior
                    // setting doesn't apply.
                    hold_profile(engine, device, m, name, then);
                    continue;
                }
                match m.mode {
                    HoldStyle::Hold => {
                        action_down(engine, device, &m.action);
                        rt.out_down = holdable(&m.action);
                    }
                    HoldStyle::Tap => {
                        action_down(engine, device, &m.action);
                        if holdable(&m.action) {
                            rt.out_down = true;
                            rt.tap_up_at = Some(now + Duration::from_millis(30));
                        }
                    }
                    HoldStyle::Turbo => {
                        rt.turbo = true;
                        rt.turbo_next = Some(now);
                    }
                    HoldStyle::Toggle => {
                        if rt.toggled {
                            action_up(&m.action);
                            rt.out_down = false;
                            rt.toggled = false;
                        } else {
                            action_down(engine, device, &m.action);
                            rt.out_down = holdable(&m.action);
                            rt.toggled = true;
                        }
                    }
                }
            }
            if rt.turbo {
                if let Some(next) = rt.turbo_next {
                    if now >= next {
                        let half = Duration::from_millis((m.turbo_ms.clamp(20, 2000) / 2) as u64);
                        if rt.out_down {
                            action_up(&m.action);
                            rt.out_down = false;
                        } else {
                            action_down(engine, device, &m.action);
                            rt.out_down = holdable(&m.action);
                        }
                        rt.turbo_next = Some(now + half);
                    }
                }
            }
            if end {
                rt.active = false;
                match m.mode {
                    HoldStyle::Hold | HoldStyle::Turbo => {
                        rt.turbo = false;
                        rt.turbo_next = None;
                        if rt.out_down {
                            action_up(&m.action);
                            rt.out_down = false;
                        }
                    }
                    HoldStyle::Tap | HoldStyle::Toggle => {}
                }
            }
        }
        let mut inject = 0u32;
        let mut helper = 0u32;
        // Xbox buttons go to the virtual Xbox pad, else to the parked Xbox
        // pad or the helper pad (see `process`).
        let own_xbox = device.virt.active() == Some(VirtualKind::Xbox360);
        for (m, rt) in profile.mappings.iter().zip(self.maps.iter()) {
            if !m.enabled || !rt.out_down {
                continue;
            }
            match m.action {
                Action::Gamepad { button } => inject |= button.bit(),
                Action::Xbox { button } if own_xbox => inject |= button.pad_button().bit(),
                Action::Xbox { button } => helper |= button.pad_button().bit(),
                _ => {}
            }
        }
        (suppress, inject, helper)
    }
}

/// The physical report with the gyro still, for an idle virtual pad: the
/// accelerometer stays, so the game keeps its sense of which way is down.
fn still(
    raw: Option<&[u8; ds_proto::virtual_pad::DS_USB_INPUT_LEN]>,
) -> Option<[u8; ds_proto::virtual_pad::DS_USB_INPUT_LEN]> {
    raw.map(|r| {
        let mut r = *r;
        r[16..22].fill(0);
        r
    })
}

/// The pad the game sees: mappings and mutes applied, sticks that drive the
/// mouse centered, then inversion, deadzones, and trigger ranges.
fn virtual_pad(
    profile: &Profile,
    s: &InputState,
    suppress: u32,
    inject: u32,
    left_mode: StickMode,
    right_mode: StickMode,
    touch_mode: TouchMode,
) -> ds_proto::virtual_pad::PadState {
    use crate::virt::shape;
    let vo = &profile.virtual_out;
    let mut b = ds_proto::Buttons((s.buttons.0 & !suppress) | inject);
    if touch_mode == TouchMode::Mouse && profile.touchpad.click_to_click && !vo.touch_passthrough {
        b.set(Button::Touchpad, false);
    }
    // Zone clicks are mapping inputs. The virtual pad still has one touchpad button.
    b.set(Button::TouchpadLeft, false);
    b.set(Button::TouchpadCenter, false);
    b.set(Button::TouchpadRight, false);
    let inv = |v: u8, on: bool| if on { 255 - v } else { v };
    let (lx, ly) = if left_mode == StickMode::Passthrough {
        shape::stick(
            &vo.left_deadzone,
            inv(s.lx, vo.invert[0]),
            inv(s.ly, vo.invert[1]),
        )
    } else {
        (128, 128)
    };
    let (rx, ry) = if right_mode == StickMode::Passthrough {
        shape::stick(
            &vo.right_deadzone,
            inv(s.rx, vo.invert[2]),
            inv(s.ry, vo.invert[3]),
        )
    } else {
        (128, 128)
    };
    let trig = |v: u8, btn: Button, range: [u8; 2]| -> u8 {
        if inject & btn.bit() != 0 {
            255
        } else if suppress & btn.bit() != 0 {
            0
        } else {
            shape::trigger(v, range)
        }
    };
    let l2 = trig(s.l2, Button::L2, vo.left_trigger_range);
    let r2 = trig(s.r2, Button::R2, vo.right_trigger_range);
    b.set(Button::L2, l2 > 0);
    b.set(Button::R2, r2 > 0);
    ds_proto::virtual_pad::PadState {
        buttons: b,
        lx,
        ly,
        rx,
        ry,
        l2,
        r2,
    }
}

#[cfg(test)]
mod turn_off_tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use ds_proto::input::Button;
    use ds_proto::{Connection, InputState};

    use super::{Pipeline, TURN_OFFS};
    use crate::device::Device;
    use crate::engine::Engine;
    use crate::profile::{
        Action, Gesture, GyroMode, Mapping, Profile, StickMode, TouchMode, VirtualKind,
    };

    /// Tests that read `TURN_OFFS` run one at a time.
    pub(super) fn gate() -> parking_lot::MutexGuard<'static, ()> {
        static GATE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        GATE.lock()
    }

    pub(super) fn take_offs() -> Vec<String> {
        std::mem::take(&mut *TURN_OFFS.lock())
    }

    struct Rig {
        dir: PathBuf,
        engine: Arc<Engine>,
        device: Arc<Device>,
        pipe: Pipeline,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn rig(conn: Connection, mac: &str, mappings: Vec<Mapping>) -> Rig {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dsz-turn-off-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let engine = Engine::new(dir.clone());
        let device = Device::for_test(conn, mac);
        let mut profile = Profile::named("Turn off");
        profile.mappings = mappings;
        profile.gyro.mode = GyroMode::Off;
        profile.touchpad.mode = TouchMode::Passthrough;
        profile.sticks.left.mode = StickMode::Passthrough;
        profile.sticks.right.mode = StickMode::Passthrough;
        profile.virtual_out.kind = VirtualKind::Off;
        profile.haptics.buttons.enabled = false;
        let name = engine.profiles.add(profile);
        engine.set_device_profile(&device, &name);
        Rig {
            dir,
            engine,
            device,
            pipe: Pipeline::new(),
        }
    }

    fn centered(buttons: &[Button]) -> InputState {
        let mut s = InputState {
            lx: 128,
            ly: 128,
            rx: 128,
            ry: 128,
            ..InputState::default()
        };
        for b in buttons {
            s.buttons.set(*b, true);
        }
        s
    }

    fn step(rig: &mut Rig, prev: &InputState, now_s: &InputState, at: Instant) {
        rig.pipe
            .process(&rig.engine, &rig.device, now_s, prev, None, at);
    }

    fn off(button: Button) -> Mapping {
        Mapping {
            button,
            action: Action::TurnOffController,
            ..Mapping::default()
        }
    }

    #[test]
    fn fires_once_per_press_on_any_button() {
        let _g = gate();
        let _ = take_offs();
        let mut rig = rig(
            Connection::Bluetooth,
            "AA-BB-CC-DD-EE-FF",
            vec![off(Button::Options)],
        );
        let up = centered(&[]);
        let other = centered(&[Button::Cross]);
        let down = centered(&[Button::Options]);
        let t = Instant::now();
        step(&mut rig, &up, &other, t);
        assert!(take_offs().is_empty());
        step(&mut rig, &up, &down, t + Duration::from_millis(10));
        assert_eq!(take_offs(), vec!["AA-BB-CC-DD-EE-FF".to_string()]);
        // Held: still one request. Release, then a second press, asks again.
        step(&mut rig, &down, &down, t + Duration::from_millis(40));
        assert!(take_offs().is_empty());
        step(&mut rig, &down, &up, t + Duration::from_millis(80));
        step(&mut rig, &up, &down, t + Duration::from_millis(120));
        assert_eq!(take_offs(), vec!["AA-BB-CC-DD-EE-FF".to_string()]);
        let toasts: Vec<_> = rig
            .engine
            .toasts
            .lock()
            .iter()
            .map(|toast| (toast.warn, toast.text.clone()))
            .collect();
        assert_eq!(
            toasts
                .iter()
                .filter(|(_, text)| text == "Turning the controller off")
                .count(),
            2
        );
        assert!(toasts.iter().all(|(warn, _)| !*warn));
    }

    #[test]
    fn combo_waits_for_every_button() {
        let _g = gate();
        let _ = take_offs();
        let mut rig = rig(
            Connection::Bluetooth,
            "11-22-33-44-55-66",
            vec![Mapping {
                button: Button::Square,
                with: vec![Button::Create],
                action: Action::TurnOffController,
                ..Mapping::default()
            }],
        );
        let up = centered(&[]);
        let solo = centered(&[Button::Square]);
        let combo = centered(&[Button::Create, Button::Square]);
        let t = Instant::now();
        step(&mut rig, &up, &solo, t);
        assert!(take_offs().is_empty());
        step(&mut rig, &solo, &combo, t + Duration::from_millis(10));
        assert_eq!(take_offs(), vec!["11-22-33-44-55-66".to_string()]);
    }

    #[test]
    fn long_press_waits_out_the_hold() {
        let _g = gate();
        let _ = take_offs();
        let mut rig = rig(
            Connection::Bluetooth,
            "AA-BB-CC-DD-EE-01",
            vec![Mapping {
                button: Button::Ps,
                gesture: Gesture::LongPress,
                long_ms: 400,
                action: Action::TurnOffController,
                ..Mapping::default()
            }],
        );
        let up = centered(&[]);
        let down = centered(&[Button::Ps]);
        let t = Instant::now();
        step(&mut rig, &up, &down, t);
        step(&mut rig, &down, &down, t + Duration::from_millis(399));
        assert!(take_offs().is_empty());
        step(&mut rig, &down, &down, t + Duration::from_millis(400));
        assert_eq!(take_offs(), vec!["AA-BB-CC-DD-EE-01".to_string()]);
        step(&mut rig, &down, &down, t + Duration::from_millis(800));
        assert!(take_offs().is_empty());
    }

    #[test]
    fn disabled_mapping_does_not_fire() {
        let _g = gate();
        let _ = take_offs();
        let mut rig = rig(
            Connection::Bluetooth,
            "AA-BB-CC-DD-EE-02",
            vec![Mapping {
                enabled: false,
                ..off(Button::Cross)
            }],
        );
        let up = centered(&[]);
        let down = centered(&[Button::Cross]);
        step(&mut rig, &up, &down, Instant::now());
        assert!(take_offs().is_empty());
        assert!(rig.engine.toasts.lock().is_empty());
    }

    #[test]
    fn usb_and_a_missing_address_do_not_disconnect() {
        let _g = gate();
        let _ = take_offs();
        let mut usb = rig(
            Connection::Usb,
            "AA-BB-CC-DD-EE-FF",
            vec![off(Button::Mute)],
        );
        let up = centered(&[]);
        let down = centered(&[Button::Mute]);
        let t = Instant::now();
        step(&mut usb, &up, &down, t);
        assert!(take_offs().is_empty());
        assert!(usb
            .engine
            .toasts
            .lock()
            .iter()
            .any(|toast| { toast.warn && toast.text == "Turn off works over Bluetooth" }));

        let mut missing = rig(Connection::Bluetooth, "not-a-mac", vec![off(Button::Mute)]);
        step(&mut missing, &up, &down, t);
        assert!(take_offs().is_empty());
        assert!(missing.engine.toasts.lock().iter().any(|toast| {
            toast.warn && toast.text == "No Bluetooth address, so the controller stays on"
        }));
    }
}

#[cfg(test)]
mod hold_profile_tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use ds_proto::input::Button;
    use ds_proto::{Connection, InputState};

    use super::turn_off_tests::{gate, take_offs};
    use super::Pipeline;
    use crate::device::Device;
    use crate::engine::Engine;
    use crate::profile::{
        Action, Gesture, GyroMode, HoldStyle, Mapping, Profile, StickMode, TouchMode, VirtualKind,
    };

    const MAC: &str = "AA-BB-CC-DD-EE-23";

    struct Rig {
        dir: PathBuf,
        engine: Arc<Engine>,
        device: Arc<Device>,
        pipe: Pipeline,
        prev: InputState,
        at: Instant,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl Rig {
        /// Feed one report, 10 ms after the last.
        fn step(&mut self, buttons: &[Button]) {
            let s = centered(buttons);
            self.at += Duration::from_millis(10);
            self.pipe
                .process(&self.engine, &self.device, &s, &self.prev, None, self.at);
            self.prev = s;
        }
        fn wait(&mut self, ms: u64) {
            let held: Vec<Button> = self.prev.buttons.iter().collect();
            self.at += Duration::from_millis(ms);
            self.step(&held);
        }
        fn profile(&self) -> String {
            self.engine.profile_name_for(&self.device)
        }
        fn saved(&self) -> Option<String> {
            self.engine.settings.read().device_profiles.get(MAC).cloned()
        }
    }

    fn plain(name: &str, mappings: Vec<Mapping>) -> Profile {
        let mut p = Profile::named(name);
        p.mappings = mappings;
        p.gyro.mode = GyroMode::Off;
        p.touchpad.mode = TouchMode::Passthrough;
        p.sticks.left.mode = StickMode::Passthrough;
        p.sticks.right.mode = StickMode::Passthrough;
        p.virtual_out.kind = VirtualKind::Off;
        p.haptics.buttons.enabled = false;
        p
    }

    fn hold(button: Button, with: &[Button], name: &str, then: &str, mute: bool) -> Mapping {
        Mapping {
            button,
            with: with.to_vec(),
            mute,
            action: Action::HoldProfile {
                name: name.into(),
                then: then.into(),
            },
            ..Mapping::default()
        }
    }

    fn off(button: Button) -> Mapping {
        Mapping {
            button,
            action: Action::TurnOffController,
            ..Mapping::default()
        }
    }

    /// "Base" holds "Chord" with the given mapping. In Chord, Cross turns
    /// the controller off (a mapping the tests can see fire), and so does
    /// Create, the usual held button, which must not fire from the hold.
    fn rig(base: Vec<Mapping>) -> Rig {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("dsz-hold-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let engine = Engine::new(dir.clone());
        let device = Device::for_test(Connection::Bluetooth, MAC);
        engine.profiles.add(plain("Base", base));
        engine
            .profiles
            .add(plain("Chord", vec![off(Button::Cross), off(Button::Create)]));
        engine.profiles.add(plain("After", vec![]));
        engine.profiles.add(plain("Game", vec![]));
        engine.set_device_profile(&device, "Base");
        let _ = take_offs();
        Rig {
            dir,
            engine,
            device,
            pipe: Pipeline::new(),
            prev: centered(&[]),
            at: Instant::now(),
        }
    }

    fn centered(buttons: &[Button]) -> InputState {
        let mut s = InputState {
            lx: 128,
            ly: 128,
            rx: 128,
            ry: 128,
            ..InputState::default()
        };
        for b in buttons {
            s.buttons.set(*b, true);
        }
        s
    }

    #[test]
    fn held_profile_lasts_while_the_button_is_down() {
        let _g = gate();
        let mut r = rig(vec![hold(Button::Create, &[], "Chord", "", true)]);
        r.step(&[]);
        assert_eq!(r.profile(), "Base");
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Chord");
        // Held: the held button's own mapping in Chord doesn't fire.
        r.step(&[Button::Create]);
        r.wait(600);
        assert!(take_offs().is_empty());
        // Chord's mappings work while held.
        r.step(&[Button::Create, Button::Cross]);
        assert_eq!(take_offs(), vec![MAC.to_string()]);
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Chord");
        r.step(&[]);
        assert_eq!(r.profile(), "Base");
        // Back on Base, Cross does nothing, and nothing was saved.
        r.step(&[Button::Cross]);
        assert!(take_offs().is_empty());
        assert_eq!(r.saved().as_deref(), Some("Base"));
        assert!(r.engine.toasts.lock().iter().all(|t| !t.text.contains("Chord")));
        // And it works again.
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Chord");
        r.step(&[]);
        assert_eq!(r.profile(), "Base");
    }

    #[test]
    fn behavior_setting_does_not_latch_a_held_profile() {
        let _g = gate();
        for mode in [HoldStyle::Toggle, HoldStyle::Tap, HoldStyle::Turbo] {
            let mut r = rig(vec![Mapping {
                mode,
                ..hold(Button::Create, &[], "Chord", "", false)
            }]);
            r.step(&[Button::Create]);
            r.wait(300);
            assert_eq!(r.profile(), "Chord", "{mode:?}");
            r.step(&[]);
            assert_eq!(r.profile(), "Base", "{mode:?}");
        }
    }

    #[test]
    fn release_profile_sticks() {
        let _g = gate();
        let mut r = rig(vec![hold(Button::Create, &[], "Chord", "After", true)]);
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Chord");
        assert_eq!(r.saved().as_deref(), Some("Base"));
        r.step(&[]);
        assert_eq!(r.profile(), "After");
        assert_eq!(r.saved().as_deref(), Some("After"));
        // A release profile that is gone goes back instead.
        let mut r = rig(vec![hold(Button::Create, &[], "Chord", "Missing", true)]);
        r.step(&[Button::Create]);
        r.step(&[]);
        assert_eq!(r.profile(), "Base");
    }

    #[test]
    fn combo_ends_on_either_button_and_keeps_hiding_the_rest() {
        let _g = gate();
        let mut r = rig(vec![hold(Button::L1, &[Button::Create], "Chord", "", true)]);
        let both = Button::L1.bit() | Button::Create.bit();
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Base");
        assert_eq!(r.pipe.held_hide, 0);
        r.step(&[Button::Create, Button::L1]);
        assert_eq!(r.profile(), "Chord");
        assert_eq!(r.pipe.held_hide, both);
        r.step(&[Button::Create, Button::L1]);
        assert_eq!(r.pipe.held_hide, both, "hidden from Chord's pad too");
        // L1 up ends the hold; Create, still down, stays hidden until let go.
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Base");
        assert_eq!(r.pipe.held_hide, Button::Create.bit());
        r.step(&[]);
        assert_eq!(r.pipe.held_hide, 0);

        // Without "Hide from game" the game sees the buttons.
        let mut r = rig(vec![hold(Button::Create, &[], "Chord", "", false)]);
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Chord");
        assert_eq!(r.pipe.held_hide, 0);
    }

    #[test]
    fn long_press_holds_after_the_wait() {
        let _g = gate();
        let mut r = rig(vec![Mapping {
            gesture: Gesture::LongPress,
            long_ms: 400,
            ..hold(Button::Ps, &[], "Chord", "", true)
        }]);
        r.step(&[Button::Ps]);
        r.wait(200);
        assert_eq!(r.profile(), "Base");
        r.wait(250);
        assert_eq!(r.profile(), "Chord");
        r.step(&[]);
        assert_eq!(r.profile(), "Base");
        // A short press never switches.
        r.step(&[Button::Ps]);
        r.step(&[]);
        r.wait(500);
        assert_eq!(r.profile(), "Base");
    }

    #[test]
    fn game_switch_while_held_is_where_release_goes() {
        let _g = gate();
        let mut r = rig(vec![hold(Button::Create, &[], "Chord", "", true)]);
        r.step(&[Button::Create]);
        *r.engine.game_override.write() = Some(("Game".into(), "game.exe".into()));
        r.engine.bump_config();
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Chord", "the hold wins over a game's profile");
        r.step(&[]);
        assert_eq!(r.profile(), "Game");
        // The game's rule is still on: the hold didn't dismiss it.
        assert!(r.engine.game_dismissed.lock().is_none());
        *r.engine.game_override.write() = None;
        r.engine.bump_config();
        assert_eq!(r.profile(), "Base");
    }

    #[test]
    fn a_pick_while_held_stays() {
        let _g = gate();
        let mut r = rig(vec![hold(Button::Create, &[], "Chord", "After", true)]);
        r.step(&[Button::Create]);
        r.engine.set_device_profile(&r.device, "Game");
        assert_eq!(r.profile(), "Game");
        r.step(&[Button::Create]);
        r.step(&[]);
        assert_eq!(r.profile(), "Game", "no release profile after a pick");
    }

    #[test]
    fn disconnect_while_held_drops_the_hold() {
        let _g = gate();
        let mut r = rig(vec![hold(Button::Create, &[], "Chord", "After", true)]);
        r.step(&[Button::Create]);
        assert_eq!(r.profile(), "Chord");
        r.engine.cancel_held_profile(&r.device);
        assert_eq!(r.profile(), "Base");
        assert_eq!(r.saved().as_deref(), Some("Base"));
        assert!(r.device.held_profile.lock().is_none());
    }

    #[test]
    fn one_hold_at_a_time_and_missing_profiles_do_nothing() {
        let _g = gate();
        let mut r = rig(vec![
            hold(Button::Create, &[], "Chord", "", true),
            hold(Button::Options, &[], "Missing", "", true),
        ]);
        r.step(&[Button::Options]);
        assert_eq!(r.profile(), "Base");
        assert!(r.device.held_profile.lock().is_none());
        r.step(&[]);
        // Chord has no hold of its own; a second hold from Base's
        // mappings can't start while Chord is held.
        r.step(&[Button::Create]);
        assert!(!r.engine.hold_profile(
            &r.device,
            crate::engine::HeldProfile {
                name: "After".into(),
                buttons: Button::Square.bit(),
                hide: 0,
                then: None,
            }
        ));
        assert_eq!(r.profile(), "Chord");
        r.step(&[]);
        assert_eq!(r.profile(), "Base");
    }

    #[test]
    fn virtual_pad_follows_the_profile_under_the_hold() {
        let _g = gate();
        let mut r = rig(vec![hold(Button::Create, &[], "Chord", "", true)]);
        assert!(r.engine.unheld_profile(&r.device).is_none());
        r.step(&[Button::Create]);
        assert_eq!(r.engine.unheld_profile(&r.device).unwrap().name, "Base");
        r.step(&[]);
        assert!(r.engine.unheld_profile(&r.device).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_proto::input::TouchPoint;

    #[test]
    fn directional_reach_only_shapes_passthrough_virtual_sticks() {
        let mut profile = Profile::default();
        let vo = &mut profile.virtual_out;
        vo.left_deadzone.shape = DeadzoneShape::None;
        vo.right_deadzone.shape = DeadzoneShape::None;
        vo.left_deadzone.reach.enabled = true;
        vo.left_deadzone.reach.gains[8] = 116.0;
        vo.right_deadzone.reach.enabled = true;
        vo.right_deadzone.reach.gains[0] = 75.0;
        let s = InputState { lx: 128, ly: 237, rx: 128, ry: 0, ..Default::default() };
        let pad = virtual_pad(&profile, &s, 0, 0, StickMode::Passthrough, StickMode::Passthrough, TouchMode::Passthrough);
        assert!(pad.ly >= 254);
        assert!((31..=33).contains(&pad.ry));
        assert_eq!((s.lx, s.ly, s.rx, s.ry), (128, 237, 128, 0));
        for mode in [StickMode::Mouse, StickMode::Scroll, StickMode::Wasd, StickMode::Arrows] {
            let pad = virtual_pad(&profile, &s, 0, 0, mode, mode, TouchMode::Passthrough);
            assert_eq!((pad.lx, pad.ly, pad.rx, pad.ry), (128, 128, 128, 128));
        }
        profile.virtual_out.invert[1] = true;
        let pad = virtual_pad(&profile, &s, 0, 0, StickMode::Passthrough, StickMode::Passthrough, TouchMode::Passthrough);
        assert_eq!(pad.ly, 18, "reach follows the existing inversion order");
        profile.virtual_out.left_deadzone.reach.gains[0] = 116.0;
        let pad = virtual_pad(&profile, &s, 0, 0, StickMode::Passthrough, StickMode::Passthrough, TouchMode::Passthrough);
        assert!(pad.ly <= 1);
    }

    fn finger(x: u16) -> TouchPoint {
        TouchPoint {
            active: true,
            id: 1,
            x,
            y: 400,
        }
    }

    fn click_at(x: Option<u16>) -> InputState {
        let mut s = InputState::default();
        s.buttons.set(Button::Touchpad, true);
        if let Some(x) = x {
            s.touch[0] = finger(x);
        }
        s
    }

    #[test]
    fn zone_follows_the_finger_across_reports() {
        let mut p = Pipeline::new();
        let mut prev = InputState::default();
        let mut cur = click_at(Some(100));
        p.apply_touchpad_zones(&mut cur, &mut prev);
        assert!(cur.buttons.has(Button::Touchpad));
        assert!(cur.buttons.has(Button::TouchpadLeft));
        assert!(!prev.buttons.has(Button::TouchpadLeft));

        // Held, still on the left: not a new press.
        let mut prev = click_at(Some(100));
        let mut cur = click_at(Some(200));
        p.apply_touchpad_zones(&mut cur, &mut prev);
        assert!(prev.buttons.has(Button::TouchpadLeft));
        assert!(cur.buttons.has(Button::TouchpadLeft));
        assert!(!cur
            .buttons
            .pressed_since(prev.buttons)
            .has(Button::TouchpadLeft));

        // Sliding into the right third releases left and presses right.
        let mut prev = click_at(Some(200));
        let mut cur = click_at(Some(1500));
        p.apply_touchpad_zones(&mut cur, &mut prev);
        assert!(prev.buttons.has(Button::TouchpadLeft));
        assert!(cur.buttons.has(Button::TouchpadRight));
        assert!(cur
            .buttons
            .pressed_since(prev.buttons)
            .has(Button::TouchpadRight));
        assert!(cur
            .buttons
            .released_since(prev.buttons)
            .has(Button::TouchpadLeft));
        // The whole-pad button never edges.
        assert!(!cur
            .buttons
            .pressed_since(prev.buttons)
            .has(Button::Touchpad));
    }

    #[test]
    fn click_with_no_finger_stays_plain_touchpad() {
        let mut p = Pipeline::new();
        let mut prev = InputState::default();
        let mut cur = click_at(None);
        p.apply_touchpad_zones(&mut cur, &mut prev);
        assert!(cur.buttons.has(Button::Touchpad));
        assert!(!cur.buttons.iter().any(|b| b.is_touchpad_zone()));

        // The finger from the report before the click still places it.
        let mut prev = InputState::default();
        prev.touch[0] = finger(1280);
        let mut cur = click_at(None);
        p.apply_touchpad_zones(&mut cur, &mut prev);
        assert!(cur.buttons.has(Button::TouchpadRight));

        // It keeps that zone while the pad stays down, even with no finger.
        let mut prev = click_at(None);
        let mut cur = click_at(None);
        p.apply_touchpad_zones(&mut cur, &mut prev);
        assert!(cur.buttons.has(Button::TouchpadRight));
        assert!(prev.buttons.has(Button::TouchpadRight));

        // Releasing the pad clears it, and the previous report still shows it.
        let mut prev = click_at(None);
        let mut cur = InputState::default();
        p.apply_touchpad_zones(&mut cur, &mut prev);
        assert!(!cur.buttons.has(Button::TouchpadRight));
        assert!(prev.buttons.has(Button::TouchpadRight));
    }

    #[test]
    fn virtual_pad_still_has_one_touchpad_button() {
        let profile = Profile::default();
        let s = click_at(Some(10));
        let mut s = s;
        s.buttons.set(Button::TouchpadLeft, true);
        let modes = (StickMode::Passthrough, StickMode::Passthrough);
        let pad = virtual_pad(&profile, &s, 0, 0, modes.0, modes.1, TouchMode::Passthrough);
        assert!(pad.buttons.has(Button::Touchpad));
        assert!(!pad.buttons.has(Button::TouchpadLeft));

        // A muted zone suppresses the one touchpad button.
        let pad = virtual_pad(
            &profile,
            &s,
            Button::Touchpad.bit(),
            0,
            modes.0,
            modes.1,
            TouchMode::Passthrough,
        );
        assert!(!pad.buttons.has(Button::Touchpad));
        assert!(!pad.buttons.has(Button::TouchpadLeft));

        // Touchpad-as-mouse still drops the click when passthrough is off.
        let mut profile = profile;
        profile.touchpad.click_to_click = true;
        profile.virtual_out.touch_passthrough = false;
        let pad = virtual_pad(&profile, &s, 0, 0, modes.0, modes.1, TouchMode::Mouse);
        assert!(!pad.buttons.has(Button::Touchpad));
    }
}
