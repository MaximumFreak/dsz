//! Event haptics rendered straight at the actuator rate (3 kHz): button
//! clicks, trigger texture, rumble, and test pulses.

use std::f32::consts::TAU;

use ds_proto::input::Button;

pub const RATE: f32 = 3000.0;

#[derive(Clone, Copy, Debug)]
struct Voice {
    freq: f32,
    phase: f32,
    amp: [f32; 2],
    left: u32,
    total: u32,
}

/// A continuous tone whose level glides toward a target.
#[derive(Clone, Copy, Debug, Default)]
struct Drone {
    freq: f32,
    phase: f32,
    level: [f32; 2],
    target: [f32; 2],
}

impl Drone {
    fn render(&mut self, out: &mut [[f32; 2]]) {
        for o in out.iter_mut() {
            for c in 0..2 {
                self.level[c] += (self.target[c] - self.level[c]) * 0.02;
            }
            if self.level[0] < 1e-4 && self.level[1] < 1e-4 && self.target == [0.0; 2] {
                continue;
            }
            let s = (self.phase * TAU).sin();
            self.phase = (self.phase + self.freq / RATE).fract();
            o[0] += s * self.level[0];
            o[1] += s * self.level[1];
        }
    }
}

/// Classic rumble on the actuators. The heavy motor is a low sine on the
/// left, the light motor a quieter, higher sine on the right. Pitch rises
/// with the motor level: heavy from 34 Hz to 50 Hz, light from 85 Hz to
/// 110 Hz. Gain is the mean of the level and its square root, which lifts
/// weak rumble so it can still be felt, scaled to 0.80 (heavy) and 0.36
/// (light) at full. The level eases in over 10 ms and out over 60 ms, and
/// the pitch glides over 25 ms, so a motor step does not click.
#[derive(Clone, Copy, Debug)]
struct Rumble {
    amp: [f32; 2],
    target: [f32; 2],
    freq: [f32; 2],
    freq_target: [f32; 2],
    phase: [f32; 2],
}

impl Default for Rumble {
    fn default() -> Self {
        Rumble {
            amp: [0.0; 2],
            target: [0.0; 2],
            freq: [34.0, 85.0],
            freq_target: [34.0, 85.0],
            phase: [0.0; 2],
        }
    }
}

/// One-pole smoothing step for time constant `tau` seconds at `RATE`.
fn smoothing(tau: f32) -> f32 {
    1.0 - (-1.0 / (RATE * tau)).exp()
}

impl Rumble {
    /// `heavy` and `light` are the motor levels as 0..=1 gains.
    fn set(&mut self, heavy: f32, light: f32) {
        let x = heavy.clamp(0.0, 1.0);
        let y = light.clamp(0.0, 1.0);
        let lift = |v: f32| (v + v.sqrt()) / 2.0;
        self.target = [0.80 * lift(x), 0.36 * lift(y)];
        self.freq_target = [34.0 + 16.0 * x, 85.0 + 25.0 * y];
    }

    fn active(&self) -> bool {
        self.amp[0] + self.amp[1] > 1e-3 || self.target != [0.0; 2]
    }

    fn render(&mut self, out: &mut [[f32; 2]]) {
        if !self.active() {
            return;
        }
        let attack = smoothing(0.010);
        let release = smoothing(0.060);
        let glide = smoothing(0.025);
        for o in out.iter_mut() {
            for c in 0..2 {
                let k = if self.target[c] > self.amp[c] {
                    attack
                } else {
                    release
                };
                self.amp[c] += (self.target[c] - self.amp[c]) * k;
                self.freq[c] += (self.freq_target[c] - self.freq[c]) * glide;
                o[c] += self.amp[c] * (self.phase[c] * TAU).sin();
                self.phase[c] = (self.phase[c] + self.freq[c] / RATE).fract();
            }
        }
    }
}

#[derive(Default)]
pub struct Synth {
    voices: Vec<Voice>,
    trig: [Drone; 2],
    rumble: Rumble,
}

/// Which actuator a button sits over: 0 left, 1 right, 2 both.
fn side(b: Button) -> usize {
    use Button::*;
    match b {
        DpadUp | DpadDown | DpadLeft | DpadRight | L1 | L2 | L3 | Create | FnLeft | PaddleLeft => 0,
        Cross | Circle | Square | Triangle | R1 | R2 | R3 | Options | FnRight | PaddleRight => 1,
        _ => 2,
    }
}

impl Synth {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> bool {
        !self.voices.is_empty()
            || self
                .trig
                .iter()
                .any(|d| d.level[0] + d.level[1] > 1e-3 || d.target != [0.0; 2])
            || self.rumble.active()
    }

    /// Something other than rumble is sounding: a pulse, a click, or
    /// trigger texture.
    pub fn events_active(&self) -> bool {
        !self.voices.is_empty()
            || self
                .trig
                .iter()
                .any(|d| d.level[0] + d.level[1] > 1e-3 || d.target != [0.0; 2])
    }

    pub fn pulse(&mut self, left: f32, right: f32, freq: f32, ms: f32) {
        let n = (ms.max(5.0) / 1000.0 * RATE) as u32;
        if self.voices.len() >= 16 {
            self.voices.remove(0);
        }
        self.voices.push(Voice {
            freq: freq.clamp(20.0, 1200.0),
            phase: 0.0,
            amp: [left.clamp(0.0, 1.0), right.clamp(0.0, 1.0)],
            left: n,
            total: n,
        });
    }

    /// A click for a newly pressed button, placed over its grip.
    pub fn button_click(&mut self, b: Button, intensity: f32, freq: f32, ms: f32) {
        let (l, r) = match side(b) {
            0 => (1.0, 0.25),
            1 => (0.25, 1.0),
            _ => (0.8, 0.8),
        };
        self.pulse(l * intensity, r * intensity, freq, ms);
    }

    /// Trigger texture: level follows pressure on each side.
    pub fn triggers(&mut self, l2: u8, r2: u8, intensity: f32, freq: f32, enabled: bool) {
        for (i, p) in [l2, r2].into_iter().enumerate() {
            let d = &mut self.trig[i];
            d.freq = freq.clamp(20.0, 600.0);
            let v = if enabled {
                p as f32 / 255.0 * intensity
            } else {
                0.0
            };
            d.target = if i == 0 { [v, v * 0.15] } else { [v * 0.15, v] };
        }
    }

    /// Classic rumble rendered on the actuators: heavy motor on the left,
    /// light motor on the right, each motor's level scaled by `intensity`.
    pub fn rumble(&mut self, heavy: u8, light: u8, intensity: f32) {
        self.rumble.set(
            heavy as f32 / 255.0 * intensity,
            light as f32 / 255.0 * intensity,
        );
    }

    pub fn render(&mut self, out: &mut [[f32; 2]]) {
        self.voices.retain(|v| v.left > 0);
        for v in self.voices.iter_mut() {
            for o in out.iter_mut() {
                if v.left == 0 {
                    break;
                }
                let t = (v.total - v.left) as f32;
                // 1.5 ms attack, exponential decay to about -40 dB at the end.
                let attack = (t / (0.0015 * RATE)).min(1.0);
                let decay = (-4.6 * t / v.total as f32).exp();
                let s = (v.phase * TAU).sin() * attack * decay;
                v.phase = (v.phase + v.freq / RATE).fract();
                o[0] += s * v.amp[0];
                o[1] += s * v.amp[1];
                v.left -= 1;
            }
        }
        for d in self.trig.iter_mut() {
            d.render(out);
        }
        self.rumble.render(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(s: &mut Synth, windows: usize) -> [f32; 2] {
        let mut p = [0f32; 2];
        for _ in 0..windows {
            let mut out = [[0f32; 2]; 32];
            s.render(&mut out);
            for o in out {
                p[0] = p[0].max(o[0].abs());
                p[1] = p[1].max(o[1].abs());
            }
        }
        p
    }

    #[test]
    fn rumble_matches_per_motor_levels() {
        let mut s = Synth::new();
        s.rumble(255, 0, 1.0);
        let p = peak(&mut s, 60);
        // Full heavy motor: 0.80 on the left, nothing on the right.
        assert!((p[0] - 0.80).abs() < 0.02, "{p:?}");
        assert!(p[1] < 1e-3, "{p:?}");
        let mut s = Synth::new();
        s.rumble(0, 255, 1.0);
        let p = peak(&mut s, 60);
        assert!((p[1] - 0.36).abs() < 0.02, "{p:?}");
        assert!(p[0] < 1e-3, "{p:?}");
    }

    #[test]
    fn weak_rumble_is_lifted() {
        // A quarter heavy motor: (0.25 + 0.5) / 2 * 0.80 = 0.30.
        let mut s = Synth::new();
        s.rumble(64, 0, 1.0);
        let p = peak(&mut s, 60);
        assert!((p[0] - 0.30).abs() < 0.02, "{p:?}");
    }

    #[test]
    fn rumble_fades_out() {
        let mut s = Synth::new();
        s.rumble(200, 200, 1.0);
        peak(&mut s, 30);
        s.rumble(0, 0, 1.0);
        // 60 ms release: well under 1% after half a second.
        peak(&mut s, 50);
        let p = peak(&mut s, 2);
        assert!(p[0] < 0.01 && p[1] < 0.01, "{p:?}");
        assert!(!s.active());
    }
}
