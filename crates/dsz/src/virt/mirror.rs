//! Feature reports the virtual DualSense answers with: the ones the physical
//! controller driving it gave when it connected, so a game sees that
//! controller's own calibration, pairing address, and firmware. Before any
//! controller has connected, calibration and pairing are neutral values
//! built here; other reports are declined.
//!
//! Layouts follow Linux's `hid-playstation` driver: calibration `0x05`
//! (41 bytes), pairing info `0x09` (20 bytes, address at 1..7, least
//! significant byte first), firmware info `0x20` (64 bytes).
//!
//! The firmware's update version (bytes 44..46) is raised to at least 2.24.
//! The virtual pad is a plain DualSense, and SDL halves rumble for one on
//! firmware below 2.24. An Edge's own numbering (2.17 and up) reads as old,
//! and the app converts rumble itself, so games should send it at full scale.

/// Feature reports read from the physical controller, report id first.
#[derive(Clone, Debug, Default)]
pub struct Mirror {
    pub calibration: Option<Vec<u8>>,
    pub pairing: Option<Vec<u8>>,
    pub firmware: Option<Vec<u8>>,
}

pub const CALIBRATION_LEN: usize = 41;
pub const PAIRING_LEN: usize = 20;
pub const FIRMWARE_LEN: usize = 64;

/// Lowest update version the virtual DualSense reports: 2.24, the first
/// with improved rumble emulation.
const MIN_UPDATE_VERSION: u16 = 0x0224;

/// Address for the virtual pad before a controller connects: locally
/// administered, "DSZ" in the low bytes.
const OWN_ADDRESS: [u8; 6] = [0x02, 0x00, 0x00, b'D', b'S', b'Z'];

impl Mirror {
    /// Reads what can be read; a report the controller won't give stays `None`.
    pub fn read(get: impl Fn(u8, usize) -> Option<Vec<u8>>) -> Mirror {
        let take = |id: u8, len: usize| get(id, len).filter(|b| b.len() >= len && b[0] == id);
        Mirror {
            calibration: take(0x05, CALIBRATION_LEN).map(|b| b[..CALIBRATION_LEN].to_vec()),
            pairing: take(0x09, PAIRING_LEN).map(|b| b[..PAIRING_LEN].to_vec()),
            firmware: take(0x20, FIRMWARE_LEN).map(|b| b[..FIRMWARE_LEN].to_vec()),
        }
    }

    /// Answer to `GET_REPORT` for a DualSense feature report, or `None` to
    /// decline it.
    pub fn answer(&self, id: u8) -> Option<Vec<u8>> {
        match id {
            0x05 => Some(
                self.calibration
                    .clone()
                    .unwrap_or_else(|| neutral_calibration(0x05, CALIBRATION_LEN)),
            ),
            0x09 => Some(self.pairing.clone().unwrap_or_else(|| {
                let mut v = vec![0u8; PAIRING_LEN];
                v[0] = 0x09;
                for (i, b) in OWN_ADDRESS.iter().rev().enumerate() {
                    v[1 + i] = *b;
                }
                v
            })),
            0x20 => self.firmware.clone().map(|mut v| {
                let upd = u16::from_le_bytes([v[44], v[45]]);
                if upd < MIN_UPDATE_VERSION {
                    v[44..46].copy_from_slice(&MIN_UPDATE_VERSION.to_le_bytes());
                }
                v
            }),
            _ => None,
        }
    }
}

/// A calibration report with no gyro bias, ±8192 counts for both the gyro
/// and the accelerometer ranges, and a gyro speed of 540 per range: the
/// layout Linux reads (pitch, yaw, roll bias; gyro plus and minus per axis;
/// gyro speed plus and minus; accelerometer plus and minus per axis).
pub fn neutral_calibration(id: u8, len: usize) -> Vec<u8> {
    const VALUES: [i16; 17] = [
        0, 0, 0, 8192, -8192, 8192, -8192, 8192, -8192, 540, 540, 8192, -8192, 8192, -8192,
        8192, -8192,
    ];
    let mut v = vec![0u8; len];
    v[0] = id;
    for (i, x) in VALUES.iter().enumerate() {
        v[1 + 2 * i..3 + 2 * i].copy_from_slice(&x.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrors_what_the_controller_gave() {
        let m = Mirror::read(|id, len| {
            let mut v = vec![7u8; len];
            v[0] = id;
            (id != 0x20).then_some(v)
        });
        assert_eq!(m.answer(0x05).unwrap()[1], 7);
        assert_eq!(m.answer(0x09).unwrap()[1], 7);
        assert_eq!(m.answer(0x20), None, "declined when not read");
        assert_eq!(m.answer(0x81), None);
    }

    #[test]
    fn firmware_reads_as_improved_rumble() {
        let fw = |upd: u16| {
            Mirror::read(|id, len| {
                let mut v = vec![0u8; len];
                v[0] = id;
                if len >= 46 {
                    v[44..46].copy_from_slice(&upd.to_le_bytes());
                }
                Some(v)
            })
            .answer(0x20)
            .unwrap()
        };
        // An Edge on 2.17 is raised; newer firmware is left alone.
        assert_eq!(&fw(0x0217)[44..46], &[0x24, 0x02]);
        assert_eq!(&fw(0x0312)[44..46], &[0x12, 0x03]);
    }

    #[test]
    fn neutral_before_a_controller_connects() {
        let m = Mirror::default();
        let c = m.answer(0x05).unwrap();
        assert_eq!(c.len(), CALIBRATION_LEN);
        assert_eq!(i16::from_le_bytes([c[7], c[8]]), 8192);
        let p = m.answer(0x09).unwrap();
        assert_eq!(&p[1..4], b"ZSD");
    }
}
