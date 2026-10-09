//! Input report parsing.
//!
//! USB report `0x01` (64 bytes) and Bluetooth report `0x31` (78 bytes) share
//! one layout; Bluetooth is shifted by one byte. Bluetooth also has a short
//! `0x01` report that the controller sends until a feature report (for
//! example calibration `0x05`) is read; that one is DualShock-shaped.

use crate::{crc, Connection, Model};
use serde::{Deserialize, Serialize};

/// Button bit set. Bits are this crate's own numbering, not wire masks.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Buttons(pub u32);

impl std::fmt::Debug for Buttons {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = Button::ALL
            .iter()
            .filter(|b| self.has(**b))
            .map(|b| b.short_name())
            .collect();
        write!(f, "Buttons[{}]", names.join(" "))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum Button {
    Cross = 0,
    Circle,
    Square,
    Triangle,
    L1,
    R1,
    L2,
    R2,
    Create,
    Options,
    L3,
    R3,
    Ps,
    Touchpad,
    Mute,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    /// Edge only.
    FnLeft,
    /// Edge only.
    FnRight,
    /// Edge only, back paddle.
    PaddleLeft,
    /// Edge only, back paddle.
    PaddleRight,
}

impl Button {
    pub const ALL: [Button; 23] = [
        Button::Cross,
        Button::Circle,
        Button::Square,
        Button::Triangle,
        Button::L1,
        Button::R1,
        Button::L2,
        Button::R2,
        Button::Create,
        Button::Options,
        Button::L3,
        Button::R3,
        Button::Ps,
        Button::Touchpad,
        Button::Mute,
        Button::DpadUp,
        Button::DpadDown,
        Button::DpadLeft,
        Button::DpadRight,
        Button::FnLeft,
        Button::FnRight,
        Button::PaddleLeft,
        Button::PaddleRight,
    ];

    #[inline]
    pub fn bit(self) -> u32 {
        1 << (self as u8)
    }

    pub fn short_name(self) -> &'static str {
        match self {
            Button::Cross => "Cross",
            Button::Circle => "Circle",
            Button::Square => "Square",
            Button::Triangle => "Triangle",
            Button::L1 => "L1",
            Button::R1 => "R1",
            Button::L2 => "L2",
            Button::R2 => "R2",
            Button::Create => "Create",
            Button::Options => "Options",
            Button::L3 => "L3",
            Button::R3 => "R3",
            Button::Ps => "PS",
            Button::Touchpad => "Touchpad",
            Button::Mute => "Mute",
            Button::DpadUp => "D-pad Up",
            Button::DpadDown => "D-pad Down",
            Button::DpadLeft => "D-pad Left",
            Button::DpadRight => "D-pad Right",
            Button::FnLeft => "Fn Left",
            Button::FnRight => "Fn Right",
            Button::PaddleLeft => "Left Paddle",
            Button::PaddleRight => "Right Paddle",
        }
    }

    pub fn edge_only(self) -> bool {
        matches!(
            self,
            Button::FnLeft | Button::FnRight | Button::PaddleLeft | Button::PaddleRight
        )
    }
}

impl Buttons {
    #[inline]
    pub fn has(self, b: Button) -> bool {
        self.0 & b.bit() != 0
    }
    #[inline]
    pub fn set(&mut self, b: Button, on: bool) {
        if on {
            self.0 |= b.bit();
        } else {
            self.0 &= !b.bit();
        }
    }
    /// Buttons that are down in `self` and up in `prev`.
    #[inline]
    pub fn pressed_since(self, prev: Buttons) -> Buttons {
        Buttons(self.0 & !prev.0)
    }
    #[inline]
    pub fn released_since(self, prev: Buttons) -> Buttons {
        Buttons(!self.0 & prev.0)
    }
    pub fn iter(self) -> impl Iterator<Item = Button> {
        Button::ALL.into_iter().filter(move |b| self.has(*b))
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TouchPoint {
    pub active: bool,
    pub id: u8,
    /// 0..=1919
    pub x: u16,
    /// 0..=1079
    pub y: u16,
}

pub const TOUCH_W: u16 = 1920;
pub const TOUCH_H: u16 = 1080;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatteryStatus {
    #[default]
    Discharging,
    Charging,
    Full,
    Error,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InputState {
    pub lx: u8,
    pub ly: u8,
    pub rx: u8,
    pub ry: u8,
    pub l2: u8,
    pub r2: u8,
    pub buttons: Buttons,
    /// Wire order: pitch (x), yaw (y), roll (z). Raw counts, ~16 per deg/s.
    pub gyro: [i16; 3],
    /// Wire order x, y, z. 8192 counts per g.
    pub accel: [i16; 3],
    /// Sensor timestamp, units of 1/3 microsecond.
    pub sensor_ts: u32,
    /// False for the short Bluetooth report, which has no motion.
    pub motion_valid: bool,
    pub touch: [TouchPoint; 2],
    pub battery_percent: u8,
    pub battery_status: BatteryStatus,
    pub headphones: bool,
    pub mic_plugged: bool,
    pub seq: u8,
    /// True for the full report, false for the short Bluetooth one.
    pub full: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    TooShort,
    UnknownReport(u8),
    BadCrc,
}

#[inline]
fn le16(b: &[u8], i: usize) -> i16 {
    i16::from_le_bytes([b[i], b[i + 1]])
}

fn dpad(state: &mut Buttons, hat: u8) {
    let (u, r, d, l) = match hat & 0x0F {
        0 => (true, false, false, false),
        1 => (true, true, false, false),
        2 => (false, true, false, false),
        3 => (false, true, true, false),
        4 => (false, false, true, false),
        5 => (false, false, true, true),
        6 => (false, false, false, true),
        7 => (true, false, false, true),
        _ => (false, false, false, false),
    };
    state.set(Button::DpadUp, u);
    state.set(Button::DpadRight, r);
    state.set(Button::DpadDown, d);
    state.set(Button::DpadLeft, l);
}

fn face(state: &mut Buttons, b: u8) {
    state.set(Button::Square, b & 0x10 != 0);
    state.set(Button::Cross, b & 0x20 != 0);
    state.set(Button::Circle, b & 0x40 != 0);
    state.set(Button::Triangle, b & 0x80 != 0);
}

fn shoulders(state: &mut Buttons, b: u8) {
    state.set(Button::L1, b & 0x01 != 0);
    state.set(Button::R1, b & 0x02 != 0);
    state.set(Button::Create, b & 0x10 != 0);
    state.set(Button::Options, b & 0x20 != 0);
    state.set(Button::L3, b & 0x40 != 0);
    state.set(Button::R3, b & 0x80 != 0);
}

fn touch_point(b: &[u8]) -> TouchPoint {
    TouchPoint {
        active: b[0] & 0x80 == 0,
        id: b[0] & 0x7F,
        x: (b[1] as u16) | (((b[2] & 0x0F) as u16) << 8),
        y: ((b[2] >> 4) as u16) | ((b[3] as u16) << 4),
    }
}

/// Parse one input report.
///
/// `check_crc` validates the Bluetooth trailer; a corrupted radio packet is
/// dropped instead of producing a one-frame glitch.
pub fn parse(
    report: &[u8],
    conn: Connection,
    model: Model,
    check_crc: bool,
) -> Result<InputState, ParseError> {
    if report.is_empty() {
        return Err(ParseError::TooShort);
    }
    match (conn, report[0]) {
        (Connection::Bluetooth, 0x31) => {
            if report.len() < crate::BT_INPUT_LEN {
                return Err(ParseError::TooShort);
            }
            if check_crc && !crc::verify(crc::PREFIX_INPUT, &report[..crate::BT_INPUT_LEN]) {
                return Err(ParseError::BadCrc);
            }
            Ok(parse_full(report, 1, model))
        }
        (Connection::Bluetooth, 0x01) => {
            if report.len() < 10 {
                return Err(ParseError::TooShort);
            }
            Ok(parse_short(report))
        }
        (Connection::Usb, 0x01) => {
            if report.len() < 64 {
                return Err(ParseError::TooShort);
            }
            Ok(parse_full(report, 0, model))
        }
        (_, id) => Err(ParseError::UnknownReport(id)),
    }
}

fn parse_short(r: &[u8]) -> InputState {
    let mut s = InputState {
        lx: r[1],
        ly: r[2],
        rx: r[3],
        ry: r[4],
        l2: r[8],
        r2: r[9],
        full: false,
        motion_valid: false,
        battery_percent: 0,
        ..Default::default()
    };
    let mut b = Buttons::default();
    dpad(&mut b, r[5]);
    face(&mut b, r[5]);
    shoulders(&mut b, r[6]);
    b.set(Button::Ps, r[7] & 0x01 != 0);
    b.set(Button::Touchpad, r[7] & 0x02 != 0);
    b.set(Button::L2, s.l2 > 0);
    b.set(Button::R2, s.r2 > 0);
    s.buttons = b;
    s
}

fn parse_full(r: &[u8], o: usize, model: Model) -> InputState {
    let at = |i: usize| r[i + o];
    let mut s = InputState {
        lx: at(1),
        ly: at(2),
        rx: at(3),
        ry: at(4),
        l2: at(5),
        r2: at(6),
        seq: at(7),
        full: true,
        motion_valid: true,
        ..Default::default()
    };

    let mut b = Buttons::default();
    dpad(&mut b, at(8));
    face(&mut b, at(8));
    shoulders(&mut b, at(9));
    let special = at(10);
    b.set(Button::Ps, special & 0x01 != 0);
    b.set(Button::Touchpad, special & 0x02 != 0);
    b.set(Button::Mute, special & 0x04 != 0);
    // Edge function buttons and paddles, as SDL's `SDL_hidapi_ps5.c` reads them.
    if model.is_edge() {
        b.set(Button::FnLeft, special & 0x10 != 0);
        b.set(Button::FnRight, special & 0x20 != 0);
        b.set(Button::PaddleLeft, special & 0x40 != 0);
        b.set(Button::PaddleRight, special & 0x80 != 0);
    }
    // The analog byte is the button, not the digital bit.
    b.set(Button::L2, s.l2 > 0);
    b.set(Button::R2, s.r2 > 0);
    s.buttons = b;

    for (k, idx) in [16usize, 18, 20].into_iter().enumerate() {
        s.gyro[k] = le16(r, idx + o);
    }
    for (k, idx) in [22usize, 24, 26].into_iter().enumerate() {
        s.accel[k] = le16(r, idx + o);
    }
    s.sensor_ts = u32::from_le_bytes([at(28), at(29), at(30), at(31)]);

    s.touch[0] = touch_point(&r[33 + o..37 + o]);
    s.touch[1] = touch_point(&r[37 + o..41 + o]);

    let bat = at(53);
    s.battery_percent = ((bat & 0x0F) * 10).min(100);
    s.battery_status = match bat >> 4 {
        0x0 => BatteryStatus::Discharging,
        0x1 => BatteryStatus::Charging,
        0x2 => BatteryStatus::Full,
        _ => BatteryStatus::Error,
    };
    if s.battery_status == BatteryStatus::Full {
        s.battery_percent = 100;
    }
    // Linux's `DS_STATUS1_HP_DETECT` and `DS_STATUS1_MIC_DETECT`.
    let plug = at(54);
    s.headphones = plug & 0x01 != 0;
    s.mic_plugged = plug & 0x02 != 0;
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usb_report() -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x01;
        r[1] = 10;
        r[2] = 20;
        r[3] = 30;
        r[4] = 40;
        r[5] = 0;
        r[6] = 255;
        r[8] = 0x08 | 0x20; // hat released, cross
        r[9] = 0x01 | 0x20; // L1, options
        r[10] = 0x01 | 0x04; // PS, mute
        r[16] = 0x10; // gyro x = 16
        r[22] = 0x00;
        r[23] = 0x20; // accel x = 8192
        r[33] = 0x05; // finger down, id 5
        r[34] = 0x80;
        r[35] = 0x37; // x = 0x780 = 1920? use 0x7_80 -> 1920 is max+1, make 0x77F below
        r[36] = 0x21;
        r[37] = 0x80; // second finger up
        r[53] = 0x17; // charging, 70%
        r
    }

    #[test]
    fn usb_basic() {
        let mut r = usb_report();
        r[34] = 0x7F;
        r[35] = 0x37; // x = 0x77F = 1919, y low nibble 3
        let s = parse(&r, Connection::Usb, Model::DualSense, false).unwrap();
        assert_eq!((s.lx, s.ly, s.rx, s.ry), (10, 20, 30, 40));
        assert!(s.buttons.has(Button::Cross));
        assert!(s.buttons.has(Button::L1));
        assert!(s.buttons.has(Button::Options));
        assert!(s.buttons.has(Button::Ps));
        assert!(s.buttons.has(Button::Mute));
        assert!(s.buttons.has(Button::R2));
        assert!(!s.buttons.has(Button::L2));
        assert!(!s.buttons.has(Button::DpadUp));
        assert_eq!(s.gyro[0], 16);
        assert_eq!(s.accel[0], 8192);
        assert!(s.touch[0].active);
        assert_eq!(s.touch[0].id, 5);
        assert_eq!(s.touch[0].x, 1919);
        assert_eq!(s.touch[0].y, 3 | (0x21 << 4));
        assert!(!s.touch[1].active);
        assert_eq!(s.battery_status, BatteryStatus::Charging);
        assert_eq!(s.battery_percent, 70);
    }

    #[test]
    fn bt_is_usb_plus_one() {
        let usb = usb_report();
        let mut bt = vec![0u8; 78];
        bt[0] = 0x31;
        bt[2..2 + 63].copy_from_slice(&usb[1..64]);
        crc::seal(crc::PREFIX_INPUT, &mut bt);
        let a = parse(&usb, Connection::Usb, Model::DualSense, false).unwrap();
        let b = parse(&bt, Connection::Bluetooth, Model::DualSense, true).unwrap();
        assert_eq!(a.buttons, b.buttons);
        assert_eq!(a.gyro, b.gyro);
        assert_eq!(a.touch, b.touch);
        bt[10] ^= 0xFF;
        assert_eq!(
            parse(&bt, Connection::Bluetooth, Model::DualSense, true),
            Err(ParseError::BadCrc)
        );
    }

    #[test]
    fn edge_buttons() {
        let mut r = usb_report();
        r[10] = 0x10 | 0x80;
        let s = parse(&r, Connection::Usb, Model::DualSenseEdge, false).unwrap();
        assert!(s.buttons.has(Button::FnLeft));
        assert!(s.buttons.has(Button::PaddleRight));
        // Same bits on a plain DualSense are ignored.
        let s = parse(&r, Connection::Usb, Model::DualSense, false).unwrap();
        assert!(!s.buttons.has(Button::FnLeft));
    }

    #[test]
    fn dpad_diagonal() {
        let mut r = usb_report();
        r[8] = 0x03;
        let s = parse(&r, Connection::Usb, Model::DualSense, false).unwrap();
        assert!(s.buttons.has(Button::DpadRight) && s.buttons.has(Button::DpadDown));
    }
}
