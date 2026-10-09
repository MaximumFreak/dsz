//! Filters and the per-window resampler.

use std::f32::consts::PI;

/// RBJ biquad, transposed direct form II.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn from(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Biquad {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub fn lowpass(fs: f32, f: f32, q: f32) -> Self {
        let w = 2.0 * PI * (f / fs).clamp(1e-5, 0.49);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        Self::from(
            (1.0 - c) / 2.0,
            1.0 - c,
            (1.0 - c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        )
    }

    pub fn highpass(fs: f32, f: f32, q: f32) -> Self {
        let w = 2.0 * PI * (f / fs).clamp(1e-5, 0.49);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        Self::from(
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        )
    }

    /// Keep the state, change the coefficients (no click on a slider move).
    pub fn retune(&mut self, other: Biquad) {
        let (z1, z2) = (self.z1, self.z2);
        *self = other;
        self.z1 = z1;
        self.z2 = z2;
    }

    #[inline]
    pub fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// 4th-order Butterworth low-pass (two biquads) + 2nd-order high-pass, per
/// channel. Removes everything the actuators can't play before the signal
/// is decimated to 3 kHz, so the controller does not buzz with aliasing.
pub struct HapticFilter {
    fs: f32,
    lp_hz: f32,
    hp_hz: f32,
    lp: [[Biquad; 2]; 2],
    hp: [Biquad; 2],
}

impl HapticFilter {
    pub fn new(fs: f32, lp_hz: f32, hp_hz: f32) -> Self {
        let lp1 = Biquad::lowpass(fs, lp_hz, 0.541_196_1);
        let lp2 = Biquad::lowpass(fs, lp_hz, 1.306_563);
        let hp = Biquad::highpass(fs, hp_hz, 0.707);
        HapticFilter {
            fs,
            lp_hz,
            hp_hz,
            lp: [[lp1, lp2], [lp1, lp2]],
            hp: [hp, hp],
        }
    }

    pub fn configure(&mut self, fs: f32, lp_hz: f32, hp_hz: f32) {
        if (fs - self.fs).abs() > 1.0 {
            *self = Self::new(fs, lp_hz, hp_hz);
            return;
        }
        if (lp_hz - self.lp_hz).abs() > 0.5 || (hp_hz - self.hp_hz).abs() > 0.5 {
            let lp1 = Biquad::lowpass(fs, lp_hz, 0.541_196_1);
            let lp2 = Biquad::lowpass(fs, lp_hz, 1.306_563);
            let hp = Biquad::highpass(fs, hp_hz, 0.707);
            for ch in 0..2 {
                self.lp[ch][0].retune(lp1);
                self.lp[ch][1].retune(lp2);
                self.hp[ch].retune(hp);
            }
            self.lp_hz = lp_hz;
            self.hp_hz = hp_hz;
        }
    }

    #[inline]
    pub fn run(&mut self, ch: usize, x: f32) -> f32 {
        let y = self.hp[ch].run(x);
        let y = self.lp[ch][0].run(y);
        self.lp[ch][1].run(y)
    }
}

/// Linear interpolation of one window of input onto `out.len()` output
/// frames, continuous with the previous window through `prev`.
pub fn resample_window(prev: &mut [f32; 2], input: &[[f32; 2]], out: &mut [[f32; 2]]) {
    let m = out.len();
    if input.is_empty() {
        out.fill([0.0; 2]);
        *prev = [0.0; 2];
        return;
    }
    let l = input.len() as f32;
    for (k, o) in out.iter_mut().enumerate() {
        // Output k lands at input position (k+1)*L/M - 1; -1 is `prev`.
        let pos = (k as f32 + 1.0) * l / m as f32 - 1.0;
        let i0 = pos.floor();
        let t = pos - i0;
        let i0 = i0 as isize;
        let a = if i0 < 0 {
            *prev
        } else {
            input[(i0 as usize).min(input.len() - 1)]
        };
        let b = input[((i0 + 1).max(0) as usize).min(input.len() - 1)];
        *o = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    }
    *prev = *input.last().unwrap();
}

/// Soft knee limiter: linear to 0.7, then compresses smoothly toward 1.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.7 {
        x
    } else {
        let over = a - 0.7;
        let y = 0.7 + 0.3 * (over / (over + 0.3));
        y.copysign(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_counts() {
        let input: Vec<[f32; 2]> = (0..512).map(|i| [i as f32, -(i as f32)]).collect();
        let mut out = [[0.0; 2]; 32];
        let mut prev = [0.0; 2];
        resample_window(&mut prev, &input, &mut out);
        assert!((out[31][0] - 511.0).abs() < 1e-3);
        assert!((out[0][0] - 15.0).abs() < 1e-3);
        assert_eq!(prev, [511.0, -511.0]);
    }

    #[test]
    fn lowpass_attenuates() {
        let fs = 48_000.0;
        let mut f = HapticFilter::new(fs, 500.0, 20.0);
        let mut peak = 0f32;
        for i in 0..48_000 {
            let x = (2.0 * PI * 8000.0 * i as f32 / fs).sin();
            let y = f.run(0, x);
            if i > 1000 {
                peak = peak.max(y.abs());
            }
        }
        assert!(peak < 0.01, "8 kHz leaked: {peak}");
    }

    #[test]
    fn clip_is_bounded() {
        for i in -100..100 {
            let x = i as f32 / 10.0;
            assert!(soft_clip(x).abs() < 1.0);
        }
        assert_eq!(soft_clip(0.5), 0.5);
    }
}
