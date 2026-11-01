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

/// Streaming 16:15 resampler for the controller speaker: 1024 frames of
/// 48 kHz audio in, 960 out per report, because the controller plays each
/// report's two 10 ms Opus frames over one 21.3 ms haptics tick (measured
/// through its own microphone: a 440 Hz tone sent unsqueezed comes back at
/// 412.5 Hz). Windowed sinc (Kaiser, 32 taps, cutoff 20.5 kHz), so the
/// highs stay clean, unlike linear interpolation.
pub struct Squeeze {
    /// Input not yet fully used; `buf[0]` is input frame `base`.
    buf: Vec<[f32; 2]>,
    base: u64,
    /// Next output frame.
    n: u64,
    /// One normalized kernel per phase.
    table: [[f32; 2 * SQUEEZE_HALF]; SQUEEZE_DEN as usize],
}

const SQUEEZE_NUM: u64 = 16;
const SQUEEZE_DEN: u64 = 15;
const SQUEEZE_HALF: usize = 16;

impl Squeeze {
    pub fn new() -> Squeeze {
        // Kaiser window, beta 8; I0 by its series.
        let i0 = |x: f64| {
            let (mut sum, mut term) = (1.0, 1.0);
            for k in 1..30 {
                term *= (x / 2.0) / k as f64;
                sum += term * term;
            }
            sum
        };
        let beta = 8.0;
        let fc = 20_500.0 / 48_000.0;
        let h = SQUEEZE_HALF as f64;
        let mut table = [[0f32; 2 * SQUEEZE_HALF]; SQUEEZE_DEN as usize];
        for (p, row) in table.iter_mut().enumerate() {
            let frac = p as f64 / SQUEEZE_DEN as f64;
            let mut taps = [0f64; 2 * SQUEEZE_HALF];
            for (k, t) in taps.iter_mut().enumerate() {
                // Tap k is input frame i - HALF + 1 + k; distance from the
                // output instant i + frac.
                let x = (k as f64 - h + 1.0) - frac;
                let sinc = if x.abs() < 1e-9 {
                    1.0
                } else {
                    (std::f64::consts::PI * 2.0 * fc * x).sin() / (std::f64::consts::PI * 2.0 * fc * x)
                };
                let r = (x / h).clamp(-1.0, 1.0);
                *t = sinc * i0(beta * (1.0 - r * r).sqrt()) / i0(beta);
            }
            let sum: f64 = taps.iter().sum();
            for (o, t) in row.iter_mut().zip(taps.iter()) {
                *o = (*t / sum) as f32;
            }
        }
        // Start with a kernel's worth of silence (less two frames), so every
        // 1024 frames in give exactly 960 out from the first call.
        Squeeze {
            buf: vec![[0.0; 2]; 2 * SQUEEZE_HALF - 2],
            base: 0,
            n: 0,
            table,
        }
    }

    /// Push `input` and append every output frame it completes to `out`.
    pub fn run(&mut self, input: &[[f32; 2]], out: &mut Vec<[f32; 2]>) {
        self.buf.extend_from_slice(input);
        let end = self.base + self.buf.len() as u64;
        loop {
            let pos = self.n * SQUEEZE_NUM;
            let i = pos / SQUEEZE_DEN + SQUEEZE_HALF as u64 - 1;
            if i + SQUEEZE_HALF as u64 >= end {
                break;
            }
            let first = (i + 1 - SQUEEZE_HALF as u64 - self.base) as usize;
            let row = &self.table[(pos % SQUEEZE_DEN) as usize];
            let mut acc = [0f32; 2];
            for (t, f) in row.iter().zip(&self.buf[first..first + 2 * SQUEEZE_HALF]) {
                acc[0] += t * f[0];
                acc[1] += t * f[1];
            }
            out.push(acc);
            self.n += 1;
        }
        // Drop input no later output needs.
        let need = self.n * SQUEEZE_NUM / SQUEEZE_DEN;
        let drop = need.saturating_sub(self.base).min(self.buf.len() as u64) as usize;
        self.buf.drain(..drop);
        self.base += drop as u64;
    }
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

/// Peak limiter for a level boosted past 100 %: quiet sound gets the whole
/// boost, loud sound is held at the ceiling instead of being squashed by
/// `soft_clip`. The gain drops at once, so no sample passes the ceiling,
/// and recovers smoothly, so it doesn't pump on every peak. Both channels
/// share one gain, which keeps the balance.
pub struct Limiter {
    ceiling: f32,
    release: f32,
    gain: f32,
}

impl Limiter {
    pub fn new(rate: f32, release_secs: f32, ceiling: f32) -> Self {
        Limiter {
            ceiling,
            release: 1.0 - (-1.0 / (rate * release_secs)).exp(),
            gain: 1.0,
        }
    }

    pub fn run(&mut self, frames: &mut [[f32; 2]]) {
        for f in frames.iter_mut() {
            let peak = f[0].abs().max(f[1].abs());
            let want = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };
            self.gain = if want < self.gain {
                want
            } else {
                self.gain + (want - self.gain) * self.release
            };
            f[0] *= self.gain;
            f[1] *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_holds_the_ceiling_and_recovers() {
        let mut lim = Limiter::new(3000.0, 0.08, 0.9);
        // Quiet sound passes untouched.
        let mut quiet = [[0.3, -0.2]; 64];
        lim.run(&mut quiet);
        assert_eq!(quiet[63], [0.3, -0.2]);
        // A loud burst never passes the ceiling, on either channel, and
        // both channels drop together.
        let mut loud: Vec<[f32; 2]> = (0..300).map(|i| [((i as f32) * 0.4).sin() * 2.0, 0.5]).collect();
        lim.run(&mut loud);
        assert!(loud.iter().all(|f| f[0].abs() <= 0.9 + 1e-6 && f[1].abs() <= 0.9 + 1e-6));
        assert!(loud[299][1] < 0.5, "the quiet channel follows the shared gain");
        // Afterwards the gain comes back up over a few release times.
        let mut after = vec![[0.3, 0.3]; 3000];
        lim.run(&mut after);
        assert!(after[0][0] < 0.3);
        assert!((after[2999][0] - 0.3).abs() < 1e-3);
        // NaN neither sticks nor spreads to the gain.
        let mut odd = [[f32::NAN, 0.0], [0.3, 0.3]];
        lim.run(&mut odd);
        assert!(odd[1][0].is_finite());
    }

    #[test]
    fn squeeze_gives_960_per_1024_and_keeps_the_tone() {
        let mut sq = Squeeze::new();
        let mut out = Vec::new();
        let mut t = 0u64;
        for k in 0..20 {
            let input: Vec<[f32; 2]> = (0..1024)
                .map(|_| {
                    let v = (t as f32 * 1000.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                    t += 1;
                    [v, v]
                })
                .collect();
            let before = out.len();
            sq.run(&input, &mut out);
            assert_eq!(out.len() - before, 960, "report {k}");
        }
        // Played at 45 kHz the tone is still 1 kHz: 45 samples a cycle.
        let x: Vec<f32> = out[2000..].iter().map(|f| f[0]).collect();
        let power = |hz: f32| {
            let w = std::f32::consts::TAU * hz / 45_000.0;
            let (re, im) = x.iter().enumerate().fold((0f32, 0f32), |(re, im), (i, v)| {
                (re + v * (w * i as f32).cos(), im - v * (w * i as f32).sin())
            });
            (re * re + im * im).sqrt() / x.len() as f32
        };
        assert!(power(1000.0) > 0.2, "{}", power(1000.0));
        assert!(power(1066.7) < 0.02);
        // Amplitude kept (the kernel is normalized).
        let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
    }

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
