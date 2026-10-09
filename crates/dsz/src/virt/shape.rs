//! Stick deadzones and trigger ranges for the virtual controller. All three
//! deadzone shapes work on the stick as -1..=1 per axis (see [`axis`]) and
//! report a centered stick as 128.

use crate::profile::{DeadzoneShape, StickDeadzone};

/// Apply a deadzone to one stick's raw bytes.
pub fn stick(dz: &StickDeadzone, x: u8, y: u8) -> (u8, u8) {
    // Radial and axial: `size` percent of full deflection reads centered,
    // and anything past it passes through untouched.
    let size = dz.size.min(100) as f32 / 100.0;
    match dz.shape {
        DeadzoneShape::None => (x, y),
        DeadzoneShape::Radial => {
            if axis(x).hypot(axis(y)) <= size {
                (128, 128)
            } else {
                (x, y)
            }
        }
        DeadzoneShape::Axial => {
            let one = |v: u8| if axis(v).abs() <= size { 128 } else { v };
            (one(x), one(y))
        }
        DeadzoneShape::Curve => curve(dz, x, y),
    }
}

/// Stick byte to -1..=1 (0 and 255 are the ends; 127.5 is the middle).
fn axis(v: u8) -> f32 {
    (v as f32 - 127.5) / 127.5
}

/// -1..=1 back to a stick byte; 0 lands on 128.
fn byte(x: f32) -> u8 {
    (127.5 + x.clamp(-1.0, 1.0) * 127.5).round() as u8
}

/// See `CurveDeadzone`.
fn curve(dz: &StickDeadzone, x: u8, y: u8) -> (u8, u8) {
    let c = &dz.curve;
    let (fx, fy) = (axis(x), axis(y));
    let mag = fx.hypot(fy);
    let inner = (c.inner / 100.0).clamp(0.0, 0.9);
    let outer = (c.outer / 100.0).clamp(inner + 0.05, 1.0);
    if mag <= inner {
        return (128, 128);
    }
    // 0 at the inner edge, 1 at the outer edge.
    let mut t = ((mag - inner) / (outer - inner)).min(1.0);
    // Response: an exponent from 2.5 (gentlest) through 1 to 0.4 (quickest).
    let r = c.response.clamp(-6.0, 6.0);
    let exponent = if r >= 0.0 {
        1.0 / (1.0 + r / 4.0)
    } else {
        1.0 - r / 4.0
    };
    t = t.powf(exponent);
    let lift = (c.lift / 100.0).clamp(0.0, 0.5);
    if lift > 0.0 {
        // Anti-dead zone: output starts at `lift` just past the inner zone.
        t = lift + (1.0 - lift) * t;
    }
    let out = t * (c.max_output / 100.0).clamp(0.0, 1.0);
    let (mut ux, mut uy) = (fx / mag, fy / mag);
    if c.square_corners {
        let m = ux.abs().max(uy.abs());
        ux /= m;
        uy /= m;
    }
    (byte(ux * out), byte(uy * out))
}

/// Trigger range in percent of the pull, `[start, end]`: the span between
/// them is stretched over the full 0..=255, so below `start` reads 0 and
/// past `end` reads 255.
pub fn trigger(v: u8, range: [u8; 2]) -> u8 {
    if range[0] == 0 && range[1] >= 100 {
        return v;
    }
    let lo = range[0].min(99) as f32 * 2.55;
    let hi = (range[1].clamp(1, 100) as f32 * 2.55).max(lo + 1.0);
    let t = ((v as f32 - lo) / (hi - lo)).clamp(0.0, 1.0);
    (t * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial() {
        let dz = StickDeadzone {
            shape: DeadzoneShape::Radial,
            size: 10,
            ..Default::default()
        };
        assert_eq!(stick(&dz, 130, 125), (128, 128));
        // 10 % of full deflection is about 12.75 counts from the middle.
        assert_eq!(stick(&dz, 140, 128), (128, 128));
        assert_eq!(stick(&dz, 141, 128), (141, 128));
        assert_eq!(stick(&dz, 200, 128), (200, 128));
    }

    #[test]
    fn axial() {
        let dz = StickDeadzone {
            shape: DeadzoneShape::Axial,
            size: 10,
            ..Default::default()
        };
        assert_eq!(stick(&dz, 130, 250), (128, 250));
        assert_eq!(stick(&dz, 20, 115), (20, 128));
    }

    #[test]
    fn curve_ends_and_corners() {
        let mut dz = StickDeadzone {
            shape: DeadzoneShape::Curve,
            ..Default::default()
        };
        assert_eq!(stick(&dz, 128, 128), (128, 128));
        assert_eq!(stick(&dz, 133, 128), (128, 128), "inside the inner zone");
        assert_eq!(stick(&dz, 255, 128), (255, 128));
        assert_eq!(stick(&dz, 0, 128).0, 0);
        // A diagonal on the rim reaches both ends with square corners...
        let (x, y) = stick(&dz, 218, 38);
        assert!(x >= 250 && y <= 5, "{x} {y}");
        // ...and stays on the circle without.
        dz.curve.square_corners = false;
        let (x, y) = stick(&dz, 218, 38);
        assert!((215..=222).contains(&x) && (33..=40).contains(&y), "{x} {y}");
        // A lift puts the first step past the inner zone well off center.
        dz.curve.lift = 20.0;
        let (x, _) = stick(&dz, 140, 128);
        assert!(x >= 150, "{x}");
    }

    #[test]
    fn trigger_ranges() {
        assert_eq!(trigger(100, [0, 100]), 100);
        assert_eq!(trigger(20, [10, 100]), 0);
        assert_eq!(trigger(240, [0, 90]), 255);
        assert_eq!(trigger(128, [0, 50]), 255);
        // Halfway through a 20..80 % range is half output.
        assert_eq!(trigger(128, [20, 80]), 128);
        // A range with its end at or before its start is a switch.
        assert_eq!(trigger(100, [50, 40]), 0);
        assert_eq!(trigger(130, [50, 40]), 255);
    }
}
