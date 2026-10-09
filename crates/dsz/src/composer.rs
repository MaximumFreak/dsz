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
        }
    }

    /// Lightbar color for the profile at `now`, without overrides.
    pub fn lightbar(&mut self, p: &Profile, input: &InputState, now: Instant) -> [u8; 3] {
        let l = &p.lighting;
        let dt = now
            .saturating_duration_since(self.last)
            .as_secs_f32()
            .min(0.1);
        self.last = now;
        let bright = l.brightness.min(100) as f32 / 100.0;
        let speed = l.speed.clamp(0.0, 1.0);
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
        now: Instant,
    ) -> OutputState {
        let l = &p.lighting;
        let mut out = OutputState::default();

        // Lightbar
        let vo = &p.virtual_out;
        let game = &feedback.game;
        let mut color = self.lightbar(p, input, now);
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
    fn low_battery_double_blink() {
        let on = |ms: u64| low_battery_blink(Duration::from_millis(ms));
        assert!(on(0) && on(149) && !on(150) && !on(299));
        assert!(on(300) && on(449) && !on(450) && !on(2499));
        assert!(on(2500) && on(2800));
    }
}
