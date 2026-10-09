//! Game feedback audio: the 4-channel PCM a game writes to a (virtual) USB
//! DualSense, turned into Bluetooth haptics.
//!
//! Channels are speaker left, speaker right, left actuator, right actuator,
//! 48 kHz 16-bit. Only the actuator channels are used.
//!
//! The reader keeps about two packet windows buffered: it waits for 1024
//! frames before the first read, takes 512 frames per Bluetooth packet, and
//! drops the oldest frames when more than 1536 are waiting, to
//! catch up.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const WINDOW: usize = 512;
const PRIME: usize = 2 * WINDOW;
const MAX_BACKLOG: usize = 3 * WINDOW;

/// Interleaved 4-channel 16-bit PCM at 48 kHz.
pub trait PcmSource: Send + Sync {
    fn filled_frames(&self) -> usize;
    /// Read up to `out.len() / 4` frames. Returns frames read.
    fn read(&self, out: &mut [i16]) -> usize;
    fn drop_frames(&self, n: usize);
}

/// One window of game actuator audio, scaled to -1..1.
pub struct GameWindow {
    pub haptics: Vec<[f32; 2]>,
    pub peak: f32,
}

pub struct GameReader {
    primed: bool,
    pcm: Vec<i16>,
    /// Reused for every window, so the packet clock never allocates.
    win: GameWindow,
    pub dropped: AtomicU64,
}

impl GameReader {
    pub fn new() -> Self {
        GameReader {
            primed: false,
            pcm: vec![0; WINDOW * 4],
            win: GameWindow {
                haptics: Vec::with_capacity(WINDOW),
                peak: 0.0,
            },
            dropped: AtomicU64::new(0),
        }
    }

    /// Take one packet window. `None` while priming or after an underrun.
    pub fn take(&mut self, src: &dyn PcmSource) -> Option<&GameWindow> {
        let filled = src.filled_frames();
        if !self.primed {
            if filled < PRIME {
                return None;
            }
            self.primed = true;
        }
        if filled < WINDOW {
            self.primed = false;
            return None;
        }
        if filled > MAX_BACKLOG {
            let drop = filled - PRIME;
            src.drop_frames(drop);
            self.dropped.fetch_add(drop as u64, Ordering::Relaxed);
        }
        let n = src.read(&mut self.pcm);
        let w = &mut self.win;
        w.haptics.clear();
        w.peak = 0.0;
        for f in self.pcm[..n * 4].as_chunks::<4>().0 {
            let s = |v: i16| v as f32 / 32768.0;
            let fr = [s(f[2]), s(f[3])];
            w.peak = w.peak.max(fr[0].abs()).max(fr[1].abs());
            w.haptics.push(fr);
        }
        Some(&self.win)
    }
}

/// Classic-rumble fallback when the actuators are not streaming PCM: the
/// loudness of each actuator channel drives the motor on the same side
/// (left actuator, heavy motor; right actuator, light motor).
pub struct ActuatorRumble {
    level: [f32; 2],
}

impl ActuatorRumble {
    /// A sine at full scale averages 2/pi in magnitude; this brings it to 1.
    const GAIN: f32 = std::f32::consts::FRAC_PI_2;
    /// Below this the motors stay off, so silence and noise don't hum.
    const GATE: f32 = 0.02;
    /// Share of the level kept per window (512 frames, 10.7 ms) as it falls:
    /// about 50 ms to fade, so short effects don't chatter.
    const RELEASE: f32 = 0.8;

    pub fn new() -> Self {
        ActuatorRumble { level: [0.0; 2] }
    }

    /// One window of actuator PCM in, `(heavy, light)` motor bytes out.
    pub fn convert(&mut self, haptics: &[[f32; 2]]) -> (u8, u8) {
        if haptics.is_empty() {
            return (0, 0);
        }
        let n = haptics.len() as f32;
        let mut out = [0u8; 2];
        for (c, o) in out.iter_mut().enumerate() {
            let mean = haptics.iter().map(|f| f[c].abs()).sum::<f32>() / n;
            let now = (mean * Self::GAIN).min(1.0);
            // Rises at once, falls gradually.
            self.level[c] = now.max(self.level[c] * Self::RELEASE);
            let l = self.level[c];
            // Square root: faint effects are still felt, loud ones saturate.
            *o = if l < Self::GATE {
                0
            } else {
                (l.sqrt() * 255.0).round() as u8
            };
        }
        (out[0], out[1])
    }
}

/// A synthetic game stream for checking the path end to end without a
/// virtual DualSense: a sweep on each actuator, silence on the speaker.
pub struct TestSource {
    start: Instant,
    len: Duration,
    t: parking_lot::Mutex<u64>,
}

impl TestSource {
    pub fn new(len: Duration) -> Self {
        TestSource {
            start: Instant::now(),
            len,
            t: parking_lot::Mutex::new(0),
        }
    }
    pub fn finished(&self) -> bool {
        self.start.elapsed() >= self.len
    }
}

impl PcmSource for TestSource {
    fn filled_frames(&self) -> usize {
        if self.finished() {
            0
        } else {
            MAX_BACKLOG
        }
    }
    fn read(&self, out: &mut [i16]) -> usize {
        let mut t = self.t.lock();
        let frames = out.len() / 4;
        for f in out.as_chunks_mut::<4>().0 {
            let secs = *t as f32 / 48_000.0;
            // Left actuator first, then right, each sweeping 60..240 Hz.
            let phase = (secs % 1.0) < 0.5;
            let freq = 60.0 + 180.0 * (secs % 0.5) * 2.0;
            let v = (secs * freq * std::f32::consts::TAU).sin();
            f[0] = 0;
            f[1] = 0;
            f[2] = if phase { (v * 0.8 * 32767.0) as i16 } else { 0 };
            f[3] = if phase { 0 } else { (v * 0.8 * 32767.0) as i16 };
            *t += 1;
        }
        frames
    }
    fn drop_frames(&self, _n: usize) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(parking_lot::Mutex<usize>);
    impl PcmSource for Fake {
        fn filled_frames(&self) -> usize {
            *self.0.lock()
        }
        fn read(&self, out: &mut [i16]) -> usize {
            let mut f = self.0.lock();
            let n = (out.len() / 4).min(*f);
            for fr in out[..n * 4].chunks_exact_mut(4) {
                fr.copy_from_slice(&[1000, -1000, 16384, -16384]);
            }
            *f -= n;
            n
        }
        fn drop_frames(&self, n: usize) {
            let mut f = self.0.lock();
            *f -= n.min(*f);
        }
    }

    #[test]
    fn primes_then_reads_and_trims() {
        let src = Fake(parking_lot::Mutex::new(600));
        let mut r = GameReader::new();
        assert!(r.take(&src).is_none(), "waits for two windows");
        *src.0.lock() = 4000;
        let w = r.take(&src).unwrap();
        assert_eq!(w.haptics.len(), WINDOW);
        assert!((w.haptics[0][0] - 0.5).abs() < 1e-3);
        assert!((w.haptics[0][1] + 0.5).abs() < 1e-3);
        // Backlog trimmed to two windows, then one read.
        assert_eq!(*src.0.lock(), PRIME - WINDOW);
        assert!(r.dropped.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn rumble_fallback() {
        let mut c = ActuatorRumble::new();
        assert_eq!(c.convert(&[[0.0, 0.0]; 64]), (0, 0));
        // Full-scale sine on the left, a quiet one on the right.
        let mixed: Vec<[f32; 2]> = (0..512)
            .map(|i| {
                let v = ((i as f32) * 0.3).sin();
                [v, 0.1 * v]
            })
            .collect();
        let (h, l) = c.convert(&mixed);
        assert!(h > 240, "heavy {h}");
        assert!(l > 60 && l < h, "light {l}");
        // Silence after that fades rather than cutting off.
        let (h2, _) = c.convert(&[[0.0, 0.0]; 512]);
        assert!(h2 > 0 && h2 < h, "fading {h2}");
    }
}
