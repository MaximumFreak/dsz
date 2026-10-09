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
        Action::Key { .. } | Action::Mouse { .. } | Action::Media { .. } | Action::Gamepad { .. }
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
                "Gyro mouse on"
            } else {
                "Gyro mouse off"
            });
        }
        Action::ToggleTouchpadMouse => {
            let live = device.live.lock().touch_mouse_active;
            device.toggles.lock().touchpad = Some(!live);
            engine.toast(if live {
                "Touchpad mouse off"
            } else {
                "Touchpad mouse on"
            });
        }
        Action::Profile { name } => {
            if engine.profiles.exists(name) {
                engine.set_device_profile(device, name);
                engine.toast(format!("Profile: {name}"));
                device.identify_light();
            }
        }
        // Read from the mapping state when the pad is built.
        Action::Gamepad { .. } => {}
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
        }
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
            let gdt = self.gyro.dt(s, now);
            dps = self.gyro.update_bias(s, now);
            let mode = toggles
                .gyro
                .map(|on| if on { GyroMode::Mouse } else { GyroMode::Off })
                .unwrap_or(profile.gyro.mode);
            if mode == GyroMode::Mouse {
                gyro_active = match profile.gyro.activation {
                    GyroActivation::Always => true,
                    GyroActivation::WhileHeld(b) => s.buttons.has(b),
                    GyroActivation::WhileNotHeld(b) => !s.buttons.has(b),
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
            self.touch.process(&profile.touchpad, s, prev, now);
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
            && profile.lighting.mute_led == MuteLed::Toggle
        {
            let mut t = device.toggles.lock();
            t.mute_engaged = !t.mute_engaged;
            drop(t);
            device.kick();
        }

        // Classic-rumble click feedback when PCM haptics are not available.
        if profile.haptics.buttons.enabled && !device.pcm_mode.load(Ordering::Acquire) {
            let pressed = s.buttons.pressed_since(prev.buttons);
            let pressed = ds_proto::Buttons(pressed.0 & !(Button::L2.bit() | Button::R2.bit()));
            if !pressed.is_empty() {
                let amp = (profile.haptics.buttons.intensity.clamp(0.0, 1.0) * 255.0) as u8;
                device.test_rumble(0, amp, profile.haptics.buttons.duration_ms.max(30.0) as u64);
            }
        }

        // Mappings
        let (suppress, inject) = self.mappings(engine, device, &profile, s, prev, now);

        // Virtual controller
        if device.virt.active().is_some() {
            let pad = virtual_pad(&profile, s, suppress, inject, lm, rm, touch_mode);
            let touch = touch_mode != TouchMode::Mouse || profile.virtual_out.touch_passthrough;
            device.virt.submit(&pad, raw, touch);
        }

        // Publish for the UI and the haptics thread.
        let mut live = device.live.lock();
        live.input = *s;
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
    /// controller and the virtual buttons mappings are pressing.
    fn mappings(
        &mut self,
        engine: &Engine,
        device: &Device,
        profile: &Profile,
        s: &InputState,
        prev: &InputState,
        now: Instant,
    ) -> (u32, u32) {
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
                    suppress |= m.button.bit();
                } else if down {
                    for b in m.buttons() {
                        rt.mute_latch |= b.bit();
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
        for (m, rt) in profile.mappings.iter().zip(self.maps.iter()) {
            if let Action::Gamepad { button } = m.action {
                if m.enabled && rt.out_down {
                    inject |= button.bit();
                }
            }
        }
        (suppress, inject)
    }
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
