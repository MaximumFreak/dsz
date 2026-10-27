//! USB identities of the virtual controllers. The DualSense: device and configuration descriptors
//! with the USB Audio Class 1 functions (4-channel 48 kHz render for speaker
//! plus actuators, stereo capture), and the HID game-pad interface.
//!
//! Layout follows a physical controller (`wTotalLength` 227, as in the
//! `lsusb` dump in nondebug/dualsense): interface 0
//! audio control, 1 render streaming (EP 0x01 ISO OUT), 2 capture streaming
//! (EP 0x82 ISO IN), 3 HID (EP 0x84 interrupt IN, EP 0x03 interrupt OUT).
//! High speed, so `bInterval` 4 is one packet per millisecond.

pub const VID: u16 = 0x054C;
pub const PID: u16 = 0x0CE6;
pub const SPEED_FULL: u32 = 2;
pub const SPEED_HIGH: u32 = 3;
pub const NUM_INTERFACES: u8 = 4;

pub const IF_AUDIO_CONTROL: u8 = 0;
pub const IF_RENDER: u8 = 1;
pub const IF_CAPTURE: u8 = 2;
pub const IF_HID: u8 = 3;

pub const EP_RENDER: u32 = 1; // 0x01 ISO OUT
pub const EP_CAPTURE: u32 = 2; // 0x82 ISO IN
pub const EP_OUTPUT: u32 = 3; // 0x03 interrupt OUT
pub const EP_INPUT: u32 = 4; // 0x84 interrupt IN

/// Feature unit ids in the audio topology.
pub const UNIT_SPEAKER: u8 = 2;
pub const UNIT_MIC: u8 = 5;

pub const RENDER_CHANNELS: usize = 4;
/// Stereo 16-bit microphone frames per 1 ms packet.
pub const CAPTURE_PACKET: usize = 48 * 2 * 2;

/// The DualSense's HID report descriptor (273 bytes), as dumped from a
/// controller in dogtopus's public write-up
/// (gist.github.com/dogtopus/894da226d73afb3bdd195df41b3a26aa): input
/// report 1, output report 2, and the feature reports.
pub const HID_REPORT_DESCRIPTOR: [u8; 273] = [
    0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35,
    0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x06, 0x81, 0x02, 0x06,
    0x00, 0xFF, 0x09, 0x20, 0x95, 0x01, 0x81, 0x02, 0x05, 0x01, 0x09, 0x39, 0x15, 0x00, 0x25, 0x07,
    0x35, 0x00, 0x46, 0x3B, 0x01, 0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00, 0x05,
    0x09, 0x19, 0x01, 0x29, 0x0F, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x0F, 0x81, 0x02, 0x06,
    0x00, 0xFF, 0x09, 0x21, 0x95, 0x0D, 0x81, 0x02, 0x06, 0x00, 0xFF, 0x09, 0x22, 0x15, 0x00, 0x26,
    0xFF, 0x00, 0x75, 0x08, 0x95, 0x34, 0x81, 0x02, 0x85, 0x02, 0x09, 0x23, 0x95, 0x2F, 0x91, 0x02,
    0x85, 0x05, 0x09, 0x33, 0x95, 0x28, 0xB1, 0x02, 0x85, 0x08, 0x09, 0x34, 0x95, 0x2F, 0xB1, 0x02,
    0x85, 0x09, 0x09, 0x24, 0x95, 0x13, 0xB1, 0x02, 0x85, 0x0A, 0x09, 0x25, 0x95, 0x1A, 0xB1, 0x02,
    0x85, 0x20, 0x09, 0x26, 0x95, 0x3F, 0xB1, 0x02, 0x85, 0x21, 0x09, 0x27, 0x95, 0x04, 0xB1, 0x02,
    0x85, 0x22, 0x09, 0x40, 0x95, 0x3F, 0xB1, 0x02, 0x85, 0x80, 0x09, 0x28, 0x95, 0x3F, 0xB1, 0x02,
    0x85, 0x81, 0x09, 0x29, 0x95, 0x3F, 0xB1, 0x02, 0x85, 0x82, 0x09, 0x2A, 0x95, 0x09, 0xB1, 0x02,
    0x85, 0x83, 0x09, 0x2B, 0x95, 0x3F, 0xB1, 0x02, 0x85, 0x84, 0x09, 0x2C, 0x95, 0x3F, 0xB1, 0x02,
    0x85, 0x85, 0x09, 0x2D, 0x95, 0x02, 0xB1, 0x02, 0x85, 0xA0, 0x09, 0x2E, 0x95, 0x01, 0xB1, 0x02,
    0x85, 0xE0, 0x09, 0x2F, 0x95, 0x3F, 0xB1, 0x02, 0x85, 0xF0, 0x09, 0x30, 0x95, 0x3F, 0xB1, 0x02,
    0x85, 0xF1, 0x09, 0x31, 0x95, 0x3F, 0xB1, 0x02, 0x85, 0xF2, 0x09, 0x32, 0x95, 0x0F, 0xB1, 0x02,
    0x85, 0xF4, 0x09, 0x35, 0x95, 0x3F, 0xB1, 0x02, 0x85, 0xF5, 0x09, 0x36, 0x95, 0x03, 0xB1, 0x02,
    0xC0,
];

pub const DEVICE: [u8; 18] = [
    0x12, 0x01, 0x00, 0x02, // USB 2.0
    0x00, 0x00, 0x00, 0x40, // class per interface, EP0 64 bytes
    0x4C, 0x05, 0xE6, 0x0C, // 054C:0CE6
    0x00, 0x01, // bcdDevice 1.00
    0x01, 0x02, 0x03, // manufacturer, product, serial (ours, see SERIAL_PREFIX)
    0x01,
];

pub fn configuration() -> Vec<u8> {
    let rd = HID_REPORT_DESCRIPTOR.len() as u16;
    let mut v: Vec<u8> = vec![0x09, 0x02, 0, 0, NUM_INTERFACES, 0x01, 0x00, 0xC0, 0xFA];
    // Interface 0: audio control.
    v.extend_from_slice(&[0x09, 0x04, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00]);
    v.extend_from_slice(&[0x0A, 0x24, 0x01, 0x00, 0x01, 0x49, 0x00, 0x02, 0x01, 0x02]); // header, streaming 1 and 2
    v.extend_from_slice(&[
        0x0C, 0x24, 0x02, 0x01, 0x01, 0x01, 0x06, 0x04, 0x33, 0x00, 0x00, 0x00,
    ]); // IT 1: USB stream, 4 ch
    v.extend_from_slice(&[
        0x0C, 0x24, 0x06, 0x02, 0x01, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]); // FU 2: mute, volume
    v.extend_from_slice(&[0x09, 0x24, 0x03, 0x03, 0x01, 0x03, 0x04, 0x02, 0x00]); // OT 3: speaker
    v.extend_from_slice(&[
        0x0C, 0x24, 0x02, 0x04, 0x02, 0x04, 0x03, 0x02, 0x03, 0x00, 0x00, 0x00,
    ]); // IT 4: headset mic, 2 ch
    v.extend_from_slice(&[0x09, 0x24, 0x06, 0x05, 0x04, 0x01, 0x03, 0x00, 0x00]); // FU 5
    v.extend_from_slice(&[0x09, 0x24, 0x03, 0x06, 0x01, 0x01, 0x01, 0x05, 0x00]); // OT 6: USB stream
                                                                                  // Interface 1: render, alt 0 idle, alt 1 streaming.
    v.extend_from_slice(&[0x09, 0x04, 0x01, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00]);
    v.extend_from_slice(&[0x09, 0x04, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00]);
    v.extend_from_slice(&[0x07, 0x24, 0x01, 0x01, 0x01, 0x01, 0x00]);
    v.extend_from_slice(&[
        0x0B, 0x24, 0x02, 0x01, 0x04, 0x02, 0x10, 0x01, 0x80, 0xBB, 0x00,
    ]); // 4 ch, 16 bit, 48 kHz
    v.extend_from_slice(&[0x09, 0x05, 0x01, 0x09, 0x88, 0x01, 0x04, 0x00, 0x00]); // ISO adaptive, 392 bytes
    v.extend_from_slice(&[0x07, 0x25, 0x01, 0x00, 0x00, 0x00, 0x00]);
    // Interface 2: capture.
    v.extend_from_slice(&[0x09, 0x04, 0x02, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00]);
    v.extend_from_slice(&[0x09, 0x04, 0x02, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00]);
    v.extend_from_slice(&[0x07, 0x24, 0x01, 0x06, 0x01, 0x01, 0x00]);
    v.extend_from_slice(&[
        0x0B, 0x24, 0x02, 0x01, 0x02, 0x02, 0x10, 0x01, 0x80, 0xBB, 0x00,
    ]); // 2 ch, 16 bit, 48 kHz
    v.extend_from_slice(&[0x09, 0x05, 0x82, 0x05, 0xC4, 0x00, 0x04, 0x00, 0x00]); // ISO async, 196 bytes
    v.extend_from_slice(&[0x07, 0x25, 0x01, 0x00, 0x00, 0x00, 0x00]);
    // Interface 3: HID.
    v.extend_from_slice(&[0x09, 0x04, 0x03, 0x00, 0x02, 0x03, 0x00, 0x00, 0x00]);
    v.extend_from_slice(&hid_descriptor(rd));
    v.extend_from_slice(&[0x07, 0x05, 0x84, 0x03, 0x40, 0x00, 0x04]);
    v.extend_from_slice(&[0x07, 0x05, 0x03, 0x03, 0x40, 0x00, 0x04]);
    let total = v.len() as u16;
    v[2..4].copy_from_slice(&total.to_le_bytes());
    v
}

pub fn hid_descriptor(report_len: u16) -> [u8; 9] {
    let l = report_len.to_le_bytes();
    [0x09, 0x21, 0x11, 0x01, 0x00, 0x01, 0x22, l[0], l[1]]
}

pub fn string(index: u8) -> Option<Vec<u8>> {
    let s = match index {
        0 => return Some(vec![0x04, 0x03, 0x09, 0x04]),
        1 => "Sony Interactive Entertainment",
        2 => "Wireless Controller",
        _ => return None,
    };
    Some(utf16_string(s))
}

/// A string descriptor.
pub fn utf16_string(s: &str) -> Vec<u8> {
    let mut v = vec![0u8, 0x03];
    for c in s.encode_utf16() {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v[0] = v.len() as u8;
    v
}

// ------------------------------------------------------------------ microphone

/// The DualSense's microphone on its own: the same device identity and
/// capture function as [`configuration`], without the render stream or the
/// HID game pad, for a virtual pad that has no microphone (Xbox 360 or
/// DualShock 4). Windows names it after the product string, as with the
/// virtual DualSense, and no game finds a controller on it.
///
/// Interface 0 audio control (input terminal 4, feature unit 5, output
/// terminal 6, the ids the virtual DualSense uses), 1 capture streaming
/// (EP 0x82 ISO IN, the same format and packet as the virtual DualSense).
/// High speed, so the capture clock runs exactly as it does there.
pub mod mic {
    pub const NUM_INTERFACES: u8 = 2;
    pub const IF_AUDIO_CONTROL: u8 = 0;
    pub const IF_CAPTURE: u8 = 1;
    pub const EP_CAPTURE: u32 = super::EP_CAPTURE; // 0x82 ISO IN

    pub fn configuration() -> Vec<u8> {
        let mut v: Vec<u8> = vec![0x09, 0x02, 0, 0, NUM_INTERFACES, 0x01, 0x00, 0xC0, 0xFA];
        // Interface 0: audio control.
        v.extend_from_slice(&[0x09, 0x04, IF_AUDIO_CONTROL, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00]);
        v.extend_from_slice(&[0x09, 0x24, 0x01, 0x00, 0x01, 0x27, 0x00, 0x01, IF_CAPTURE]); // header, streaming 1
        // IT 4: headset mic, 2 ch. No associated terminal: there is no
        // speaker on this device.
        v.extend_from_slice(&[
            0x0C, 0x24, 0x02, 0x04, 0x02, 0x04, 0x00, 0x02, 0x03, 0x00, 0x00, 0x00,
        ]);
        v.extend_from_slice(&[0x09, 0x24, 0x06, 0x05, 0x04, 0x01, 0x03, 0x00, 0x00]); // FU 5
        v.extend_from_slice(&[0x09, 0x24, 0x03, 0x06, 0x01, 0x01, 0x00, 0x05, 0x00]); // OT 6: USB stream
        // Interface 1: capture, alt 0 idle, alt 1 streaming.
        v.extend_from_slice(&[0x09, 0x04, IF_CAPTURE, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00]);
        v.extend_from_slice(&[0x09, 0x04, IF_CAPTURE, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00]);
        v.extend_from_slice(&[0x07, 0x24, 0x01, 0x06, 0x01, 0x01, 0x00]);
        v.extend_from_slice(&[
            0x0B, 0x24, 0x02, 0x01, 0x02, 0x02, 0x10, 0x01, 0x80, 0xBB, 0x00,
        ]); // 2 ch, 16 bit, 48 kHz
        let ep = 0x80 | EP_CAPTURE as u8;
        v.extend_from_slice(&[0x09, 0x05, ep, 0x05, 0xC4, 0x00, 0x04, 0x00, 0x00]); // ISO async, 196 bytes
        v.extend_from_slice(&[0x07, 0x25, 0x01, 0x00, 0x00, 0x00, 0x00]);
        let total = v.len() as u16;
        v[2..4].copy_from_slice(&total.to_le_bytes());
        v
    }
}

// ------------------------------------------------------------------ Xbox 360

/// A wired Xbox 360 controller as Windows' own `xusb22` driver expects it:
/// full speed, one vendor interface (class FF, subclass 5D, protocol 01)
/// with its class descriptor, interrupt IN 0x81 and OUT 0x01.
pub mod x360 {
    pub const VID: u16 = 0x045E;
    pub const PID: u16 = 0x028E;
    pub const EP_INPUT: u32 = 1; // 0x81
    pub const EP_OUTPUT: u32 = 1; // 0x01

    pub const DEVICE: [u8; 18] = [
        0x12, 0x01, 0x00, 0x02, // USB 2.0
        0xFF, 0xFF, 0xFF, 0x08, // vendor class, EP0 8 bytes
        0x5E, 0x04, 0x8E, 0x02, // 045E:028E
        0x14, 0x01, // bcdDevice 1.14
        0x01, 0x02, 0x03, 0x01,
    ];

    pub fn configuration() -> Vec<u8> {
        let mut v: Vec<u8> = vec![0x09, 0x02, 0, 0, 0x01, 0x01, 0x00, 0xA0, 0xFA];
        v.extend_from_slice(&[0x09, 0x04, 0x00, 0x00, 0x02, 0xFF, 0x5D, 0x01, 0x00]);
        // Vendor class descriptor the driver reads for the endpoint layout.
        v.extend_from_slice(&[
            0x11, 0x21, 0x00, 0x01, 0x01, 0x25, 0x81, 0x14, 0x00, 0x00, 0x00, 0x00, 0x13, 0x01,
            0x08, 0x00, 0x00,
        ]);
        // bInterval 1: the driver gets each change as soon as it happens.
        v.extend_from_slice(&[0x07, 0x05, 0x81, 0x03, 0x20, 0x00, 0x01]);
        v.extend_from_slice(&[0x07, 0x05, 0x01, 0x03, 0x20, 0x00, 0x08]);
        let total = v.len() as u16;
        v[2..4].copy_from_slice(&total.to_le_bytes());
        v
    }

    /// Answers to the driver's capability requests (`C1 01`, interface 0),
    /// laid out as in Santroller's Xbox 360 notes
    /// (santroller.github.io/guitar-configurator, "Controller Identification
    /// and Capabilities"). XInputGetCapabilities passes them on to games and
    /// to Windows' own gamepad readers, so they describe a full wired pad.
    ///
    /// `wValue 0x0000`: both rumble motors, full range.
    pub const RUMBLE_CAPS: [u8; 8] = [0x00, 0x08, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00];
    /// `wValue 0x0100`: a mask over the input report. Every button bit
    /// (`0xF7FF`: all but the unused bit 11; Guide included), both
    /// triggers, and four sticks with 10 bits of resolution (`0xFFC0`).
    /// Reserved and flags stay 0.
    pub const INPUT_CAPS: [u8; 20] = [
        0x00, 0x14, // report 0, 20 bytes
        0xFF, 0xF7, // buttons
        0xFF, 0xFF, // triggers
        0xC0, 0xFF, 0xC0, 0xFF, // left stick X, Y
        0xC0, 0xFF, 0xC0, 0xFF, // right stick X, Y
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    pub fn string(index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, 0x03, 0x09, 0x04]),
            1 => Some(super::utf16_string("DSZ")),
            2 => Some(super::utf16_string("Controller")),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ DualShock 4

/// A wired DualShock 4 (054C:05C4) as a plain HID game pad: one interface,
/// interrupt IN 0x84 and OUT 0x03, and a report descriptor with the
/// controller's layout (report 1 in, 5 out, and the feature reports games
/// and Steam read).
pub mod ds4 {
    pub const VID: u16 = 0x054C;
    pub const PID: u16 = 0x05C4;
    pub const IF_HID: u8 = 0;
    pub const EP_INPUT: u32 = 4; // 0x84
    pub const EP_OUTPUT: u32 = 3; // 0x03

    pub const DEVICE: [u8; 18] = [
        0x12, 0x01, 0x00, 0x02, // USB 2.0
        0x00, 0x00, 0x00, 0x40, // class per interface, EP0 64 bytes
        0x4C, 0x05, 0xC4, 0x05, // 054C:05C4
        0x00, 0x01, // bcdDevice 1.00
        0x01, 0x02, 0x03, 0x01,
    ];

    #[rustfmt::skip]
    pub const HID_REPORT_DESCRIPTOR: &[u8] = &[
        0x05, 0x01, 0x09, 0x05, 0xA1, 0x01,         // Generic Desktop, Game Pad, Application
        0x85, 0x01,                                 // Report 1
        0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35, // X Y Z Rz: the sticks
        0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02,
        0x09, 0x39, 0x15, 0x00, 0x25, 0x07, 0x35, 0x00, 0x46, 0x3B, 0x01, 0x65, 0x14, // hat
        0x75, 0x04, 0x95, 0x01, 0x81, 0x42,
        0x65, 0x00,
        0x05, 0x09, 0x19, 0x01, 0x29, 0x0E, 0x15, 0x00, 0x25, 0x01, // 14 buttons
        0x75, 0x01, 0x95, 0x0E, 0x81, 0x02,
        0x06, 0x00, 0xFF, 0x09, 0x20, 0x75, 0x06, 0x95, 0x01, 0x15, 0x00, 0x25, 0x7F, 0x81, 0x02, // counter
        0x05, 0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xFF, 0x00, // Rx Ry: the triggers
        0x75, 0x08, 0x95, 0x02, 0x81, 0x02,
        0x06, 0x00, 0xFF, 0x09, 0x21, 0x95, 0x36, 0x81, 0x02, // motion, battery, touch
        0x85, 0x05, 0x09, 0x22, 0x95, 0x1F, 0x91, 0x02, // output 5: rumble, lightbar
        0x85, 0x02, 0x09, 0x24, 0x95, 0x24, 0xB1, 0x02, // feature 2: IMU calibration
        0x85, 0x12, 0x09, 0x26, 0x95, 0x0F, 0xB1, 0x02, // feature 0x12: pairing info
        0x85, 0x81, 0x09, 0x27, 0x95, 0x06, 0xB1, 0x02, // feature 0x81: MAC
        0x85, 0xA3, 0x09, 0x28, 0x95, 0x30, 0xB1, 0x02, // feature 0xA3: firmware
        0xC0,
    ];

    pub fn configuration() -> Vec<u8> {
        let rd = HID_REPORT_DESCRIPTOR.len() as u16;
        let mut v: Vec<u8> = vec![0x09, 0x02, 0, 0, 0x01, 0x01, 0x00, 0xC0, 0xFA];
        v.extend_from_slice(&[0x09, 0x04, IF_HID, 0x00, 0x02, 0x03, 0x00, 0x00, 0x00]);
        v.extend_from_slice(&super::hid_descriptor(rd));
        v.extend_from_slice(&[0x07, 0x05, 0x84, 0x03, 0x40, 0x00, 0x01]);
        v.extend_from_slice(&[0x07, 0x05, 0x03, 0x03, 0x40, 0x00, 0x01]);
        let total = v.len() as u16;
        v[2..4].copy_from_slice(&total.to_le_bytes());
        v
    }

    pub fn string(index: u8) -> Option<Vec<u8>> {
        match index {
            0 => Some(vec![0x04, 0x03, 0x09, 0x04]),
            1 => Some(super::utf16_string("Sony Computer Entertainment")),
            2 => Some(super::utf16_string("Wireless Controller")),
            _ => None,
        }
    }

    /// Feature report answers (report id first). Calibration is the
    /// physical DualSense's, whose IMU and calibration layout match.
    /// `mac` is the pad's USB serial, and reports `0x12` and `0x81` carry
    /// it in the order SDL reads back (last byte first).
    pub fn feature(id: u8, dualsense_calibration: Option<&[u8]>, mac: &[u8; 6]) -> Option<Vec<u8>> {
        match id {
            0x02 => {
                let mut v = crate::virt::mirror::neutral_calibration(0x02, 37);
                if let Some(c) = dualsense_calibration.filter(|c| c.len() >= 36 && c[0] == 0x05) {
                    v[1..36].copy_from_slice(&c[1..36]);
                }
                Some(v)
            }
            0x12 => {
                let mut v = vec![0x12];
                v.extend(mac.iter().rev());
                v.extend_from_slice(&[0x08, 0x25, 0x00]);
                v.extend_from_slice(&[0; 6]);
                Some(v)
            }
            0x81 => {
                let mut v = vec![0x81];
                v.extend(mac.iter().rev());
                Some(v)
            }
            0xA3 => {
                let mut v = vec![0u8; 49];
                v[0] = 0xA3;
                v[1..12].copy_from_slice(b"Sep 21 2018");
                v[17..25].copy_from_slice(b"04:50:51");
                Some(v)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_matches_a_physical_dualsense() {
        let c = configuration();
        // 227 bytes on a real controller; the HID descriptor length is the
        // only field that depends on our report descriptor.
        assert_eq!(c.len(), 227);
        assert_eq!(u16::from_le_bytes([c[2], c[3]]), 227);
        // Every descriptor's length chains to the end.
        let mut i = 0;
        while i < c.len() {
            assert!(c[i] >= 2);
            i += c[i] as usize;
        }
        assert_eq!(i, c.len());
    }

    #[test]
    fn other_models_chain_and_report_total_length() {
        for c in [x360::configuration(), ds4::configuration()] {
            assert_eq!(u16::from_le_bytes([c[2], c[3]]) as usize, c.len());
            let mut i = 0;
            while i < c.len() {
                assert!(c[i] >= 2);
                i += c[i] as usize;
            }
            assert_eq!(i, c.len());
        }
        assert_eq!(x360::configuration().len(), 49);
        let mac = [0u8; 6];
        assert_eq!(ds4::feature(0x02, None, &mac).unwrap().len(), 37);
        assert_eq!(ds4::feature(0x12, None, &mac).unwrap().len(), 16);
    }

    /// Every descriptor in a configuration, in order.
    fn descriptors(c: &[u8]) -> Vec<&[u8]> {
        let mut v = Vec::new();
        let mut i = 0;
        while i < c.len() {
            assert!(c[i] >= 2 && i + c[i] as usize <= c.len());
            v.push(&c[i..i + c[i] as usize]);
            i += c[i] as usize;
        }
        v
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02X}")).collect()
    }

    /// The pads' descriptors stay what they were before the microphone
    /// device existed: Windows binds its drivers to these bytes.
    #[test]
    fn pad_descriptors_are_pinned() {
        assert_eq!(hex(&DEVICE), DS_DEVICE);
        assert_eq!(hex(&x360::DEVICE), X360_DEVICE);
        assert_eq!(hex(&ds4::DEVICE), DS4_DEVICE);
        assert_eq!(hex(&x360::configuration()), X360_CONFIGURATION);
        assert_eq!(hex(&ds4::configuration()), DS4_CONFIGURATION);
        assert_eq!(hex(&configuration()), DS_CONFIGURATION);
    }

    const DS_DEVICE: &str = "12010002000000404C05E60C000101020301";
    const X360_DEVICE: &str = "12010002FFFFFF085E048E02140101020301";
    const DS4_DEVICE: &str = "12010002000000404C05C405000101020301";
    const X360_CONFIGURATION: &str = concat!(
        "09023100010100A0FA0904000002FF5D010011210001012581140000000013010800000705810320",
        "000107050103200008",
    );
    const DS4_CONFIGURATION: &str =
        "09022900010100C0FA0904000002030000000921110100012292000705840340000107050303400001";
    const DS_CONFIGURATION: &str = concat!(
        "0902E300040100C0FA0904000000010100000A2401000149000201020C2402010101060433000000",
        "0C24060201010300000000000924030301030402000C240204020403020300000009240605040103",
        "0000092403060101010500090401000001020000090401010101020000072401010101000B240201",
        "0402100180BB00090501098801040000072501000000000904020000010200000904020101010200",
        "00072401060101000B2402010202100180BB0009058205C400040000072501000000000904030002",
        "030000000921110100012211010705840340000407050303400004",
    );

    #[test]
    fn microphone_device_is_a_valid_capture_only_audio_function() {
        let c = mic::configuration();
        assert_eq!(u16::from_le_bytes([c[2], c[3]]) as usize, c.len());
        assert_eq!(c.len(), 109);
        let all = descriptors(&c);
        // Interfaces 0..n-1, each listed with alternate setting 0 first,
        // none of them HID or vendor (no game finds a pad here, no pad
        // driver loads).
        let ifs: Vec<&[u8]> = all.iter().copied().filter(|d| d[1] == 0x04).collect();
        let mut numbers: Vec<u8> = ifs.iter().filter(|d| d[3] == 0).map(|d| d[2]).collect();
        numbers.dedup();
        assert_eq!(numbers, (0..mic::NUM_INTERFACES).collect::<Vec<_>>());
        assert_eq!(c[4], mic::NUM_INTERFACES);
        assert!(ifs.iter().all(|d| d[5] == 0x01), "audio class only");
        assert_eq!((ifs[0][2], ifs[0][6]), (mic::IF_AUDIO_CONTROL, 0x01));
        assert!(ifs[1..].iter().all(|d| d[2] == mic::IF_CAPTURE && d[6] == 0x02));
        // The audio control header covers exactly the class descriptors
        // that follow it and names the streaming interface.
        let ac: Vec<&[u8]> = all[2..].iter().copied().take_while(|d| d[1] == 0x24).collect();
        let header = ac[0];
        assert_eq!(header[2], 0x01);
        let total: usize = ac.iter().map(|d| d.len()).sum();
        assert_eq!(u16::from_le_bytes([header[5], header[6]]) as usize, total);
        assert_eq!(&header[7..], &[1, mic::IF_CAPTURE]);
        // Topology: headset mic (4) -> feature unit (5) -> USB stream (6),
        // and no terminal points at one that isn't there.
        let ids: Vec<u8> = ac[1..].iter().map(|d| d[3]).collect();
        assert_eq!(ids, [4, 5, 6]);
        assert_eq!(ac[1][2], 0x02);
        assert_eq!(u16::from_le_bytes([ac[1][4], ac[1][5]]), 0x0402);
        assert_eq!(ac[1][6], 0, "no associated terminal");
        assert_eq!((ac[2][2], ac[2][3], ac[2][4]), (0x06, UNIT_MIC, 4));
        assert_eq!((ac[3][2], ac[3][7]), (0x03, UNIT_MIC));
        assert_eq!(u16::from_le_bytes([ac[3][4], ac[3][5]]), 0x0101);
        assert_eq!(ac[3][6], 0, "no associated terminal");
        // One ISO IN endpoint, the virtual DualSense's capture endpoint.
        let eps: Vec<&[u8]> = all.iter().copied().filter(|d| d[1] == 0x05).collect();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0][2] as u32, 0x80 | mic::EP_CAPTURE);
        assert_eq!(eps[0][3] & 0x03, 0x01);
        // The streaming interface and endpoint are byte for byte the
        // virtual DualSense's capture, so the same clock serves both.
        let ds = configuration();
        let tail = &c[c.len() - 52..];
        let start = ds.windows(9).position(|w| w == [0x09, 0x04, IF_CAPTURE, 0x00, 0x00, 0x01, 0x02, 0x00, 0x00]).unwrap();
        let mut ds_tail = ds[start..start + 52].to_vec();
        ds_tail[2] = mic::IF_CAPTURE;
        ds_tail[11] = mic::IF_CAPTURE;
        assert_eq!(tail, &ds_tail[..]);
        assert_eq!(u16::from_le_bytes([eps[0][4], eps[0][5]]) as usize, CAPTURE_PACKET + 4);
    }

    #[test]
    fn strings_are_utf16() {
        let s = string(2).unwrap();
        assert_eq!(s[0] as usize, s.len());
        assert_eq!(s[2], b'W');
    }
}
