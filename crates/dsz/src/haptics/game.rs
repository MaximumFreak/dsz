//! Game feedback audio: the 4-channel PCM a game writes to a (virtual) USB
//! DualSense, turned into Bluetooth haptics and controller-speaker audio.
//!
//! Channels are speaker left, speaker right, left actuator, right actuator,
//! 48 kHz 16-bit.
//!
//! The reader keeps about `PRIME` frames buffered. It waits for that much
//! before the first read, then follows the game's clock: when the backlog
//! drifts from the target it reads up to 0.5 % more or fewer frames per
//! window and resamples them to the window, which no one hears, instead of
//! dropping frames, which clicks. Only a backlog far past the target (a
//! stall) is cut. Speaker audio fades in after a gap and fades out into
//! one.
//!
//! Windows sends the audio in 10 ms URBs and holds only one or two ahead,
//! so `PRIME` is three windows: two windows' worth started every session
//! with a dropout on the first sound (issue 49).
//!
//! When the game's audio runs dry and comes back within `REFILL` (the PC
//! was too busy to deliver it in time, often while a game is still
//! loading), the target grows by a window, up to `MAX_PRIME`, so a PC that
//! drops out once keeps a deeper buffer for the rest of the connection.
//! Playback starts again once `PRIME` is back rather than the whole
//! target, so a dropout is not also a target's worth of silence, and the
//! rest of the target fills by reading up to `CATCH_UP` slower.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::dsp::resample_window;

/// Frames per window at the `0x32` haptics rate (32 samples at 3 kHz).
pub const WINDOW: usize = 512;
/// Backlog the reader aims for at first, in frames (about 32 ms), and
/// what it waits for after running dry.
const PRIME: usize = 3 * WINDOW;
/// Deepest backlog it grows to after dropouts (about 64 ms).
const MAX_PRIME: usize = 6 * WINDOW;
/// Past the target by this much the backlog is cut back to the target.
const MAX_OVER: usize = 8 * WINDOW;
/// Audio back within this long after running dry was a dropout, not the
/// game stopping its audio.
const REFILL: Duration = Duration::from_millis(250);
/// Largest read rate correction.
const MAX_SKEW: f64 = 0.005;
/// Largest slowdown while the backlog is more than a window short of the
/// target, after a dropout: fills 32 ms in about 3 s, a pitch change too
/// small to hear in game sound.
const CATCH_UP: f64 = 0.01;
/// Speaker fade length in frames (5 ms).
const FADE: usize = 240;

/// Interleaved 4-channel 16-bit PCM at 48 kHz.
pub trait PcmSource: Send + Sync {
    fn filled_frames(&self) -> usize;
    /// Read up to `out.len() / 4` frames. Returns frames read.
    fn read(&self, out: &mut [i16]) -> usize;
    fn drop_frames(&self, n: usize);
    /// Pauses in the audio arriving since the last call, which starts
    /// counting afresh.
    fn take_arrival_gaps(&self) -> ArrivalGaps {
        ArrivalGaps::default()
    }
    /// Log how the audio was delivered since the last call (when `log`),
    /// and start counting afresh.
    fn log_transport(&self, _log: bool) {}
    /// Changes whenever the game closes or reopens the stream.
    fn restarts(&self) -> u64 {
        0
    }
}

/// Times the game's audio stopped arriving for `ArrivalGaps::LATE` or
/// longer, and the longest pause (issue 49: tells a PC that stops sending
/// apart from a buffer that is too thin).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArrivalGaps {
    pub count: u64,
    pub longest: Duration,
}

impl ArrivalGaps {
    pub const LATE: Duration = Duration::from_millis(25);

    /// Count the pause between two arrivals.
    pub fn note(&mut self, pause: Duration) {
        if pause >= Self::LATE {
            self.count += 1;
            self.longest = self.longest.max(pause);
        }
    }
}

/// One window of game audio, scaled to -1..1.
pub struct GameWindow {
    /// Actuator channels as read (the window, give or take the drift
    /// correction); the caller resamples them to its rate.
    pub haptics: Vec<[f32; 2]>,
    /// Speaker channels, exactly the window long.
    pub speaker: Vec<[f32; 2]>,
    pub peak: f32,
    pub speaker_peak: f32,
}

pub struct GameReader {
    primed: bool,
    /// Backlog the reader aims for, frames.
    prime: usize,
    /// When the audio last ran dry, until it is back.
    starved_at: Option<Instant>,
    /// The source's restart count at the last read.
    restarts_seen: u64,
    /// The target grew a window when it ran dry; taken back if the audio
    /// turns out to have stopped rather than dropped out.
    grown: bool,
    /// Dropouts: the audio ran dry and came back within `REFILL`.
    pub underruns: u64,
    /// Fade the next speaker window in.
    fade_in: bool,
    pcm: Vec<i16>,
    raw_speaker: Vec<[f32; 2]>,
    prev_speaker: [f32; 2],
    /// Smoothed backlog, frames.
    fill: f64,
    /// Fractional frames owed to the next read.
    acc: f64,
    /// Reused for every window, so the packet clock never allocates.
    win: GameWindow,
    pub dropped: AtomicU64,
}

impl GameReader {
    pub fn new() -> Self {
        GameReader {
            primed: false,
            prime: PRIME,
            starved_at: None,
            restarts_seen: 0,
            grown: false,
            underruns: 0,
            fade_in: true,
            pcm: vec![0; (WINDOW + WINDOW / 64 + 2) * 4],
            raw_speaker: Vec::with_capacity(WINDOW * 2),
            prev_speaker: [0.0; 2],
            fill: PRIME as f64,
            acc: 0.0,
            win: GameWindow {
                haptics: Vec::with_capacity(WINDOW * 2),
                speaker: Vec::with_capacity(WINDOW),
                peak: 0.0,
                speaker_peak: 0.0,
            },
            dropped: AtomicU64::new(0),
        }
    }

    /// The backlog the reader aims for, in milliseconds. Growth from a
    /// dry spell not yet known to be a dropout (the audio may have closed)
    /// is left out.
    pub fn buffer_ms(&self) -> usize {
        let pending = if self.starved_at.is_some() && self.grown { WINDOW } else { 0 };
        (self.prime - pending) * 1000 / 48_000
    }

    /// Take one window of `frames` frames (at most `WINDOW`). `None` while
    /// priming or after an underrun.
    pub fn take(&mut self, src: &dyn PcmSource, frames: usize) -> Option<&GameWindow> {
        let frames = frames.min(WINDOW);
        let mut filled = src.filled_frames();
        // The game closed or reopened its audio since the last read.
        let restarts = src.restarts();
        let restarted = std::mem::replace(&mut self.restarts_seen, restarts) != restarts;
        if !self.primed {
            // Not back in time, or back as a new stream: the game stopped
            // or reopened its audio (FFXIV reopens it at once on start), so
            // this was no dropout.
            if self.starved_at.is_some_and(|at| restarted || at.elapsed() >= REFILL) {
                self.starved_at = None;
                if self.grown {
                    self.grown = false;
                    self.prime -= WINDOW;
                }
            }
            if filled < PRIME.min(self.prime) {
                return None;
            }
            self.primed = true;
            self.fade_in = true;
            self.fill = filled as f64;
            self.acc = 0.0;
            if let Some(at) = self.starved_at.take() {
                self.underruns += 1;
                if self.grown {
                    log::info!(
                        "game audio ran dry and was back {} ms later; buffering up to {} ms of it now",
                        at.elapsed().as_millis(),
                        self.prime * 1000 / 48_000
                    );
                } else {
                    log::debug!(
                        "game audio ran dry again and was back {} ms later",
                        at.elapsed().as_millis()
                    );
                }
            }
        }
        if filled > self.prime + MAX_OVER {
            let drop = filled - self.prime;
            src.drop_frames(drop);
            self.dropped.fetch_add(drop as u64, Ordering::Relaxed);
            filled = self.prime;
            self.fill = self.prime as f64;
        }
        // Follow the game's clock: read a little more while the backlog
        // grows, a little less while it shrinks.
        self.fill += (filled as f64 - self.fill) * 0.02;
        let slowest = if self.fill + (WINDOW as f64) < self.prime as f64 { CATCH_UP } else { MAX_SKEW };
        let skew = ((self.fill - self.prime as f64) / (8.0 * WINDOW as f64)).clamp(-slowest, MAX_SKEW);
        self.acc += frames as f64 * (1.0 + skew);
        let want = self.acc.floor() as usize;
        self.acc -= want as f64;

        let underrun = filled < want;
        let n = src.read(&mut self.pcm[..want.min(filled) * 4]);
        let w = &mut self.win;
        w.haptics.clear();
        w.speaker.clear();
        self.raw_speaker.clear();
        w.peak = 0.0;
        w.speaker_peak = 0.0;
        let s = |v: i16| v as f32 / 32768.0;
        for f in self.pcm[..n * 4].as_chunks::<4>().0 {
            let h = [s(f[2]), s(f[3])];
            let sp = [s(f[0]), s(f[1])];
            w.peak = w.peak.max(h[0].abs()).max(h[1].abs());
            w.speaker_peak = w.speaker_peak.max(sp[0].abs()).max(sp[1].abs());
            w.haptics.push(h);
            self.raw_speaker.push(sp);
        }
        if underrun {
            // Play what there is, fading to silence, then wait to prime.
            self.primed = false;
            // Refill deeper, so the restart already has the larger cushion,
            // unless the stream itself ended or started again.
            self.starved_at = (!restarted).then(Instant::now);
            self.grown = !restarted && self.prime < MAX_PRIME;
            if self.grown {
                self.prime += WINDOW;
            }
            let len = self.raw_speaker.len().max(1) as f32;
            for (k, f) in self.raw_speaker.iter_mut().enumerate() {
                let g = 1.0 - k as f32 / len;
                *f = [f[0] * g, f[1] * g];
            }
            w.speaker.extend_from_slice(&self.raw_speaker);
            w.speaker.resize(frames, [0.0; 2]);
            self.prev_speaker = [0.0; 2];
            return Some(&self.win);
        }
        w.speaker.resize(frames, [0.0; 2]);
        resample_window(&mut self.prev_speaker, &self.raw_speaker, &mut w.speaker);
        if self.fade_in {
            self.fade_in = false;
            for (k, f) in w.speaker.iter_mut().take(FADE).enumerate() {
                let g = k as f32 / FADE as f32;
                *f = [f[0] * g, f[1] * g];
            }
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

/// What a `TestSource` plays.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestSignal {
    /// A sweep on the left actuator, then the right; the speaker silent.
    Actuators,
    /// On the speaker channels, after 1 s of silence (the speaker stream
    /// starts in it): a rising C major arpeggio, then a steady A440; the
    /// actuators silent. A dropout or a click is easy to hear in
    /// it, and to find in what the controller's microphone hears.
    Speaker,
}

/// A synthetic game stream for checking the path end to end without a
/// virtual DualSense.
pub struct TestSource {
    start: Instant,
    len: Duration,
    signal: TestSignal,
    t: parking_lot::Mutex<u64>,
}

impl TestSource {
    pub fn new(len: Duration, signal: TestSignal) -> Self {
        TestSource {
            start: Instant::now(),
            len,
            signal,
            t: parking_lot::Mutex::new(0),
        }
    }
    pub fn finished(&self) -> bool {
        self.start.elapsed() >= self.len
    }
}

/// Silence before the speaker test's first note.
pub const SPEAKER_TEST_QUIET: f32 = 1.0;

/// The speaker test at `secs`, -1..1.
fn speaker_test(secs: f32) -> f32 {
    let secs = secs - SPEAKER_TEST_QUIET;
    let tone = |hz: f32, start: f32, len: f32| {
        let t = secs - start;
        if !(0.0..len).contains(&t) {
            return 0.0;
        }
        // 10 ms ramps, so the notes themselves don't click.
        let env = (t / 0.01).min((len - t) / 0.01).min(1.0);
        (t * hz * std::f32::consts::TAU).sin() * env
    };
    let mut v = 0.0;
    for (k, hz) in [523.25, 659.25, 783.99, 1046.5].into_iter().enumerate() {
        v += tone(hz, k as f32 * 0.25, 0.22);
    }
    v += tone(440.0, 1.25, 1.25);
    v * 0.5
}

impl PcmSource for TestSource {
    fn filled_frames(&self) -> usize {
        // Made on demand: enough for any target the reader has grown to.
        if self.finished() {
            0
        } else {
            MAX_PRIME
        }
    }
    fn read(&self, out: &mut [i16]) -> usize {
        let mut t = self.t.lock();
        let frames = out.len() / 4;
        for f in out.as_chunks_mut::<4>().0 {
            let secs = *t as f32 / 48_000.0;
            *f = [0; 4];
            match self.signal {
                TestSignal::Actuators => {
                    // Left actuator first, then right, each sweeping 60..240 Hz.
                    let phase = (secs % 1.0) < 0.5;
                    let freq = 60.0 + 180.0 * (secs % 0.5) * 2.0;
                    let v = ((secs * freq * std::f32::consts::TAU).sin() * 0.8 * 32767.0) as i16;
                    f[if phase { 2 } else { 3 }] = v;
                }
                TestSignal::Speaker => {
                    let v = (speaker_test(secs) * 32767.0) as i16;
                    f[0] = v;
                    f[1] = v;
                }
            }
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
                fr.copy_from_slice(&[8192, -8192, 16384, -16384]);
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
    fn primes_reads_and_cuts_a_stall() {
        let src = Fake(parking_lot::Mutex::new(600));
        let mut r = GameReader::new();
        assert!(r.take(&src, WINDOW).is_none(), "waits for the target backlog");
        *src.0.lock() = PRIME + MAX_OVER + 100;
        let w = r.take(&src, WINDOW).unwrap();
        assert!((w.haptics.len() as i64 - WINDOW as i64).abs() <= 3);
        assert_eq!(w.speaker.len(), WINDOW);
        assert!((w.haptics[0][0] - 0.5).abs() < 1e-3);
        assert!((w.haptics[0][1] + 0.5).abs() < 1e-3);
        // Faded in from silence, then the full level.
        assert!(w.speaker[0][0].abs() < 0.01);
        assert!((w.speaker[300][0] - 0.25).abs() < 1e-3);
        assert!(r.dropped.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn follows_a_growing_backlog_without_dropping() {
        let src = Fake(parking_lot::Mutex::new(PRIME));
        let mut r = GameReader::new();
        let mut read = 0usize;
        // The game delivers 0.3 % faster than the windows are taken.
        for _ in 0..3000 {
            let before = *src.0.lock();
            let w = r.take(&src, 480).unwrap();
            assert_eq!(w.speaker.len(), 480);
            read += before - *src.0.lock();
            *src.0.lock() += 481;
        }
        assert_eq!(r.dropped.load(Ordering::Relaxed), 0);
        let backlog = *src.0.lock();
        assert!(backlog < PRIME + 2 * WINDOW, "backlog {backlog}");
        assert!(read > 3000 * 480);
    }

    #[test]
    fn underrun_fades_out() {
        let src = Fake(parking_lot::Mutex::new(PRIME));
        let mut r = GameReader::new();
        r.take(&src, WINDOW).unwrap();
        *src.0.lock() = 100;
        let w = r.take(&src, WINDOW).unwrap();
        assert!(w.speaker[0][0] > 0.2);
        assert!(w.speaker[99][0].abs() < 0.01);
        assert_eq!(w.speaker[200][0], 0.0);
        assert!(r.take(&src, WINDOW).is_none(), "primes again");
    }

    #[test]
    fn a_dropout_deepens_the_buffer_and_a_closed_stream_does_not() {
        let src = Fake(parking_lot::Mutex::new(PRIME));
        let mut r = GameReader::new();
        r.take(&src, WINDOW).unwrap();
        assert_eq!(r.buffer_ms(), 32);
        // Runs dry, and the audio is back at once: one dropout, one window deeper.
        *src.0.lock() = 100;
        r.take(&src, WINDOW).unwrap();
        assert_eq!(r.buffer_ms(), 32, "not known yet to be a dropout");
        *src.0.lock() = PRIME - 1;
        assert!(r.take(&src, WINDOW).is_none(), "waits for the first target");
        *src.0.lock() = PRIME;
        r.take(&src, WINDOW).unwrap();
        assert_eq!((r.underruns, r.prime, r.buffer_ms()), (1, PRIME + WINDOW, 42));
        // It stops growing at MAX_PRIME.
        for _ in 0..10 {
            *src.0.lock() = 0;
            r.take(&src, WINDOW);
            *src.0.lock() = MAX_PRIME;
            r.take(&src, WINDOW).unwrap();
        }
        assert_eq!((r.underruns, r.prime), (11, MAX_PRIME));
        // Audio that comes back much later was the game stopping its audio.
        let mut r = GameReader::new();
        *src.0.lock() = PRIME;
        r.take(&src, WINDOW).unwrap();
        *src.0.lock() = 0;
        r.take(&src, WINDOW);
        r.starved_at = Some(Instant::now() - REFILL * 2);
        *src.0.lock() = PRIME;
        r.take(&src, WINDOW).unwrap();
        assert_eq!((r.underruns, r.prime), (0, PRIME));
    }

    #[test]
    fn after_a_dropout_it_reads_slower_until_the_buffer_is_full() {
        let src = Fake(parking_lot::Mutex::new(MAX_PRIME));
        let mut r = GameReader::new();
        r.prime = MAX_PRIME;
        r.take(&src, WINDOW).unwrap();
        // Back with only the first target buffered.
        *src.0.lock() = 0;
        r.take(&src, WINDOW);
        r.prime = MAX_PRIME;
        r.grown = false;
        *src.0.lock() = PRIME;
        let mut read = 0usize;
        let windows = 48_000 * 4 / WINDOW;
        for _ in 0..windows {
            let before = *src.0.lock();
            r.take(&src, WINDOW).expect("plays on while it fills");
            read += before - *src.0.lock();
            *src.0.lock() += WINDOW;
        }
        let backlog = *src.0.lock();
        assert!(backlog + WINDOW >= MAX_PRIME, "filled to {backlog} of {MAX_PRIME}");
        assert!(read < windows * WINDOW, "it read slower");
        assert_eq!(r.dropped.load(Ordering::Relaxed), 0);
    }

    /// FFXIV opens its audio, closes it and opens it again within 0.2 s.
    #[test]
    fn a_reopened_stream_is_no_dropout() {
        struct Reopening(Fake, std::sync::atomic::AtomicU64);
        impl PcmSource for Reopening {
            fn filled_frames(&self) -> usize {
                self.0.filled_frames()
            }
            fn read(&self, out: &mut [i16]) -> usize {
                self.0.read(out)
            }
            fn drop_frames(&self, n: usize) {
                self.0.drop_frames(n)
            }
            fn restarts(&self) -> u64 {
                self.1.load(Ordering::Relaxed)
            }
        }
        let src = Reopening(Fake(parking_lot::Mutex::new(PRIME)), Default::default());
        let mut r = GameReader::new();
        r.take(&src, WINDOW).unwrap();
        // Closed and reopened between two reads: runs dry at once.
        src.1.fetch_add(2, Ordering::Relaxed);
        *src.0 .0.lock() = 0;
        r.take(&src, WINDOW);
        *src.0 .0.lock() = PRIME;
        r.take(&src, WINDOW).unwrap();
        assert_eq!((r.underruns, r.prime), (0, PRIME));
        // Runs dry, then reopens before it is back.
        *src.0 .0.lock() = 0;
        r.take(&src, WINDOW);
        src.1.fetch_add(2, Ordering::Relaxed);
        *src.0 .0.lock() = PRIME;
        r.take(&src, WINDOW).unwrap();
        assert_eq!((r.underruns, r.prime), (0, PRIME));
    }

    #[test]
    fn the_test_source_starts_after_the_buffer_grew() {
        let src = Fake(parking_lot::Mutex::new(PRIME));
        let mut r = GameReader::new();
        r.take(&src, WINDOW).unwrap();
        for _ in 0..10 {
            *src.0.lock() = 0;
            r.take(&src, WINDOW);
            *src.0.lock() = MAX_PRIME;
            r.take(&src, WINDOW).unwrap();
        }
        assert_eq!(r.prime, MAX_PRIME);
        let test = TestSource::new(Duration::from_secs(1), TestSignal::Speaker);
        assert!(r.take(&test, WINDOW).is_some());
    }

    #[test]
    fn speaker_test_is_loud_enough_and_bounded() {
        let peak = (0..48_000 * 5)
            .map(|i| speaker_test(i as f32 / 48_000.0).abs())
            .fold(0.0f32, f32::max);
        assert!(peak > 0.4 && peak <= 1.0, "{peak}");
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
