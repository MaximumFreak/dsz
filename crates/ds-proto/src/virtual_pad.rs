//! Virtual controller reports: what a game sees when the physical pad is
//! hidden and a virtual Xbox 360 or USB DualSense stands in for it, and
//! what the game sends back.
//!
//! - [`XusbReport`]: the 12-byte XInput gamepad state.
//! - [`ds_usb_input`]: the 64-byte USB input report `0x01` of a virtual
//!   DualSense, built from the processed pad plus the physical report's
//!   motion, timestamps, and touch.
//! - [`GameOutput`]: the game's 48-byte USB output report `0x02`, parsed with
//!   the per-section valid-flag rules.

use crate::input::{Button, Buttons};

/// Processed pad state after mappings, mutes, deadzones, and stick modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PadState {
    pub buttons: Buttons,
    pub lx: u8,
    pub ly: u8,
    pub rx: u8,
    pub ry: u8,
    pub l2: u8,
    pub r2: u8,
}

impl Default for PadState {
    fn default() -> Self {
        PadState {
            buttons: Buttons::default(),
            lx: 128,
            ly: 128,
            rx: 128,
            ry: 128,
            l2: 0,
            r2: 0,
        }
    }
}

// ------------------------------------------------------------------ Xbox 360

pub const XUSB_DPAD_UP: u16 = 0x0001;
pub const XUSB_DPAD_DOWN: u16 = 0x0002;
pub const XUSB_DPAD_LEFT: u16 = 0x0004;
pub const XUSB_DPAD_RIGHT: u16 = 0x0008;
pub const XUSB_START: u16 = 0x0010;
pub const XUSB_BACK: u16 = 0x0020;
pub const XUSB_LEFT_THUMB: u16 = 0x0040;
pub const XUSB_RIGHT_THUMB: u16 = 0x0080;
pub const XUSB_LEFT_SHOULDER: u16 = 0x0100;
pub const XUSB_RIGHT_SHOULDER: u16 = 0x0200;
pub const XUSB_GUIDE: u16 = 0x0400;
pub const XUSB_A: u16 = 0x1000;
pub const XUSB_B: u16 = 0x2000;
pub const XUSB_X: u16 = 0x4000;
pub const XUSB_Y: u16 = 0x8000;

/// `XINPUT_GAMEPAD` / `XUSB_REPORT`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XusbReport {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

impl XusbReport {
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[0..2].copy_from_slice(&self.buttons.to_le_bytes());
        b[2] = self.left_trigger;
        b[3] = self.right_trigger;
        b[4..6].copy_from_slice(&self.lx.to_le_bytes());
        b[6..8].copy_from_slice(&self.ly.to_le_bytes());
        b[8..10].copy_from_slice(&self.rx.to_le_bytes());
        b[10..12].copy_from_slice(&self.ry.to_le_bytes());
        b
    }
}

/// Stick byte onto the XInput range: the byte repeated in both halves of a
/// 16-bit word (`v * 257`) spans 0..=65535 exactly, then shifts to signed.
pub fn xbox_axis(v: u8) -> i16 {
    (v as i32 * 257 - 32768) as i16
}

/// Xbox mapping: Cross A, Circle B, Square X, Triangle Y, Create
/// Back, Options Start, PS Guide. Touchpad, mute, and Edge buttons have no
/// Xbox counterpart. Y axes are inverted (XInput up is positive).
pub fn xusb_report(p: &PadState) -> XusbReport {
    const MAP: [(Button, u16); 15] = [
        (Button::Cross, XUSB_A),
        (Button::Circle, XUSB_B),
        (Button::Square, XUSB_X),
        (Button::Triangle, XUSB_Y),
        (Button::L1, XUSB_LEFT_SHOULDER),
        (Button::R1, XUSB_RIGHT_SHOULDER),
        (Button::Create, XUSB_BACK),
        (Button::Options, XUSB_START),
        (Button::L3, XUSB_LEFT_THUMB),
        (Button::R3, XUSB_RIGHT_THUMB),
        (Button::Ps, XUSB_GUIDE),
        (Button::DpadUp, XUSB_DPAD_UP),
        (Button::DpadDown, XUSB_DPAD_DOWN),
        (Button::DpadLeft, XUSB_DPAD_LEFT),
        (Button::DpadRight, XUSB_DPAD_RIGHT),
    ];
    let mut buttons = 0u16;
    for (b, bit) in MAP {
        if p.buttons.has(b) {
            buttons |= bit;
        }
    }
    XusbReport {
        buttons,
        left_trigger: p.l2,
        right_trigger: p.r2,
        lx: xbox_axis(p.lx),
        ly: xbox_axis(255 - p.ly),
        rx: xbox_axis(p.rx),
        ry: xbox_axis(255 - p.ry),
    }
}

/// Interrupt-IN packet of a wired Xbox 360 controller: type 0, length 20,
/// then the `XUSB_REPORT` fields.
pub const X360_USB_INPUT_LEN: usize = 20;

pub fn x360_usb_input(r: &XusbReport) -> [u8; X360_USB_INPUT_LEN] {
    let mut b = [0u8; X360_USB_INPUT_LEN];
    b[0] = 0x00;
    b[1] = 0x14;
    b[2..14].copy_from_slice(&r.to_bytes());
    b
}

/// What the Xbox 360 driver sent on the interrupt-OUT endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X360Output {
    /// Large (left) and small (right) motor.
    Rumble(u8, u8),
    /// Ring LED pattern; [`x360_player`] turns it into an XInput slot.
    Led(u8),
}

pub fn parse_x360_output(d: &[u8]) -> Option<X360Output> {
    match d {
        [0x00, 0x08, _, large, small, ..] => Some(X360Output::Rumble(*large, *small)),
        [0x01, 0x03, led, ..] => Some(X360Output::Led(*led)),
        _ => None,
    }
}

/// XInput user index (0..=3) from a ring LED pattern: 2..=5 flash a
/// quadrant then light it, 6..=9 light it.
pub fn x360_player(led: u8) -> Option<u32> {
    match led {
        2..=5 => Some(led as u32 - 2),
        6..=9 => Some(led as u32 - 6),
        _ => None,
    }
}

// ------------------------------------------------------------------ DualShock 4

/// The DualShock 4's core state: sticks, buttons with the d-pad as a hat in
/// the low nibble, the PS / touchpad-click byte, then the triggers (USB
/// input bytes 1..=9, without the counter bits).
pub fn ds4_report(p: &PadState) -> [u8; 9] {
    const MAP: [(Button, u16); 12] = [
        (Button::Square, 1 << 4),
        (Button::Cross, 1 << 5),
        (Button::Circle, 1 << 6),
        (Button::Triangle, 1 << 7),
        (Button::L1, 1 << 8),
        (Button::R1, 1 << 9),
        (Button::L2, 1 << 10),
        (Button::R2, 1 << 11),
        (Button::Create, 1 << 12),
        (Button::Options, 1 << 13),
        (Button::L3, 1 << 14),
        (Button::R3, 1 << 15),
    ];
    let hat = hat(p.buttons) as u16;
    let mut buttons = hat;
    for (btn, bit) in MAP {
        if p.buttons.has(btn) {
            buttons |= bit;
        }
    }
    let special = p.buttons.has(Button::Ps) as u8 | (p.buttons.has(Button::Touchpad) as u8) << 1;
    let w = buttons.to_le_bytes();
    [p.lx, p.ly, p.rx, p.ry, w[0], w[1], special, p.l2, p.r2]
}

pub const DS4_USB_INPUT_LEN: usize = 64;

/// The virtual DualShock 4's USB input report `0x01`, laid out as Linux's
/// `dualshock4_input_report_usb`. Motion and touch come from the physical
/// DualSense report in USB layout (`raw`, see [`usb_layout`]): the IMU
/// formats match, and touch rows are rescaled from the DualSense's 1080 to
/// the DualShock 4's 942. The battery is the DualSense's, reported as on a
/// cable.
pub fn ds4_usb_input(
    pad: &PadState,
    raw: Option<&[u8; DS_USB_INPUT_LEN]>,
    touch: bool,
    seq: u8,
) -> [u8; DS4_USB_INPUT_LEN] {
    let mut r = [0u8; DS4_USB_INPUT_LEN];
    r[0] = 0x01;
    r[1..10].copy_from_slice(&ds4_report(pad));
    r[7] |= seq << 2;
    // status[0]: battery capacity in the low nibble (0..=10 charging,
    // 11 full) with the cable bit (Linux's `DS4_STATUS0_*`).
    let capacity = raw
        .map(|raw| {
            let level = (raw[53] & 0x0F).min(10);
            if raw[53] >> 4 == 2 {
                11
            } else {
                level
            }
        })
        .unwrap_or(10);
    r[30] = 0x10 | capacity;
    // One touch report follows.
    r[33] = 1;
    r[34] = seq;
    r[35] = 0x80;
    r[39] = 0x80;
    if let Some(raw) = raw {
        // Sensor timestamp: DualSense 1/3 µs ticks to DualShock 4 16/3 µs.
        let ts = u32::from_le_bytes([raw[28], raw[29], raw[30], raw[31]]) / 16;
        r[10..12].copy_from_slice(&(ts as u16).to_le_bytes());
        r[12] = raw[32];
        r[13..25].copy_from_slice(&raw[16..28]);
        if touch {
            for (k, at) in [(0usize, 35usize), (1, 39)] {
                let f = &raw[33 + 4 * k..37 + 4 * k];
                let x = f[1] as u16 | ((f[2] as u16 & 0x0F) << 8);
                let y = (f[2] as u16 >> 4) | ((f[3] as u16) << 4);
                let y = (y as u32 * 942 / 1080) as u16;
                r[at] = f[0];
                r[at + 1] = x as u8;
                r[at + 2] = ((x >> 8) as u8 & 0x0F) | ((y as u8 & 0x0F) << 4);
                r[at + 3] = (y >> 4) as u8;
            }
        }
    }
    r
}

/// What a game asked of the virtual DualShock 4 (output report `0x05`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ds4Output {
    /// (large / left, small / right).
    pub rumble: Option<(u8, u8)>,
    pub rgb: Option<[u8; 3]>,
}

pub fn parse_ds4_output(r: &[u8]) -> Option<Ds4Output> {
    if r.len() < 9 || r[0] != 0x05 {
        return None;
    }
    let flags = r[1];
    Some(Ds4Output {
        rumble: (flags & 0x01 != 0).then_some((r[5], r[4])),
        rgb: (flags & 0x02 != 0).then_some([r[6], r[7], r[8]]),
    })
}

// ------------------------------------------------------------------ DualSense input

pub const DS_USB_INPUT_LEN: usize = 64;

/// D-pad as a hat switch: 0 north, clockwise to 7 north-west, 8 released.
/// Opposite directions cancel.
fn hat(b: Buttons) -> u8 {
    let (up, down, left, right) = (
        b.has(Button::DpadUp),
        b.has(Button::DpadDown),
        b.has(Button::DpadLeft),
        b.has(Button::DpadRight),
    );
    let y = up as i8 - down as i8;
    let x = right as i8 - left as i8;
    match (x, y) {
        (0, 1) => 0,
        (1, 1) => 1,
        (1, 0) => 2,
        (1, -1) => 3,
        (0, -1) => 4,
        (-1, -1) => 5,
        (-1, 0) => 6,
        (-1, 1) => 7,
        _ => 8,
    }
}

/// Copy a physical input report into USB layout (byte 0 = report id `0x01`).
/// Bluetooth `0x31` reports are shifted one byte left. Returns `None` for the
/// short Bluetooth report, which has no motion or touch.
pub fn usb_layout(report: &[u8]) -> Option<[u8; DS_USB_INPUT_LEN]> {
    let mut out = [0u8; DS_USB_INPUT_LEN];
    match report.first()? {
        0x31 if report.len() >= 64 => {
            out[1..].copy_from_slice(&report[2..65]);
        }
        0x01 if report.len() >= 64 => {
            out.copy_from_slice(&report[..64]);
        }
        _ => return None,
    }
    out[0] = 0x01;
    Some(out)
}

/// Build the virtual DualSense's USB input report.
///
/// `raw` is the physical report in USB layout ([`usb_layout`]); everything
/// after the buttons (counter, motion, timestamps, touch, battery) passes
/// through. Sticks, triggers, and buttons come from `pad`. With `touch`
/// false the touch points are reported lifted. Byte 54 is SDL's
/// `ucConnectState`: always USB (`0x08`), plus the controller's headphone
/// bit. Edge-only buttons are dropped because the virtual pad is a plain
/// DualSense.
pub fn ds_usb_input(
    pad: &PadState,
    raw: Option<&[u8; DS_USB_INPUT_LEN]>,
    touch: bool,
    seq: u8,
) -> [u8; DS_USB_INPUT_LEN] {
    let mut r = [0u8; DS_USB_INPUT_LEN];
    r[0] = 0x01;
    r[1] = pad.lx;
    r[2] = pad.ly;
    r[3] = pad.rx;
    r[4] = pad.ry;
    r[5] = pad.l2;
    r[6] = pad.r2;
    let b = pad.buttons;
    let mut b8 = hat(b);
    if b.has(Button::Square) {
        b8 |= 0x10;
    }
    if b.has(Button::Cross) {
        b8 |= 0x20;
    }
    if b.has(Button::Circle) {
        b8 |= 0x40;
    }
    if b.has(Button::Triangle) {
        b8 |= 0x80;
    }
    let mut b9 = 0u8;
    for (btn, bit) in [
        (Button::L1, 0x01),
        (Button::R1, 0x02),
        (Button::Create, 0x10),
        (Button::Options, 0x20),
        (Button::L3, 0x40),
        (Button::R3, 0x80),
    ] {
        if b.has(btn) {
            b9 |= bit;
        }
    }
    // The digital L2/R2 bits follow the analog bytes, as on the real pad.
    if pad.l2 > 0 {
        b9 |= 0x04;
    }
    if pad.r2 > 0 {
        b9 |= 0x08;
    }
    let mut b10 = 0u8;
    if b.has(Button::Ps) {
        b10 |= 0x01;
    }
    if b.has(Button::Touchpad) {
        b10 |= 0x02;
    }
    if b.has(Button::Mute) {
        b10 |= 0x04;
    }
    r[8] = b8;
    r[9] = b9;
    r[10] = b10;
    match raw {
        Some(raw) => {
            r[7] = raw[7];
            r[11..54].copy_from_slice(&raw[11..54]);
            r[54] = 0x08 | (raw[54] & 0x01);
        }
        None => {
            r[7] = seq;
            // Battery: 100 %, not charging.
            r[53] = 0x0A;
            r[54] = 0x08;
        }
    }
    if raw.is_none() || !touch {
        r[33..41].fill(0);
        r[33] = 0x80;
        r[37] = 0x80;
    }
    r
}

// ------------------------------------------------------------------ DualSense output

pub const DS_USB_OUTPUT_LEN: usize = 48;

/// What one game output report set. Each optional field is `Some` only
/// when the report's valid flag for that section is set, the way the
/// controller itself reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GameOutput {
    /// `(heavy_left, light_right)` motor levels; zeros stop the motors.
    /// Every report carries them: with the rumble flags clear the controller
    /// leaves rumble emulation for audio haptics, so the motors stop. SDL
    /// stops rumble this way, with a report that has no rumble flags.
    pub rumble: (u8, u8),
    pub right_trigger: Option<[u8; 11]>,
    pub left_trigger: Option<[u8; 11]>,
    pub mute_led: Option<u8>,
    pub rgb: Option<[u8; 3]>,
    pub player_leds: Option<u8>,
    /// The game hands the lights back (release LEDs flag).
    pub release_leds: bool,
    /// Motor power reduction byte.
    pub motor_power: Option<u8>,
}

/// USB output report `0x02` offsets and valid flags, from the layout in
/// Linux's `hid-playstation` driver (`dualsense_output_report_common`),
/// with the trigger and motor power fields as pydualsense documents them.
mod out {
    pub const FLAGS0: usize = 1;
    pub const FLAGS1: usize = 2;
    pub const MOTOR_RIGHT: usize = 3;
    pub const MOTOR_LEFT: usize = 4;
    pub const MUTE_LED: usize = 9;
    pub const RIGHT_TRIGGER: usize = 11;
    pub const LEFT_TRIGGER: usize = 22;
    pub const MOTOR_POWER: usize = 37;
    pub const PLAYER_LEDS: usize = 44;
    pub const RGB: usize = 45;

    /// Flags 0: rumble emulation and haptics select both mean "these are
    /// the motor levels"; then the two triggers.
    pub const RUMBLE: u8 = 0x01 | 0x02;
    pub const RIGHT_TRIGGER_ON: u8 = 0x04;
    pub const LEFT_TRIGGER_ON: u8 = 0x08;
    /// Flags 1.
    pub const MUTE_LED_ON: u8 = 0x01;
    pub const RGB_ON: u8 = 0x04;
    pub const RELEASE_LEDS: u8 = 0x08;
    pub const PLAYER_LEDS_ON: u8 = 0x10;
    pub const MOTOR_POWER_ON: u8 = 0x40;
}

/// Parse a USB output report `0x02` from the virtual DualSense. Anything
/// that is not 48 bytes with id 2 is ignored.
pub fn parse_ds_usb_output(r: &[u8]) -> Option<GameOutput> {
    if r.len() < DS_USB_OUTPUT_LEN || r[0] != 0x02 {
        return None;
    }
    let (f0, f1) = (r[out::FLAGS0], r[out::FLAGS1]);
    let on = |flags: u8, bit: u8| flags & bit != 0;
    let trigger = |at: usize| {
        let mut t = [0u8; 11];
        t.copy_from_slice(&r[at..at + 11]);
        t
    };
    Some(GameOutput {
        rumble: if on(f0, out::RUMBLE) {
            (r[out::MOTOR_LEFT], r[out::MOTOR_RIGHT])
        } else {
            (0, 0)
        },
        right_trigger: on(f0, out::RIGHT_TRIGGER_ON).then(|| trigger(out::RIGHT_TRIGGER)),
        left_trigger: on(f0, out::LEFT_TRIGGER_ON).then(|| trigger(out::LEFT_TRIGGER)),
        mute_led: on(f1, out::MUTE_LED_ON).then(|| r[out::MUTE_LED]),
        rgb: on(f1, out::RGB_ON).then(|| [r[out::RGB], r[out::RGB + 1], r[out::RGB + 2]]),
        player_leds: on(f1, out::PLAYER_LEDS_ON).then(|| r[out::PLAYER_LEDS]),
        release_leds: on(f1, out::RELEASE_LEDS),
        motor_power: on(f1, out::MOTOR_POWER_ON).then(|| r[out::MOTOR_POWER]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xbox_axes() {
        assert_eq!(xbox_axis(0), -32768);
        assert_eq!(xbox_axis(255), 32767);
        let p = PadState {
            ly: 0,
            ..Default::default()
        };
        // Stick pushed up is positive Y in XInput.
        assert_eq!(xusb_report(&p).ly, 32767);
    }

    #[test]
    fn xbox_buttons() {
        let mut b = Buttons::default();
        b.set(Button::Cross, true);
        b.set(Button::Ps, true);
        b.set(Button::DpadLeft, true);
        let r = xusb_report(&PadState {
            buttons: b,
            ..Default::default()
        });
        assert_eq!(r.buttons, XUSB_A | XUSB_GUIDE | XUSB_DPAD_LEFT);
        assert_eq!(
            r.to_bytes()[0..2],
            (XUSB_A | XUSB_GUIDE | XUSB_DPAD_LEFT).to_le_bytes()
        );
    }

    #[test]
    fn ds_input_buttons_and_hat() {
        let mut b = Buttons::default();
        b.set(Button::DpadUp, true);
        b.set(Button::DpadRight, true);
        b.set(Button::Triangle, true);
        b.set(Button::Options, true);
        b.set(Button::FnLeft, true);
        let r = ds_usb_input(
            &PadState {
                buttons: b,
                ..Default::default()
            },
            None,
            false,
            3,
        );
        assert_eq!(r[0], 1);
        assert_eq!(r[8], 0x80 | 1);
        assert_eq!(r[9], 0x20);
        assert_eq!(r[10], 0);
        assert_eq!(r[33] & 0x80, 0x80);
        assert_eq!(r[54], 0x08);
    }

    #[test]
    fn bt_to_usb_layout() {
        let mut bt = [0u8; 78];
        bt[0] = 0x31;
        bt[2] = 0x11; // LX
        bt[17] = 0x22; // gyro low byte (USB 16)
        let u = usb_layout(&bt).unwrap();
        assert_eq!(u[0], 1);
        assert_eq!(u[1], 0x11);
        assert_eq!(u[16], 0x22);
    }

    #[test]
    fn output_sections() {
        let mut r = [0u8; 48];
        r[0] = 2;
        // No valid flags: nothing is set.
        assert_eq!(parse_ds_usb_output(&r).unwrap(), GameOutput::default());
        // Motors and the right trigger.
        r[1] = 0x01 | 0x04;
        r[3] = 40;
        r[4] = 200;
        r[11] = 0x21;
        let o = parse_ds_usb_output(&r).unwrap();
        assert_eq!(o.rumble, (200, 40));
        assert_eq!(o.right_trigger.unwrap()[0], 0x21);
        assert!(o.left_trigger.is_none());
        // Clearing the rumble flags stops the motors, whatever the motor
        // bytes say (SDL's stop report).
        r[1] = 0x04;
        assert_eq!(parse_ds_usb_output(&r).unwrap().rumble, (0, 0));
        r[1] = 0;
        assert_eq!(parse_ds_usb_output(&r).unwrap().rumble, (0, 0));
        r[3] = 0;
        r[4] = 0;
        // Lightbar only; trigger bytes without their flag are ignored.
        r[1] = 0;
        r[2] = 0x04;
        r[45] = 1;
        r[46] = 2;
        r[47] = 3;
        let o = parse_ds_usb_output(&r).unwrap();
        assert_eq!(o.rgb, Some([1, 2, 3]));
        assert!(o.rumble == (0, 0) && o.right_trigger.is_none());
        // Release LEDs.
        r[2] = 0x08;
        assert!(parse_ds_usb_output(&r).unwrap().release_leds);
        // Wrong id or length is ignored.
        r[0] = 5;
        assert!(parse_ds_usb_output(&r).is_none());
        assert!(parse_ds_usb_output(&r[..40]).is_none());
    }
}

#[cfg(test)]
mod ds4_tests {
    use super::*;

    #[test]
    fn x360_packets() {
        let mut p = PadState::default();
        p.buttons.set(Button::Cross, true);
        p.r2 = 200;
        let b = x360_usb_input(&xusb_report(&p));
        assert_eq!(&b[..6], &[0x00, 0x14, 0x00, 0x10, 0, 200]);
        assert_eq!(
            parse_x360_output(&[0, 8, 0, 0xC0, 0x40, 0, 0, 0]),
            Some(X360Output::Rumble(0xC0, 0x40))
        );
        assert_eq!(parse_x360_output(&[1, 3, 7]), Some(X360Output::Led(7)));
        assert_eq!(x360_player(7), Some(1));
        assert_eq!(x360_player(1), None);
    }

    #[test]
    fn ds4_usb_report_carries_motion_and_scaled_touch() {
        let mut raw = [0u8; DS_USB_INPUT_LEN];
        raw[0] = 0x01;
        for (i, b) in raw[16..28].iter_mut().enumerate() {
            *b = i as u8 + 1;
        }
        // Finger 0: id 5, x 1000, y 1080 (bottom edge).
        raw[33] = 5;
        raw[34] = (1000 & 0xFF) as u8;
        raw[35] = ((1000 >> 8) as u8) | (((1080 & 0x0F) as u8) << 4);
        raw[36] = (1080 >> 4) as u8;
        raw[37] = 0x80;
        let r = ds4_usb_input(&PadState::default(), Some(&raw), true, 3);
        assert_eq!(r[0], 0x01);
        assert_eq!(&r[1..5], &[128; 4]);
        assert_eq!(r[5] & 0x0F, 8, "hat released");
        assert_eq!(r[7] >> 2, 3, "counter");
        assert_eq!(&r[13..25], &raw[16..28]);
        assert_eq!(r[35], 5);
        let x = r[36] as u16 | ((r[37] as u16 & 0x0F) << 8);
        let y = (r[37] as u16 >> 4) | ((r[38] as u16) << 4);
        assert_eq!((x, y), (1000, 942));
        assert_eq!(r[39], 0x80);
        let o = parse_ds4_output(&[0x05, 0x03, 0x04, 0, 0x20, 0x90, 1, 2, 3]).unwrap();
        assert_eq!(o.rumble, Some((0x90, 0x20)));
        assert_eq!(o.rgb, Some([1, 2, 3]));
    }

    #[test]
    fn ds4_report_layout() {
        let mut p = PadState::default();
        assert_eq!(ds4_report(&p)[4] & 0x0F, 8, "released hat");
        p.buttons.set(Button::Cross, true);
        p.buttons.set(Button::DpadUp, true);
        p.buttons.set(Button::DpadRight, true);
        p.buttons.set(Button::Ps, true);
        p.l2 = 200;
        let r = ds4_report(&p);
        assert_eq!(u16::from_le_bytes([r[4], r[5]]), 1 | (1 << 5));
        assert_eq!(r[6], 1);
        assert_eq!(r[7], 200);
        assert_eq!(&r[..4], &[128, 128, 128, 128]);
    }
}
