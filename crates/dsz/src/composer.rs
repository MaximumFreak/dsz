//! Builds the control report state from the active profile, live input,
//! mod overrides, and transient feedback. Runs on the writer thread.

use std::time::{Duration, Instant};

use ds_proto::output::{player_pattern, OutputState};
use ds_proto::{BatteryStatus, InputState};

use crate::device::{Feedback, Overrides, Toggles};
use crate::profile::{LedBrightness, LightMode, MuteLed, PlayerLeds, Profile};

pub struct Composer {
    hue: f32,
    breath: f32,
    breath_dir: f32,
    strobe_on: bool,
    last_strobe: Instant,
    last: Instant,
    /// When the battery went low, the start of the warning blink pattern.
    low_since: Option<Instant>,
    /// The last report ran the classic motors.
    motors_on: bool,
    /// The battery blend shown, eased toward the battery's level so a 10 %
    /// step doesn't jump; see [`battery_blend`].
    blend: Option<f32>,
    /// Lightbar level for the idle power-off countdown, 1 while in use;
    /// see [`idle_fade`]. Set by the writer before each report.
    pub idle_level: f32,
}

pub fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    ]
}

fn scale(c: [u8; 3], k: f32) -> [u8; 3] {
    let k = k.clamp(0.0, 1.0);
    [
        (c[0] as f32 * k).round() as u8,
        (c[1] as f32 * k).round() as u8,
        (c[2] as f32 * k).round() as u8,
    ]
}

fn lerp(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    [f(a[0], b[0]), f(a[1], b[1]), f(a[2], b[2])]
}

/// Red at empty, amber in the middle, green when full.
pub fn battery_color(percent: u8) -> [u8; 3] {
    let p = percent as f32 / 100.0;
    if p < 0.5 {
        lerp([255, 32, 16], [255, 170, 0], p * 2.0)
    } else {
        lerp([255, 170, 0], [40, 220, 90], (p - 0.5) * 2.0)
    }
}

/// How far toward the empty color the battery blend is (0 = the set color,
/// 1 = the empty color): none at or above `from` percent, all of it at
/// 0 %, in a straight line between, as DS4Windows blends its "color by
/// battery" from 100 % down. None while the level is unknown (the short
/// Bluetooth report).
pub fn battery_blend(input: &InputState, from: u8) -> Option<f32> {
    if !input.full {
        return None;
    }
    let from = from.clamp(10, 100) as f32;
    let pct = input.battery_percent.min(100) as f32;
    Some(((from - pct) / from).clamp(0.0, 1.0))
}

/// The blend moves at most this much a second, so a 10 % step of the
/// whole-charge blend takes two seconds.
const BLEND_RATE: f32 = 0.05;

/// Longest stretch the lightbar takes to fade out before an idle power-off.
const IDLE_FADE_MAX: Duration = Duration::from_secs(5 * 60);
/// The fade moves in this many steps, each a single report with the new
/// color: a few seconds apart over five minutes, too small to see as steps,
/// and no more reports than the keepalive sends while the pad lies idle.
const IDLE_FADE_STEPS: f32 = 32.0;

/// Lightbar level (1 = as set, 0 = dark) after `idle` without input, when
/// the controller turns itself off after `timeout`. Like DS4Windows' idle
/// fade, it eases out to dark over the second half of the timeout, so the
/// first dimming shows soonest and the bar is dark as the controller turns
/// off, but over five minutes at most, so a long timeout keeps the bar lit
/// for most of the wait. Rounded up to [`IDLE_FADE_STEPS`].
pub fn idle_fade(idle: Duration, timeout: Duration) -> f32 {
    if timeout.is_zero() {
        return 1.0;
    }
    let window = (timeout / 2).min(IDLE_FADE_MAX);
    let start = timeout - window;
    if idle <= start {
        return 1.0;
    }
    let e = ((idle - start).as_secs_f32() / window.as_secs_f32()).min(1.0);
    ((1.0 - e * (2.0 - e)) * IDLE_FADE_STEPS).ceil() / IDLE_FADE_STEPS
}

/// Low-battery warning: two short blinks of the warning color, then a
/// pause, every 2.5 s.
fn low_battery_blink(t: Duration) -> bool {
    let ms = t.as_millis() % 2500;
    ms < 150 || (300..450).contains(&ms)
}

/// The player LEDs a profile shows, before game and mod overrides.
pub fn profile_player_leds(l: &crate::profile::Lighting, input: &InputState) -> u8 {
    match l.player_leds {
        PlayerLeds::Player(n) => player_pattern(n),
        PlayerLeds::Custom(mask) => mask & 0x1F,
        PlayerLeds::Off => 0,
        PlayerLeds::Battery => {
            // Five lights, each worth 20 %, rounded to the nearest light
            // (at least one), filled from the left like a gauge.
            let n = ((input.battery_percent.min(100) as u32 + 10) / 20).clamp(1, 5);
            (1u8 << n) - 1
        }
    }
}

impl Composer {
    pub fn new() -> Self {
        let now = Instant::now();
        Composer {
            hue: 0.0,
            breath: 0.0,
            breath_dir: 1.0,
            strobe_on: true,
            last_strobe: now,
            last: now,
            low_since: None,
            motors_on: false,
            blend: None,
            idle_level: 1.0,
        }
    }

    /// Lightbar color for the profile at `now`, without overrides.
    /// `loudness` is the system-audio level, 0 at silence and about 1 at full scale.
    pub fn lightbar(
        &mut self,
        p: &Profile,
        input: &InputState,
        loudness: f32,
        now: Instant,
    ) -> [u8; 3] {
        let l = &p.lighting;
        let dt = now
            .saturating_duration_since(self.last)
            .as_secs_f32()
            .min(0.1);
        self.last = now;
        let bright = l.brightness.min(100) as f32 / 100.0;
        let speed = l.speed.clamp(0.0, 1.0);
        let color = self.mode_color(l, input, loudness, now, dt, bright, speed);
        if !l.battery_fade
            || !matches!(l.mode, LightMode::Static | LightMode::Breathing | LightMode::Strobe)
        {
            self.blend = None;
            return color;
        }
        let Some(target) = battery_blend(input, l.battery_fade_from) else {
            return color;
        };
        let shown = match self.blend {
            Some(b) if (target - b).abs() > BLEND_RATE * dt => {
                b + (target - b).signum() * BLEND_RATE * dt
            }
            _ => target,
        };
        self.blend = Some(shown);
        let empty = scale(
            l.battery_fade_color,
            l.battery_fade_brightness.min(100) as f32 / 100.0,
        );
        lerp(color, empty, shown)
    }

    #[allow(clippy::too_many_arguments)]
    fn mode_color(
        &mut self,
        l: &crate::profile::Lighting,
        input: &InputState,
        loudness: f32,
        now: Instant,
        dt: f32,
        bright: f32,
        speed: f32,
    ) -> [u8; 3] {
        match l.mode {
            LightMode::Off => [0, 0, 0],
            LightMode::Static => scale(l.color, bright),
            LightMode::Rainbow => {
                // 0.25 to 6 revolutions per... 20 s to 1.2 s per cycle.
                let period = 20.0 * (1.0 - speed) + 1.2 * speed;
                self.hue = (self.hue + 360.0 * dt / period) % 360.0;
                hsv(self.hue, 1.0, bright)
            }
            LightMode::Breathing => {
                let period = 6.0 * (1.0 - speed) + 0.8 * speed;
                self.breath += self.breath_dir * dt * 2.0 / period;
                if self.breath >= 1.0 {
                    self.breath = 1.0;
                    self.breath_dir = -1.0;
                } else if self.breath <= 0.0 {
                    self.breath = 0.0;
                    self.breath_dir = 1.0;
                }
                // Ease so it lingers at the ends like a real breath.
                let t = self.breath * self.breath * (3.0 - 2.0 * self.breath);
                scale(lerp(l.color2, l.color, t), bright)
            }
            LightMode::Strobe => {
                let half = Duration::from_secs_f32(0.5 * (1.0 - speed) + 0.03 * speed);
                if now.saturating_duration_since(self.last_strobe) >= half {
                    self.strobe_on = !self.strobe_on;
                    self.last_strobe = now;
                }
                if self.strobe_on {
                    scale(l.color, bright)
                } else {
                    scale(l.color2, bright)
                }
            }
            LightMode::Battery => {
                let c = battery_color(input.battery_percent);
                if input.battery_status == BatteryStatus::Charging {
                    // Slow pulse while charging.
                    self.breath = (self.breath + dt * 0.6) % 2.0;
                    let t = if self.breath > 1.0 {
                        2.0 - self.breath
                    } else {
                        self.breath
                    };
                    scale(c, bright * (0.35 + 0.65 * t))
                } else {
                    scale(c, bright)
                }
            }
            LightMode::Audio => crate::audio_led::paint(
                loudness,
                l.audio_quiet,
                l.audio_medium,
                l.audio_loud,
                l.audio_medium_at,
                l.audio_loud_at,
                l.brightness,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compose(
        &mut self,
        p: &Profile,
        input: &InputState,
        overrides: &Overrides,
        feedback: &Feedback,
        toggles: &Toggles,
        pcm_haptics: bool,
        loudness: f32,
        now: Instant,
    ) -> OutputState {
        let l = &p.lighting;
        let mut out = OutputState::default();

        // Lightbar
        let vo = &p.virtual_out;
        let game = &feedback.game;
        let mut color = self.lightbar(p, input, loudness, now);
        if self.idle_level < 1.0 {
            // Only the profile's own light fades: a game's or a mod's color,
            // the low-battery blink and the identify flash stay as sent.
            color = scale(color, self.idle_level);
        }
        if let (true, Some(rgb)) = (vo.game_leds, game.rgb) {
            color = rgb;
        }
        if let Some(rgb) = overrides.rgb {
            color = rgb;
        }
        let low_battery = l.low_battery_flash
            && input.full
            && input.battery_percent > 0
            && input.battery_percent <= l.low_battery_threshold
            && input.battery_status == BatteryStatus::Discharging;
        if low_battery {
            let since = *self.low_since.get_or_insert(now);
            if low_battery_blink(now.saturating_duration_since(since)) {
                color = l.low_battery_color;
            }
        } else {
            self.low_since = None;
        }
        if let Some(until) = feedback.identify_until {
            if now < until {
                let phase = ((until - now).as_millis() / 150).is_multiple_of(2);
                color = if phase { [255, 255, 255] } else { [0, 0, 0] };
            }
        }
        out.lightbar = color;

        // Player LEDs
        out.player_leds = profile_player_leds(l, input);
        if let (true, Some(pl)) = (vo.game_leds, game.player_leds) {
            out.player_leds = pl & 0x1F;
        }
        if let Some(pl) = overrides.player_leds {
            out.player_leds = pl;
        }
        if feedback.identify_until.map(|u| now < u).unwrap_or(false) {
            out.player_leds = 0x1F;
        }
        out.led_brightness = match l.led_brightness {
            LedBrightness::High => 0,
            LedBrightness::Medium => 1,
            LedBrightness::Low => 2,
        };

        // Mute LED
        out.mute_led = match l.mute_led {
            MuteLed::Off => 0,
            MuteLed::On => 1,
            MuteLed::Pulse => 2,
            MuteLed::Toggle => toggles.mute_engaged as u8,
        };
        if let (true, Some(m)) = (vo.game_leds, game.mute_led) {
            out.mute_led = m;
        }
        if let Some(m) = overrides.mic_led {
            out.mute_led = m;
        }

        // Triggers
        // Mod overrides, then what the game sent, then the profile.
        let gl = game.left_trigger.filter(|_| vo.game_triggers);
        let gr = game.right_trigger.filter(|_| vo.game_triggers);
        out.left_trigger = overrides
            .left_trigger
            .or(gl)
            .unwrap_or_else(|| p.triggers.left.encode());
        out.right_trigger = overrides
            .right_trigger
            .or(gr)
            .unwrap_or_else(|| p.triggers.right.encode());
        if let Some((l2, r2, until)) = feedback.trigger_preview {
            if now < until {
                if let Some(b) = l2 {
                    out.left_trigger = b;
                }
                if let Some(b) = r2 {
                    out.right_trigger = b;
                }
            }
        }

        // Rumble. While PCM haptics stream, the motor fields are marked
        // invalid so the actuators keep playing the stream; rumble requests
        // are rendered into that stream instead. Handing over from running
        // motors sends one more valid report with them at zero first, so the
        // firmware's rumble stops rather than holding its last level.
        out.rumble_reduction = p.rumble.power_reduction.min(7);
        out.enhanced_rumble = p.rumble.enhanced;
        if pcm_haptics {
            out.rumble_valid = self.motors_on;
            self.motors_on = false;
        } else {
            let (mut lo, mut hi) = feedback.rumble_now(now);
            if vo.game_rumble {
                let (gh, gl) = feedback.game_rumble_now();
                lo = lo.max(gh);
                hi = hi.max(gl);
            }
            out.rumble_left = lo;
            out.rumble_right = hi;
            self.motors_on = lo > 0 || hi > 0;
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_gauge_rounds_to_the_nearest_light() {
        let leds = |pct: u8| {
            let mut l = crate::profile::Lighting::default();
            l.player_leds = PlayerLeds::Battery;
            let input = InputState {
                battery_percent: pct,
                ..Default::default()
            };
            profile_player_leds(&l, &input)
        };
        assert_eq!(leds(0), 0x01);
        assert_eq!(leds(29), 0x01);
        assert_eq!(leds(30), 0x03);
        assert_eq!(leds(50), 0x07);
        assert_eq!(leds(89), 0x0F);
        assert_eq!(leds(90), 0x1F);
        assert_eq!(leds(100), 0x1F);
    }

    #[test]
    fn idle_fade_eases_out_over_the_end_of_the_timeout() {
        let m = |x: u64| Duration::from_secs(x * 60);
        let s = |x: u64| Duration::from_secs(x);
        // 15 minutes: lit for 10, then five minutes to dark.
        assert_eq!(idle_fade(m(0), m(15)), 1.0);
        assert_eq!(idle_fade(m(10), m(15)), 1.0);
        let half = idle_fade(m(10) + s(150), m(15));
        assert_eq!(half, 0.25);
        assert_eq!(idle_fade(m(10) + s(1), m(15)), 1.0, "a step at a time");
        assert_eq!(idle_fade(m(15), m(15)), 0.0);
        assert_eq!(idle_fade(m(20), m(15)), 0.0);
        // Short timeouts fade over their second half, as DS4Windows does.
        assert_eq!(idle_fade(s(30), m(1)), 1.0);
        assert!(idle_fade(s(45), m(1)) < 0.3);
        // Steadily darker, never brighter, along the way, in few steps.
        let mut last = 1.0;
        let mut steps = 0;
        for t in 0..=900 {
            let v = idle_fade(s(t), m(15));
            assert!(v <= last && (0.0..=1.0).contains(&v));
            steps += (v != last) as u32;
            last = v;
        }
        assert!(steps <= IDLE_FADE_STEPS as u32, "{steps}");
        assert_eq!(idle_fade(m(5), Duration::ZERO), 1.0);
    }

    #[test]
    fn idle_fade_dims_only_the_profile_light() {
        let mut c = Composer::new();
        let mut p = Profile::default();
        p.lighting.brightness = 100;
        p.lighting.color = [200, 100, 40];
        let input = InputState::default();
        let none = crate::device::Overrides::default();
        let toggles = crate::device::Toggles::default();
        let now = Instant::now();
        let fb = crate::device::Feedback::default();
        let lit = c.compose(&p, &input, &none, &fb, &toggles, false, 0.0, now);
        assert_eq!(lit.lightbar, [200, 100, 40], "in use: as set");
        c.idle_level = 0.5;
        let dim = c.compose(&p, &input, &none, &fb, &toggles, false, 0.0, now);
        assert_eq!(dim.lightbar, [100, 50, 20]);
        assert_eq!(OutputState { lightbar: lit.lightbar, ..dim }, lit, "nothing else changes");
        // A game's color stays as the game sent it.
        p.virtual_out.game_leds = true;
        let mut game = crate::device::Feedback::default();
        game.game.rgb = Some([10, 20, 30]);
        assert_eq!(c.compose(&p, &input, &none, &game, &toggles, false, 0.0, now).lightbar, [10, 20, 30]);
        // The low-battery blink shows at full strength.
        let low = InputState {
            full: true,
            battery_percent: 10,
            battery_status: BatteryStatus::Discharging,
            ..Default::default()
        };
        let blink = c.compose(&p, &low, &none, &fb, &toggles, false, 0.0, now);
        assert_eq!(blink.lightbar, p.lighting.low_battery_color);
    }

    #[test]
    fn battery_blend_follows_the_charge() {
        let at = |pct: u8, from: u8| {
            let input = InputState {
                full: true,
                battery_percent: pct,
                battery_status: BatteryStatus::Discharging,
                ..Default::default()
            };
            battery_blend(&input, from).unwrap()
        };
        assert_eq!(at(100, 100), 0.0);
        assert_eq!(at(50, 100), 0.5);
        assert_eq!(at(0, 100), 1.0);
        assert_eq!(at(60, 50), 0.0, "not yet below the start");
        assert_eq!(at(50, 50), 0.0);
        assert_eq!(at(20, 50), 0.6);
        assert_eq!(at(0, 50), 1.0);
        assert_eq!(battery_blend(&InputState::default(), 100), None, "level unknown");
    }

    #[test]
    fn battery_fade_mixes_toward_the_empty_color_only_when_on() {
        let mut p = Profile::default();
        p.lighting.brightness = 100;
        p.lighting.color = [0, 0, 255];
        let half = InputState {
            full: true,
            battery_percent: 50,
            battery_status: BatteryStatus::Discharging,
            ..Default::default()
        };
        let now = Instant::now();
        // Off (every existing profile): the color as set, at any level.
        let mut c = Composer::new();
        assert_eq!(c.lightbar(&p, &half, 0.0, now), [0, 0, 255]);
        for mode in [LightMode::Rainbow, LightMode::Battery, LightMode::Audio, LightMode::Off] {
            let mut q = p.clone();
            q.lighting.mode = mode;
            let plain = Composer::new().lightbar(&q, &half, 0.0, now);
            q.lighting.battery_fade = true;
            assert_eq!(Composer::new().lightbar(&q, &half, 0.0, now), plain, "{mode:?} ignores it");
        }
        // On: blue to purple to red.
        p.lighting.battery_fade = true;
        p.lighting.battery_fade_color = [255, 0, 0];
        assert_eq!(c.lightbar(&p, &half, 0.0, now), [128, 0, 128]);
        // The empty end has its own brightness.
        p.lighting.brightness = 50;
        let empty = InputState { battery_percent: 0, ..half };
        let mut c = Composer::new();
        assert_eq!(c.lightbar(&p, &empty, 0.0, now), [255, 0, 0]);
        // A step down eases over rather than jumping.
        let mut c = Composer::new();
        p.lighting.brightness = 100;
        let full = InputState { battery_percent: 100, ..half };
        assert_eq!(c.lightbar(&p, &full, 0.0, now), [0, 0, 255]);
        let ninety = InputState { battery_percent: 90, ..half };
        let soon = c.lightbar(&p, &ninety, 0.0, now + Duration::from_millis(100));
        assert!(soon[0] < 26 && soon[0] > 0, "{soon:?}");
        let mut t = now + Duration::from_millis(100);
        for _ in 0..30 {
            t += Duration::from_millis(100);
            c.lightbar(&p, &ninety, 0.0, t);
        }
        let end = c.lightbar(&p, &ninety, 0.0, t);
        assert!(end[0].abs_diff(26) <= 1 && end[1] == 0 && end[2].abs_diff(230) <= 1, "{end:?}");
    }

    #[test]
    fn low_battery_double_blink() {
        let on = |ms: u64| low_battery_blink(Duration::from_millis(ms));
        assert!(on(0) && on(149) && !on(150) && !on(299));
        assert!(on(300) && on(449) && !on(450) && !on(2499));
        assert!(on(2500) && on(2800));
    }

    #[test]
    fn audio_bands_follow_level_until_a_game_color_wins() {
        let mut c = Composer::new();
        let mut p = Profile::default();
        p.lighting.mode = LightMode::Audio;
        p.lighting.brightness = 50;
        let now = Instant::now();
        let input = InputState::default();
        let half = |ch: u8| (f32::from(ch) * 0.5).round() as u8;
        let quiet = c.lightbar(&p, &input, 0.0, now);
        assert_eq!(
            quiet,
            [
                half(p.lighting.audio_quiet[0]),
                half(p.lighting.audio_quiet[1]),
                half(p.lighting.audio_quiet[2]),
            ]
        );
        p.lighting.brightness = 100;
        let loud = c.compose(
            &p,
            &input,
            &crate::device::Overrides::default(),
            &crate::device::Feedback::default(),
            &crate::device::Toggles::default(),
            false,
            0.9,
            now,
        );
        assert_eq!(loud.lightbar, p.lighting.audio_loud);
        p.virtual_out.game_leds = true;
        let mut fb = crate::device::Feedback::default();
        fb.game.rgb = Some([1, 2, 3]);
        let game = c.compose(
            &p,
            &input,
            &crate::device::Overrides::default(),
            &fb,
            &crate::device::Toggles::default(),
            false,
            0.9,
            now,
        );
        assert_eq!(game.lightbar, [1, 2, 3]);
    }
}
