//! Stick reach, deadzones and trigger ranges for the virtual controller. All
//! deadzone shapes work on the stick as -1..=1 per axis (see [`axis`]) and
//! report a centered stick as 128.

use crate::profile::{DeadzoneShape, StickDeadzone, StickReach};

/// Apply a deadzone to one stick's raw bytes.
pub fn stick(dz: &StickDeadzone, x: u8, y: u8) -> (u8, u8) {
    let (x, y) = reach(&dz.reach, x, y);
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
pub(crate) fn axis(v: u8) -> f32 {
    (v as f32 - 127.5) / 127.5
}

/// -1..=1 back to a stick byte; 0 lands on 128.
fn byte(x: f32) -> u8 {
    (127.5 + x.clamp(-1.0, 1.0) * 127.5).round() as u8
}

/// Clockwise from up, in sectors rather than radians.
pub(crate) fn direction(x: f32, y: f32) -> f32 {
    (x.atan2(-y).rem_euclid(std::f32::consts::TAU)
        * 16.0 / std::f32::consts::TAU).rem_euclid(16.0)
}

pub(crate) fn reach_gain(reach: &StickReach, sector: f32) -> f32 {
    if !reach.enabled { return 1.0; }
    let sector = sector.rem_euclid(16.0);
    let i = sector.floor() as usize;
    let t = (1.0 - ((sector - i as f32) * std::f32::consts::PI).cos()) * 0.5;
    let gain = |i: usize| {
        let g = reach.gains[i];
        if g.is_finite() { g.clamp(50.0, 150.0) } else { 100.0 }
    };
    (gain(i) + (gain((i + 1) % 16) - gain(i)) * t) / 100.0
}

/// The square is intentional: raw corners must survive None and Radial.
pub(crate) fn reach(reach: &StickReach, x: u8, y: u8) -> (u8, u8) {
    if !reach.enabled || reach.gains.iter().all(|&g| g == 100.0) {
        return (x, y);
    }
    let (fx, fy) = (axis(x), axis(y));
    let gain = reach_gain(reach, direction(fx, fy));
    (byte(fx * gain), byte(fy * gain))
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
    fn default_reach_preserves_controller_reports_and_usb_identity() {
        use crate::composer::Composer;
        use crate::device::{Feedback, Overrides, Toggles};
        use crate::profile::Profile;
        use crate::virt::usbip::{descriptors as d, wire};
        use ds_proto::{stream, Connection, InputState};

        let mut bytes = Vec::new();
        let mut add = |data: &[u8]| {
            bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(data);
        };
        let mut p = Profile::default();
        let mut buf = [0u8; stream::MAX_REPORT];
        for disabled in [false, true] {
            if disabled {
                p.virtual_out.left_deadzone.reach.gains = [150.0; 16];
                p.virtual_out.right_deadzone.reach.gains = [50.0; 16];
            }
            let out = Composer::new().compose(
                &p, &InputState::default(), &Overrides::default(), &Feedback::default(),
                &Toggles::default(), false, 0.0, std::time::Instant::now(),
            );
            add(out.build(Connection::Bluetooth, &mut buf));
            add(out.build(Connection::Usb, &mut buf));
        }
        add(stream::build(&mut stream::StreamCounters::new(), &[0; stream::HAPTIC_FRAME], &mut buf));
        add(stream::build_audio(
            &mut stream::StreamCounters::new(), &[[0; stream::OPUS_FRAME]; stream::BLOCKS],
            &[[0; stream::HAPTIC_FRAME]; stream::BLOCKS], false, &mut buf,
        ));
        for (device, config, report) in [
            (&d::DEVICE[..], d::configuration(), &d::HID_REPORT_DESCRIPTOR[..]),
            (&d::x360::DEVICE[..], d::x360::configuration(), &[][..]),
            (&d::ds4::DEVICE[..], d::ds4::configuration(), d::ds4::HID_REPORT_DESCRIPTOR),
        ] {
            add(device);
            add(&config);
            add(report);
            let mut reply = Vec::new();
            wire::ret_submit(&mut reply, 1, 0, config.len() as u32, 0, None, &config);
            add(&reply);
        }
        add(&wire::op_common(wire::OP_REP_IMPORT, 0));
        add(&wire::op_common(wire::OP_REP_DEVLIST, 0));
        for id in [0x02, 0x12, 0x81, 0xA3] {
            add(&d::ds4::feature(id, None, &[0; 6]).unwrap());
        }
        // Captured from the unchanged report builders, before enabling reach.
        assert_eq!(bytes.as_slice(), include_bytes!("fixtures/reach-disabled.bin"));
    }

    #[test]
    fn neutral_reach_is_byte_exact_for_every_stick_position() {
        // Keep the pre-reach path here so a neutral feature cannot alter old profiles.
        fn previous(dz: &StickDeadzone, x: u8, y: u8) -> (u8, u8) {
            let size = dz.size.min(100) as f32 / 100.0;
            match dz.shape {
                DeadzoneShape::None => (x, y),
                DeadzoneShape::Radial => {
                    if axis(x).hypot(axis(y)) <= size { (128, 128) } else { (x, y) }
                }
                DeadzoneShape::Axial => {
                    let one = |v: u8| if axis(v).abs() <= size { 128 } else { v };
                    (one(x), one(y))
                }
                DeadzoneShape::Curve => curve(dz, x, y),
            }
        }
        for shape in [DeadzoneShape::None, DeadzoneShape::Radial, DeadzoneShape::Axial, DeadzoneShape::Curve] {
            let mut dz = StickDeadzone { shape, ..Default::default() };
            for neutral in 0..3 {
                dz.reach.enabled = neutral == 1;
                dz.reach.gains = [if neutral == 2 { 150.0 } else { 100.0 }; 16];
                for x in 0..=255 {
                    for y in 0..=255 {
                        assert_eq!(stick(&dz, x, y), previous(&dz, x, y), "{shape:?} {neutral} {x} {y}");
                    }
                }
            }
        }
    }

    #[test]
    fn reach_boosts_down_before_each_deadzone() {
        let mut dz = StickDeadzone { shape: DeadzoneShape::None, ..Default::default() };
        dz.reach.enabled = true;
        dz.reach.gains[8] = 116.0;
        assert!((axis(237) - 0.86).abs() < 0.002);
        assert!(stick(&dz, 128, 237).1 >= 254);
        for (x, y) in [(128, 0), (0, 128), (255, 128), (0, 0), (255, 0)] {
            assert_eq!(stick(&dz, x, y), (x, y));
        }
        dz.size = 95;
        dz.curve.inner = 90.0;
        for shape in [DeadzoneShape::Radial, DeadzoneShape::Axial, DeadzoneShape::Curve] {
            dz.shape = shape;
            assert!(stick(&dz, 128, 237).1 >= 254, "{shape:?}");
            dz.reach.enabled = false;
            assert_eq!(stick(&dz, 128, 237), (128, 128));
            dz.reach.enabled = true;
        }
    }

    #[test]
    fn reach_interpolation_is_smooth_and_cyclic() {
        let reach = StickReach {
            enabled: true,
            gains: std::array::from_fn(|i| 50.0 + i as f32 * 6.0),
        };
        for i in 0..16 {
            let at = i as f32;
            let expected = reach.gains[i] / 100.0;
            assert!((reach_gain(&reach, at) - expected).abs() < 0.000001);
            for eps in [-0.0001, 0.0001] {
                assert!((reach_gain(&reach, at + eps) - expected).abs() < 0.000001);
            }
            let next = reach.gains[(i + 1) % 16] / 100.0;
            assert!((reach_gain(&reach, at + 0.5) - (expected + next) * 0.5).abs() < 0.000001);
        }
        for (x, y, expected) in [(0.0, -1.0, 0.0), (1.0, 0.0, 4.0), (0.0, 1.0, 8.0), (-1.0, 0.0, 12.0)] {
            assert!((direction(x, y) - expected).abs() < 0.000001);
        }
    }

    #[test]
    fn reach_can_limit_a_direction_without_rounding_corners() {
        let mut dz = StickDeadzone { shape: DeadzoneShape::None, ..Default::default() };
        dz.reach.enabled = true;
        dz.reach.gains[8] = 75.0;
        let (_, y) = stick(&dz, 128, 255);
        assert!((axis(y) - 0.75).abs() < 0.005);
        for shape in [DeadzoneShape::None, DeadzoneShape::Radial] {
            dz.shape = shape;
            dz.reach.gains = [150.0; 16];
            for (x, y) in [(0, 0), (255, 255), (0, 255), (255, 0)] {
                assert_eq!(stick(&dz, x, y), (x, y));
            }
        }
    }

    #[test]
    fn reach_stays_finite_and_clips_each_axis_at_the_edges() {
        let mut r = StickReach { enabled: true, gains: [150.0; 16] };
        for x in 0..=255 {
            for y in 0..=255 {
                assert_eq!(reach(&r, x, y), (byte(axis(x) * 1.5), byte(axis(y) * 1.5)));
            }
        }
        for g in [f32::NAN, f32::INFINITY, -1000.0, 1000.0] {
            r.gains = [g; 16];
            for sector in 0..160 {
                assert!((0.5..=1.5).contains(&reach_gain(&r, sector as f32 / 10.0)));
            }
        }
    }

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
