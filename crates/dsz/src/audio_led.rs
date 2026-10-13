//! Loudness to a lightbar colour. Capture stays in `haptics::audio`.

/// How fast a falling peak eases toward silence. A rise is immediate.
pub const RELEASE_SECS: f32 = 0.15;

/// Peak absolute sample across stereo frames. Non-finite samples are skipped.
pub fn peak_of(frames: &[[f32; 2]]) -> f32 {
    let mut peak = 0.0f32;
    for &[l, r] in frames {
        if l.is_finite() {
            peak = peak.max(l.abs());
        }
        if r.is_finite() {
            peak = peak.max(r.abs());
        }
    }
    peak
}

/// Move `prev` toward `sample`. Rises snap; falls use a one-pole release.
pub fn follow(prev: f32, sample: f32, dt: f32, release: f32) -> f32 {
    let sample = if sample.is_finite() {
        sample.max(0.0)
    } else {
        0.0
    };
    let prev = if prev.is_finite() { prev.max(0.0) } else { 0.0 };
    if !dt.is_finite() || dt <= 0.0 || !release.is_finite() || release <= 0.0 || sample >= prev {
        return sample;
    }
    let k = (-dt / release).exp();
    if !k.is_finite() {
        return sample;
    }
    prev * k + sample * (1.0 - k)
}

/// Level after one capture buffer. An empty buffer leaves `prev` alone.
pub fn next_level(prev: f32, frames: &[[f32; 2]], rate: u32) -> f32 {
    if frames.is_empty() {
        return if prev.is_finite() { prev.max(0.0) } else { 0.0 };
    }
    let dt = frames.len() as f32 / rate.max(1) as f32;
    follow(prev, peak_of(frames), dt, RELEASE_SECS)
}

/// Longest wait between capture buffers that still counts as playing.
/// Buffers normally come every 10 ms.
pub const GAP_SECS: f32 = 0.05;

/// The level `quiet_secs` after the last capture buffer. Loopback capture
/// sends nothing while nothing plays, so past [`GAP_SECS`] the level falls
/// as if silence had arrived.
pub fn after_gap(level: f32, quiet_secs: f32) -> f32 {
    let level = if level.is_finite() { level.max(0.0) } else { 0.0 };
    if !quiet_secs.is_finite() || quiet_secs <= GAP_SECS {
        return level;
    }
    follow(level, 0.0, quiet_secs - GAP_SECS, RELEASE_SECS)
}

/// Band colour scaled by profile brightness (0..=100), same rounding as the other modes.
pub fn paint(
    level: f32,
    quiet: [u8; 3],
    medium: [u8; 3],
    loud: [u8; 3],
    medium_at: u8,
    loud_at: u8,
    brightness: u8,
) -> [u8; 3] {
    let c = band_color(level, quiet, medium, loud, medium_at, loud_at);
    let k = f32::from(brightness.min(100)) / 100.0;
    [
        (f32::from(c[0]) * k).round() as u8,
        (f32::from(c[1]) * k).round() as u8,
        (f32::from(c[2]) * k).round() as u8,
    ]
}

/// Quiet / medium / loud. Thresholds are percent of full scale.
///
/// A loud threshold below the medium one becomes a single boundary: quiet
/// below it, loud at and above it.
pub fn band_color(
    level: f32,
    quiet: [u8; 3],
    medium: [u8; 3],
    loud: [u8; 3],
    medium_at: u8,
    loud_at: u8,
) -> [u8; 3] {
    let level = if level.is_finite() {
        level.max(0.0)
    } else {
        0.0
    };
    let low = f32::from(medium_at.min(100)) / 100.0;
    let high = f32::from(loud_at.min(100)) / 100.0;
    let high = high.max(low);
    if level < low {
        quiet
    } else if high > low && level < high {
        medium
    } else {
        loud
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_uses_either_channel_and_skips_nan() {
        assert_eq!(peak_of(&[]), 0.0);
        assert_eq!(peak_of(&[[0.1, -0.4], [0.2, 0.05]]), 0.4);
        let p = peak_of(&[[f32::NAN, 0.25], [f32::INFINITY, -0.1]]);
        assert!((p - 0.25).abs() < 1e-6);
    }

    #[test]
    fn attack_is_instant_and_release_eases() {
        assert!((follow(0.1, 0.8, 0.01, RELEASE_SECS) - 0.8).abs() < 1e-6);
        let dt = 0.01;
        let k = (-dt / RELEASE_SECS).exp();
        let fell = follow(1.0, 0.0, dt, RELEASE_SECS);
        assert!((fell - k).abs() < 1e-5);
        assert!(fell < 1.0 && fell > 0.9);
        // A non-finite sample counts as silence and releases, it does not snap.
        let nan = follow(0.4, f32::NAN, dt, RELEASE_SECS);
        let toward = follow(0.4, 0.0, dt, RELEASE_SECS);
        assert!((nan - toward).abs() < 1e-6);
    }

    #[test]
    fn empty_buffer_holds_the_level() {
        let frames = [[0.0, 0.9], [0.2, -0.3]];
        let up = next_level(0.0, &frames, 48_000);
        assert!((up - 0.9).abs() < 1e-6);
        assert!((next_level(up, &[], 48_000) - up).abs() < 1e-6);
        let quiet = next_level(up, &[[0.0, 0.0]; 480], 48_000);
        assert!(quiet < up && quiet > 0.5);
    }

    #[test]
    fn level_falls_when_capture_stops() {
        // Between buffers: held, so a band edge doesn't flicker.
        assert_eq!(after_gap(0.6, 0.0), 0.6);
        assert_eq!(after_gap(0.6, GAP_SECS), 0.6);
        // Paused: it eases to silence instead of sticking.
        let later = after_gap(0.6, GAP_SECS + RELEASE_SECS);
        assert!(later < 0.6 * 0.4 && later > 0.0);
        assert!(after_gap(0.6, 5.0) < 0.001);
        assert_eq!(after_gap(f32::NAN, 1.0), 0.0);
    }

    #[test]
    fn bands_switch_on_the_thresholds() {
        let q = [0, 1, 0];
        let m = [0, 0, 1];
        let l = [1, 0, 0];
        let at = |level: f32| band_color(level, q, m, l, 12, 35);
        assert_eq!(at(0.0), q);
        assert_eq!(at(0.119), q);
        assert_eq!(at(0.12), m);
        assert_eq!(at(0.349), m);
        assert_eq!(at(0.35), l);
        assert_eq!(at(2.0), l);
        assert_eq!(band_color(f32::NAN, q, m, l, 12, 35), q);
        // Loud below medium: one boundary, medium never shows.
        assert_eq!(band_color(0.59, q, m, l, 60, 20), q);
        assert_eq!(band_color(0.60, q, m, l, 60, 20), l);
    }

    #[test]
    fn brightness_scales_the_chosen_band() {
        let q = [40, 200, 80];
        assert_eq!(paint(0.0, q, [0, 0, 0], [0, 0, 0], 12, 35, 100), q);
        assert_eq!(
            paint(0.0, q, [0, 0, 0], [0, 0, 0], 12, 35, 50),
            [20, 100, 40]
        );
        assert_eq!(
            paint(0.9, q, [0, 0, 0], [255, 32, 32], 12, 35, 0),
            [0, 0, 0]
        );
    }
}
