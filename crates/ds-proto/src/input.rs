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
    /// Touchpad click with the finger in the left third of the pad.
    /// Derived from the touch point; the wire still has one touchpad bit.
    /// Appended so the earlier variants keep their bit positions.
    TouchpadLeft,
    /// Touchpad click with the finger in the center third.
    TouchpadCenter,
    /// Touchpad click with the finger in the right third.
    TouchpadRight,
}

impl Button {
    pub const ALL: [Button; 26] = [
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
        Button::TouchpadLeft,
        Button::TouchpadCenter,
        Button::TouchpadRight,
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
            Button::TouchpadLeft => "Touchpad Left",
            Button::TouchpadCenter => "Touchpad Center",
            Button::TouchpadRight => "Touchpad Right",
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

    /// Left / center / right touchpad click. These are not separate wire
    /// buttons; see [`touchpad_click_zone`].
    pub fn is_touchpad_zone(self) -> bool {
        matches!(
            self,
            Button::TouchpadLeft | Button::TouchpadCenter | Button::TouchpadRight
        )
    }

    /// Bits of the three zone buttons.
    pub const TOUCHPAD_ZONE_BITS: u32 = (1 << Button::TouchpadLeft as u8)
        | (1 << Button::TouchpadCenter as u8)
        | (1 << Button::TouchpadRight as u8);

    /// The button on the wire: Touchpad for a zone, itself otherwise.
    pub fn physical(self) -> Button {
        if self.is_touchpad_zone() {
            Button::Touchpad
        } else {
            self
        }
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

/// First x of the center third. The pad is [`TOUCH_W`] wide and the three
/// click zones are equal bands of 640: left is `x < 640`, center is
/// `640 <= x < 1280`, right is `x >= 1280`.
pub const TOUCH_ZONE_CENTER: u16 = TOUCH_W / 3;
/// First x of the right third.
pub const TOUCH_ZONE_RIGHT: u16 = 2 * TOUCH_W / 3;

/// Which third of the pad `x` is in.
pub fn touchpad_zone(x: u16) -> Button {
    if x < TOUCH_ZONE_CENTER {
        Button::TouchpadLeft
    } else if x < TOUCH_ZONE_RIGHT {
        Button::TouchpadCenter
    } else {
        Button::TouchpadRight
    }
}

fn mean_touch_x(points: &[TouchPoint; 2]) -> Option<u16> {
    let mut n = 0u32;
    let mut sum = 0u32;
    for p in points {
        if p.active {
            sum += p.x as u32;
            n += 1;
        }
    }
    (n > 0).then(|| (sum / n) as u16)
}

/// Zone button for a physical touchpad click.
///
/// The controller reports one touchpad button. The zone follows the fingers
/// on the pad: the average x of the contacts in `current`, or in `previous`
/// when the click frame itself has no finger (the finger often lifts as the
/// pad goes down). `None` when neither report has a finger, so a click with
/// nowhere to place it stays the plain Touchpad button.
pub fn touchpad_click_zone(
    current: &[TouchPoint; 2],
    previous: &[TouchPoint; 2],
) -> Option<Button> {
    mean_touch_x(current)
        .or_else(|| mean_touch_x(previous))
        .map(touchpad_zone)
}

/// True while a finger is on the third of the pad that `zone` names (one of
/// `TouchpadLeft`, `TouchpadCenter`, `TouchpadRight`), with no click needed.
pub fn touch_zone_held(touch: &[TouchPoint; 2], zone: Button) -> bool {
    touch.iter().any(|p| p.active && touchpad_zone(p.x) == zone)
}

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
    /// A Bluetooth `0x31` report that carries something other than the
    /// controller state (its type nibble is not 1): microphone audio while
    /// the controller receives speaker audio. Same id, same length, valid
    /// CRC, so the type nibble is the only way to tell.
    NotState(u8),
    /// A touch point off the pad: not a real state report.
    Implausible,
    /// Sony's USB adapter reporting that no controller is on the radio.
    /// The rest of the report is zeros and must not be used as input.
    NotConnected,
}

/// Type nibble (low four bits of byte 1) of a Bluetooth `0x31` report that
/// carries the controller state. Observed on a DualSense Edge: state
/// reports are `31 x1`; while the speaker plays, microphone packets arrive
/// as `31 x2 <counter> D4 …`.
const BT_STATE_TYPE: u8 = 0x01;

/// Reject a state whose active touch points are well off the 1920 × 1080
/// pad (a little past the edge is allowed).
fn plausible(s: InputState) -> Result<InputState, ParseError> {
    let off = |t: &TouchPoint| t.active && (t.x > 1950 || t.y > 1110);
    if s.touch.iter().any(off) {
        Err(ParseError::Implausible)
    } else {
        Ok(s)
    }
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
    if model.is_dualshock4() {
        return crate::ds4::parse_input(report, conn, model, check_crc);
    }
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
            let kind = report[1] & 0x0F;
            // 0 is accepted too, for firmware that leaves the nibble clear.
            if kind != BT_STATE_TYPE && kind != 0 {
                return Err(ParseError::NotState(kind));
            }
            plausible(parse_full(report, 1, model))
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
            plausible(parse_full(report, 0, model))
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

    fn unhex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    /// Reports captured from a DualSense Edge over Bluetooth while it played
    /// speaker audio: a state report, then a microphone packet with the
    /// same id, length and a valid CRC.
    #[test]
    fn microphone_packets_are_not_state() {
        let state = unhex("31 31 7F 82 82 7E 00 00 01 08 00 00 00 6A 26 6E B8 06 00 08 00 FB FF BF FF 0A 20 A1 05 D3 4E E2 3B 12 80 00 00 00 80 00 00 00 00 09 09 00 00 00 00 00 3C 00 08 01 04 00 00 1A 14 86 AD 81 73 D1 89 00 00 00 00 00 00 00 00 00 91 E8 B6 B1");
        let mic = unhex("31 12 00 D4 7E CE 2B EF 0E 45 AC 7B AB 01 FF DA C3 0D 66 CE 3A 3C 87 D9 95 68 7F 1F ED 42 0F B5 3B EF 67 03 CE 5E D3 8C 25 E0 C4 6F 63 3B DC C4 7C C2 F5 C1 56 7D C4 F8 AE AB A0 96 89 5B 68 94 2A DD 21 B0 13 5F 8C 3C 80 24 47 A5 39 73");
        assert!(crc::verify(crc::PREFIX_INPUT, &state));
        assert!(crc::verify(crc::PREFIX_INPUT, &mic), "the mic packet's CRC is valid");
        let s = parse(&state, Connection::Bluetooth, Model::DualSenseEdge, true).unwrap();
        assert_eq!(s.buttons.0, 0);
        assert!(!s.touch[0].active);
        assert_eq!(
            parse(&mic, Connection::Bluetooth, Model::DualSenseEdge, true),
            Err(ParseError::NotState(2))
        );
    }

    #[test]
    fn touch_off_the_pad_is_rejected() {
        let mut usb = usb_report();
        // Touch point 1 active at x = 0xFFF.
        usb[33] = 0x01;
        usb[34] = 0xFF;
        usb[35] = 0x0F;
        assert_eq!(
            parse(&usb, Connection::Usb, Model::DualSense, false),
            Err(ParseError::Implausible)
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

    #[test]
    fn touchpad_zones_are_equal_thirds() {
        // Existing buttons keep their bits; the zones are appended.
        assert_eq!(Button::Touchpad as u8, 13);
        assert_eq!(Button::PaddleRight as u8, 22);
        assert_eq!(Button::TouchpadLeft as u8, 23);
        assert_eq!(TOUCH_ZONE_CENTER, 640);
        assert_eq!(TOUCH_ZONE_RIGHT, 1280);
        assert_eq!(touchpad_zone(0), Button::TouchpadLeft);
        assert_eq!(touchpad_zone(639), Button::TouchpadLeft);
        assert_eq!(touchpad_zone(640), Button::TouchpadCenter);
        assert_eq!(touchpad_zone(1279), Button::TouchpadCenter);
        assert_eq!(touchpad_zone(1280), Button::TouchpadRight);
        assert_eq!(touchpad_zone(1919), Button::TouchpadRight);
        for b in Button::ALL {
            assert_eq!(Button::TOUCHPAD_ZONE_BITS & b.bit() != 0, b.is_touchpad_zone());
        }
        assert_eq!(Button::TouchpadCenter.physical(), Button::Touchpad);
        assert_eq!(Button::Cross.physical(), Button::Cross);
    }

    fn finger(x: u16) -> TouchPoint {
        TouchPoint {
            active: true,
            id: 1,
            x,
            y: 540,
        }
    }

    #[test]
    fn touch_zone_held_follows_fingers_without_a_click() {
        let none = [TouchPoint::default(); 2];
        let left = [finger(100), TouchPoint::default()];
        assert!(touch_zone_held(&left, Button::TouchpadLeft));
        assert!(!touch_zone_held(&left, Button::TouchpadRight));
        assert!(!touch_zone_held(&none, Button::TouchpadLeft));
        // A second finger in the middle third counts for that third only.
        let two = [finger(100), finger(1000)];
        assert!(touch_zone_held(&two, Button::TouchpadCenter));
        assert!(touch_zone_held(&two, Button::TouchpadLeft));
        assert!(!touch_zone_held(&two, Button::TouchpadRight));
        // A finger that has lifted doesn't count.
        let lifted = [TouchPoint { active: false, ..finger(1500) }, TouchPoint::default()];
        assert!(!touch_zone_held(&lifted, Button::TouchpadRight));
    }

    #[test]
    fn touchpad_click_zone_uses_fingers() {
        let none = [TouchPoint::default(); 2];
        let left = [finger(100), TouchPoint::default()];
        assert_eq!(
            touchpad_click_zone(&left, &none),
            Some(Button::TouchpadLeft)
        );
        // No finger on the click frame: the previous contact places it.
        assert_eq!(
            touchpad_click_zone(&none, &left),
            Some(Button::TouchpadLeft)
        );
        // A click with no finger at all is not a zone.
        assert_eq!(touchpad_click_zone(&none, &none), None);
        // Two fingers, one in each outer third, meet in the center.
        let span = [finger(0), finger(1919)];
        assert_eq!(
            touchpad_click_zone(&span, &none),
            Some(Button::TouchpadCenter)
        );
        // The current contact wins over the previous one.
        let right = [finger(1500), TouchPoint::default()];
        assert_eq!(
            touchpad_click_zone(&right, &left),
            Some(Button::TouchpadRight)
        );
    }

    #[test]
    fn parse_does_not_invent_zone_buttons() {
        let mut r = usb_report();
        r[10] |= 0x02;
        let s = parse(&r, Connection::Usb, Model::DualSense, false).unwrap();
        assert!(s.buttons.has(Button::Touchpad));
        assert!(!s.buttons.has(Button::TouchpadLeft));
        assert!(!s.buttons.has(Button::TouchpadCenter));
        assert!(!s.buttons.has(Button::TouchpadRight));
    }
}
