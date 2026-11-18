//! Feature reports the virtual DualSense answers with: the ones the physical
//! controller driving it gave when it connected, so a game sees that
//! controller's own calibration, pairing address, and firmware. Before any
//! controller has connected, or when it wouldn't give them, calibration,
//! pairing and firmware are neutral values built here; other reports are
//! declined. A game that never gets the firmware report (FFXIV asks for it
//! every half second) doesn't take the pad at all.
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
        let mut m = Mirror::default();
        m.fill_missing(get);
        m
    }

    /// Reads the reports still missing. Returns whether any came.
    pub fn fill_missing(&mut self, get: impl Fn(u8, usize) -> Option<Vec<u8>>) -> bool {
        let take = |id: u8, len: usize| {
            get(id, len)
                .filter(|b| b.len() >= len && b[0] == id)
                .map(|b| b[..len].to_vec())
        };
        let mut got = false;
        for (slot, id, len) in [
            (&mut self.calibration, 0x05, CALIBRATION_LEN),
            (&mut self.pairing, 0x09, PAIRING_LEN),
            (&mut self.firmware, 0x20, FIRMWARE_LEN),
        ] {
            if slot.is_none() {
                *slot = take(id, len);
                got |= slot.is_some();
            }
        }
        got
    }

    /// Takes the reports `other` has and this one doesn't. Returns whether
    /// any were taken.
    pub fn fill_from(&mut self, other: &Mirror) -> bool {
        let mut got = false;
        for (mine, theirs) in [
            (&mut self.calibration, &other.calibration),
            (&mut self.pairing, &other.pairing),
            (&mut self.firmware, &other.firmware),
        ] {
            if mine.is_none() && theirs.is_some() {
                *mine = theirs.clone();
                got = true;
            }
        }
        got
    }

    /// Whether `id` is one of the mirrored reports and the controller hasn't
    /// given it.
    pub fn lacks(&self, id: u8) -> bool {
        match id {
            0x05 => self.calibration.is_none(),
            0x09 => self.pairing.is_none(),
            0x20 => self.firmware.is_none(),
            _ => false,
        }
    }

    /// Whether any mirrored report is missing.
    pub fn incomplete(&self) -> bool {
        [0x05, 0x09, 0x20].iter().any(|&id| self.lacks(id))
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
            0x09 => Some(
                self.pairing
                    .clone()
                    .unwrap_or_else(|| pairing_report(&OWN_ADDRESS)),
            ),
            0x20 => {
                let mut v = self.firmware.clone().unwrap_or_else(neutral_firmware);
                let upd = u16::from_le_bytes([v[44], v[45]]);
                if upd < MIN_UPDATE_VERSION {
                    v[44..46].copy_from_slice(&MIN_UPDATE_VERSION.to_le_bytes());
                }
                Some(v)
            }
            _ => None,
        }
    }
}

/// A pairing report `0x09` with `address` (in transmission order).
pub fn pairing_report(address: &[u8; 6]) -> Vec<u8> {
    let mut v = vec![0u8; PAIRING_LEN];
    v[0] = 0x09;
    for (i, b) in address.iter().rev().enumerate() {
        v[1 + i] = *b;
    }
    v
}

/// A firmware report for a pad whose controller wouldn't give one: report
/// id and nothing else, so its update version reads as
/// [`MIN_UPDATE_VERSION`] once [`Mirror::answer`] raises it.
pub fn neutral_firmware() -> Vec<u8> {
    let mut v = vec![0u8; FIRMWARE_LEN];
    v[0] = 0x20;
    v
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
        assert!(m.lacks(0x20) && !m.lacks(0x05) && !m.lacks(0x81));
        let fw = m.answer(0x20).expect("a stand-in when not read");
        assert_eq!(fw.len(), FIRMWARE_LEN);
        assert_eq!(fw[0], 0x20);
        assert_eq!(&fw[44..46], &[0x24, 0x02]);
        assert_eq!(m.answer(0x81), None);
    }

    #[test]
    fn fills_only_what_is_missing() {
        let mut m = Mirror::read(|id, len| {
            let mut v = vec![1u8; len];
            v[0] = id;
            (id == 0x09).then_some(v)
        });
        assert!(m.incomplete());
        let got = m.fill_missing(|id, len| {
            let mut v = vec![2u8; len];
            v[0] = id;
            Some(v)
        });
        assert!(got);
        assert!(!m.incomplete());
        assert_eq!(m.pairing.as_ref().unwrap()[1], 1, "kept what it had");
        assert_eq!(m.calibration.as_ref().unwrap()[1], 2);
        assert_eq!(m.firmware.as_ref().unwrap()[1], 2);
        assert!(!m.fill_missing(|_, _| panic!("nothing left to read")));

        let mut route = Mirror::default();
        assert!(route.fill_from(&m));
        assert!(!route.incomplete());
        assert!(!route.fill_from(&m));
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
