//! DualShock 4 wire protocol: identification, input reports, and the
//! rumble / lightbar output report.
//!
//! Layouts follow the report structure documented by Linux's
//! `hid-playstation` driver and SDL's HIDAPI PS4 driver. This is an
//! independent implementation of that behavior. The DualShock 4 shares
//! [`crate::InputState`] with the DualSense; touch Y is scaled from the
//! DualShock 4's 942 to the DualSense's 1080 so the rest of the app,
//! including the virtual pad, keeps one coordinate space.

use crate::crc::{self, PREFIX_INPUT, PREFIX_OUTPUT};
use crate::input::{BatteryStatus, Button, Buttons, InputState, ParseError, TouchPoint};
use crate::output::OutputState;
use crate::{Connection, Model, DUALSENSE_EDGE_PID, DUALSENSE_PID, SONY_VID};

/// USB input report `0x01`.
pub const USB_INPUT_LEN: usize = 64;
/// Bluetooth input/output report `0x11`, not counting Windows' padding.
pub const BT_REPORT_LEN: usize = 78;
/// USB output report `0x05`.
pub const USB_OUTPUT_LEN: usize = 32;
/// Short Bluetooth report `0x01`: the id plus the first 9 block bytes.
pub const SHORT_INPUT_LEN: usize = 10;

const TOUCH_H_DS4: u32 = 942;
const TOUCH_H_DS: u32 = 1080;
/// 16-bit DualShock 4 timestamp, in DualSense ticks of 1/3 µs (one DualShock 4
/// tick is 16/3 µs).
const TS_SPAN: u32 = 65536 * 16;

/// What [`identify`] knows about a VID/PID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerKind {
    pub model: Model,
    /// Gyro and accelerometer are present. Pads without them stay out of the
    /// gyro mouse so a stuck sensor cannot move the cursor.
    pub has_motion: bool,
    /// Lightbar and rumble output is safe to send.
    pub has_output: bool,
}

struct Known {
    vid: u16,
    pid: u16,
    model: Model,
    motion: bool,
    output: bool,
}

/// Gamepads that speak the DualShock 4 input report. Arcade sticks, wheels,
/// and XInput-mode ids are not here: XInput pads are already an Xbox
/// controller to Windows.
const KNOWN: &[Known] = &[
    // Sony.
    Known {
        vid: SONY_VID,
        pid: 0x05C4,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: SONY_VID,
        pid: 0x09CC,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    // Strike Pack grip, and adapters that reuse this Sony PID.
    Known {
        vid: SONY_VID,
        pid: 0x05C5,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: SONY_VID,
        pid: 0x0BA0,
        model: Model::DualShock4Dongle,
        motion: true,
        output: true,
    },
    // Zeroplus and the generic pads that use its id.
    Known {
        vid: 0x0C12,
        pid: 0x0E10,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: 0x0C12,
        pid: 0x0E15,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: 0x0C12,
        pid: 0x0E20,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    // Lightbar, no gyro and no touchpad.
    Known {
        vid: 0x0C12,
        pid: 0x1E10,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x0C12,
        pid: 0x1E1C,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    // HORI.
    Known {
        vid: 0x0F0D,
        pid: 0x0055,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x0F0D,
        pid: 0x0066,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    // No lightbar and no rumble; writes are not attempted.
    Known {
        vid: 0x0F0D,
        pid: 0x00EE,
        model: Model::DualShock4,
        motion: false,
        output: false,
    },
    // Nacon Revolution, the pads that stay in DualShock 4 mode.
    Known {
        vid: 0x146B,
        pid: 0x0D01,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: 0x146B,
        pid: 0x0D02,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: 0x146B,
        pid: 0x0D10,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: 0x146B,
        pid: 0x0D13,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    // Razer Raiju. No gyro; some revisions send the USB report over Bluetooth.
    Known {
        vid: 0x1532,
        pid: 0x1000,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x1532,
        pid: 0x1004,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x1532,
        pid: 0x1007,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x1532,
        pid: 0x1008,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x1532,
        pid: 0x1009,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x1532,
        pid: 0x100A,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x7545,
        pid: 0x0104,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: 0x11C0,
        pid: 0x4001,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    Known {
        vid: 0x9886,
        pid: 0x0025,
        model: Model::DualShock4,
        motion: true,
        output: true,
    },
    // No gyro. The lightbar does not follow writes, but writes do not hang it.
    Known {
        vid: 0x044F,
        pid: 0xD00E,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
    Known {
        vid: 0x20D6,
        pid: 0x792A,
        model: Model::DualShock4,
        motion: false,
        output: true,
    },
];

/// DualSense, DualSense Edge, DualShock 4, the USB adapter, or a known clone.
pub fn identify(vid: u16, pid: u16) -> Option<ControllerKind> {
    if vid == SONY_VID && (pid == DUALSENSE_PID || pid == DUALSENSE_EDGE_PID) {
        return Some(ControllerKind {
            model: Model::from_pid(pid).unwrap(),
            has_motion: true,
            has_output: true,
        });
    }
    KNOWN
        .iter()
        .find(|k| k.vid == vid && k.pid == pid)
        .map(|k| ControllerKind {
            model: k.model,
            has_motion: k.motion,
            has_output: k.output,
        })
}

/// Cheap path check before a device is opened: Sony, or a VID from [`KNOWN`].
pub fn hid_path_matches(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    // "054c" anywhere keeps the previous Sony match, including path spellings
    // that are neither `vid_054c` nor `vid&0002054c`.
    path.contains("054c")
        || KNOWN.iter().any(|k| {
            let vid = format!("{:04x}", k.vid);
            path.contains(&format!("vid_{vid}")) || path.contains(&format!("vid&0002{vid}"))
        })
}

/// USB when the HID input report is 64 bytes (the wired pad and the Sony
/// adapter). Any other size is Bluetooth, where Windows delivers a larger
/// buffer with the 78-byte report at the front. The adapter is always USB.
pub fn connection(model: Model, input_len: usize) -> Connection {
    if model == Model::DualShock4Dongle || input_len == USB_INPUT_LEN {
        Connection::Usb
    } else {
        Connection::Bluetooth
    }
}

/// Continue a 16-bit DualShock 4 timestamp (`sample`, already in 1/3 µs
/// ticks) into the running DualSense clock. `prev` is `None` on the first
/// report.
pub fn unwrap_timestamp(prev: Option<u32>, sample: u32) -> u32 {
    let sample = sample % TS_SPAN;
    let Some(prev) = prev else {
        return sample;
    };
    let prev_mod = prev % TS_SPAN;
    let delta = if sample >= prev_mod {
        sample - prev_mod
    } else {
        TS_SPAN - prev_mod + sample
    };
    prev.wrapping_add(delta)
}

/// Factory calibration looks like a real gyro and accelerometer span.
///
/// USB feature `0x02` stores each axis as plus then minus. Bluetooth feature
/// `0x05` stores the three plus values, then the three minus values. Both
/// start with pitch, yaw, and roll bias. A clone that returns zeros, or a
/// read that came back as a different report, fails this and the gyro keeps
/// the software bias.
pub fn calibration_plausible(report: &[u8], bluetooth: bool) -> bool {
    let need = if bluetooth { 41 } else { 37 };
    let id = if bluetooth { 0x05 } else { 0x02 };
    if report.len() < need || report[0] != id {
        return false;
    }
    let word = |i: usize| i16::from_le_bytes([report[i], report[i + 1]]) as i32;
    let bias = [word(1), word(3), word(5)];
    let (plus, minus) = if bluetooth {
        ([word(7), word(9), word(11)], [word(13), word(15), word(17)])
    } else {
        ([word(7), word(11), word(15)], [word(9), word(13), word(17)])
    };
    if word(19) <= 0 || word(21) <= 0 {
        return false;
    }
    for i in 0..3 {
        let denom = (plus[i] - bias[i]).unsigned_abs() + (minus[i] - bias[i]).unsigned_abs();
        if denom == 0 || denom > 20_000 {
            return false;
        }
    }
    for i in 0..3 {
        let range = word(23 + 4 * i) - word(25 + 4 * i);
        if !(1_000..=20_000).contains(&range) {
            return false;
        }
    }
    true
}

/// Parse a DualShock 4 input report into the shared [`InputState`].
pub fn parse_input(
    report: &[u8],
    conn: Connection,
    model: Model,
    check_crc: bool,
) -> Result<InputState, ParseError> {
    if report.is_empty() {
        return Err(ParseError::TooShort);
    }
    match (conn, report[0]) {
        (Connection::Bluetooth, 0x11..=0x19) => {
            if report.len() < BT_REPORT_LEN {
                return Err(ParseError::TooShort);
            }
            // Bit 7 set: this packet carries the pad, not a bare radio frame.
            if report[1] & 0x80 == 0 {
                return Err(ParseError::NotState(report[1]));
            }
            if check_crc && !crc::verify(PREFIX_INPUT, &report[..BT_REPORT_LEN]) {
                return Err(ParseError::BadCrc);
            }
            parse_block(report, 3, model)
        }
        (Connection::Bluetooth, 0x01) => {
            if report.len() >= USB_INPUT_LEN && tail_present(report) {
                parse_block(report, 1, model)
            } else if report.len() >= SHORT_INPUT_LEN {
                Ok(parse_short(report))
            } else {
                Err(ParseError::TooShort)
            }
        }
        (Connection::Usb, 0x01) => {
            if report.len() < USB_INPUT_LEN {
                return Err(ParseError::TooShort);
            }
            parse_block(report, 1, model)
        }
        (_, id) => Err(ParseError::UnknownReport(id)),
    }
}

/// A Bluetooth `0x01` buffer that is more than the 9-byte header. Windows
/// pads the short report with zeros; a USB-shaped report has a battery
/// byte, motion, or a touch sample in that tail.
fn tail_present(report: &[u8]) -> bool {
    report
        .get(10..44)
        .is_some_and(|t| t.iter().any(|b| *b != 0))
}

fn parse_short(r: &[u8]) -> InputState {
    let mut s = InputState {
        lx: r[1],
        ly: r[2],
        rx: r[3],
        ry: r[4],
        l2: r[8],
        r2: r[9],
        seq: r[7] >> 2,
        full: false,
        motion_valid: false,
        ..Default::default()
    };
    s.buttons = buttons(r[5], r[6], r[7], s.l2, s.r2);
    s
}

fn parse_block(r: &[u8], o: usize, model: Model) -> Result<InputState, ParseError> {
    if r.len() < o + 32 {
        return Err(ParseError::TooShort);
    }
    let at = |i: usize| r[o + i];
    // Adapter with nothing paired: the sticks in this report are zeros, and
    // feeding them through would slam a virtual pad to the corner.
    if model == Model::DualShock4Dongle && at(30) & 0x04 != 0 {
        return Err(ParseError::NotConnected);
    }
    let mut s = InputState {
        lx: at(0),
        ly: at(1),
        rx: at(2),
        ry: at(3),
        l2: at(7),
        r2: at(8),
        seq: at(6) >> 2,
        full: true,
        motion_valid: true,
        ..Default::default()
    };
    s.buttons = buttons(at(4), at(5), at(6), s.l2, s.r2);
    s.sensor_ts = u16::from_le_bytes([at(9), at(10)]) as u32 * 16;
    for k in 0..3 {
        s.gyro[k] = i16::from_le_bytes([at(12 + 2 * k), at(13 + 2 * k)]);
        s.accel[k] = i16::from_le_bytes([at(18 + 2 * k), at(19 + 2 * k)]);
    }
    let (pct, status) = battery(at(29));
    s.battery_percent = pct;
    s.battery_status = status;

    if r.len() > o + 32 {
        let max_samples = if o == 1 { 3 } else { 4 };
        let n = (at(32) as usize).min(max_samples);
        if n > 0 {
            let base = o + 33 + (n - 1) * 9;
            if base + 9 <= r.len() {
                s.touch[0] = touch_point(&r[base + 1..base + 5])?;
                s.touch[1] = touch_point(&r[base + 5..base + 9])?;
            }
        }
    }
    Ok(s)
}

fn buttons(hat_face: u8, shoulders: u8, special: u8, l2: u8, r2: u8) -> Buttons {
    let mut b = Buttons::default();
    let (u, right, d, l) = match hat_face & 0x0F {
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
    b.set(Button::DpadUp, u);
    b.set(Button::DpadRight, right);
    b.set(Button::DpadDown, d);
    b.set(Button::DpadLeft, l);
    b.set(Button::Square, hat_face & 0x10 != 0);
    b.set(Button::Cross, hat_face & 0x20 != 0);
    b.set(Button::Circle, hat_face & 0x40 != 0);
    b.set(Button::Triangle, hat_face & 0x80 != 0);
    b.set(Button::L1, shoulders & 0x01 != 0);
    b.set(Button::R1, shoulders & 0x02 != 0);
    b.set(Button::Create, shoulders & 0x10 != 0);
    b.set(Button::Options, shoulders & 0x20 != 0);
    b.set(Button::L3, shoulders & 0x40 != 0);
    b.set(Button::R3, shoulders & 0x80 != 0);
    b.set(Button::Ps, special & 0x01 != 0);
    b.set(Button::Touchpad, special & 0x02 != 0);
    // The analog byte is the button. Some clones only set the digital bit.
    b.set(Button::L2, l2 > 0 || shoulders & 0x04 != 0);
    b.set(Button::R2, r2 > 0 || shoulders & 0x08 != 0);
    b
}

fn battery(status0: u8) -> (u8, BatteryStatus) {
    let cap = status0 & 0x0F;
    let cable = status0 & 0x10 != 0;
    if !cable {
        let pct = if cap >= 10 { 100 } else { cap * 10 };
        return (pct, BatteryStatus::Discharging);
    }
    match cap {
        11 => (100, BatteryStatus::Full),
        14 | 15 => (0, BatteryStatus::Error),
        10 => (100, BatteryStatus::Charging),
        n => (n * 10, BatteryStatus::Charging),
    }
}

fn touch_point(b: &[u8]) -> Result<TouchPoint, ParseError> {
    let active = b[0] & 0x80 == 0;
    if !active {
        return Ok(TouchPoint {
            active: false,
            id: b[0] & 0x7F,
            x: 0,
            y: 0,
        });
    }
    let x = b[1] as u16 | ((b[2] as u16 & 0x0F) << 8);
    let y = (b[2] as u16 >> 4) | ((b[3] as u16) << 4);
    // A little past the 1920 × 942 pad is allowed; well off it is not a
    // real sample.
    if x > 1950 || y > 980 {
        return Err(ParseError::Implausible);
    }
    let y = (y as u32 * TOUCH_H_DS / TOUCH_H_DS4) as u16;
    Ok(TouchPoint {
        active: true,
        id: b[0] & 0x7F,
        x,
        y,
    })
}

fn put_touch(dst: &mut [u8], t: &TouchPoint) {
    dst[0] = if t.active { t.id & 0x7F } else { 0x80 };
    dst[1] = t.x as u8;
    dst[2] = ((t.x >> 8) as u8 & 0x0F) | ((t.y as u8 & 0x0F) << 4);
    dst[3] = (t.y >> 4) as u8;
}

fn battery_byte(s: &InputState) -> u8 {
    let nibble = (s.battery_percent / 10).min(10);
    match s.battery_status {
        BatteryStatus::Discharging => nibble,
        BatteryStatus::Charging => 0x10 | nibble,
        BatteryStatus::Full => 0x2A,
        BatteryStatus::Error => 0xF0,
    }
}

/// DualSense USB report `0x01` carrying this pad's motion, touch, battery,
/// and timestamp, for the virtual-pad path that already speaks that layout.
/// Touch Y is already in DualSense units. Motion is omitted when it is not
/// valid, so a pad without a gyro does not publish zeros as a reading.
pub fn as_dualsense_usb(s: &InputState) -> [u8; 64] {
    let mut r = [0u8; 64];
    r[0] = 0x01;
    r[1] = s.lx;
    r[2] = s.ly;
    r[3] = s.rx;
    r[4] = s.ry;
    r[5] = s.l2;
    r[6] = s.r2;
    r[7] = s.seq;
    if s.motion_valid {
        for k in 0..3 {
            r[16 + 2 * k..18 + 2 * k].copy_from_slice(&s.gyro[k].to_le_bytes());
            r[22 + 2 * k..24 + 2 * k].copy_from_slice(&s.accel[k].to_le_bytes());
        }
    }
    r[28..32].copy_from_slice(&s.sensor_ts.to_le_bytes());
    put_touch(&mut r[33..37], &s.touch[0]);
    put_touch(&mut r[37..41], &s.touch[1]);
    r[53] = battery_byte(s);
    r
}

/// Rumble and lightbar output. USB is report `0x05` (32 bytes); Bluetooth is
/// report `0x11` (78 bytes) with the CRC at bytes 74..77.
///
/// Both enable bits are always set. A clone that is sent rumble without the
/// lightbar bit, or the lightbar without rumble, drops the other one.
/// Adaptive triggers, player LEDs, and the mute LED have nowhere to go.
///
/// Bluetooth byte 1 is `0xC4`: HID data, CRC present, and a 4 ms poll, which
/// is the wired rate. `out` must hold the report.
pub fn output_report<'a>(state: &OutputState, conn: Connection, out: &'a mut [u8]) -> &'a [u8] {
    output_report_with_audio(state, conn, AudioOut::Untouched, out)
}

/// [`output_report`] that also sets the Bluetooth audio fields. With
/// [`AudioOut::Untouched`] the report is exactly [`output_report`]'s; over
/// USB `audio` is ignored (there the pad is a USB audio device to Windows).
pub fn output_report_with_audio<'a>(
    state: &OutputState,
    conn: Connection,
    audio: AudioOut,
    out: &'a mut [u8],
) -> &'a [u8] {
    let (n, flag, motor, led) = match conn {
        Connection::Usb => {
            out[..USB_OUTPUT_LEN].fill(0);
            out[0] = 0x05;
            (USB_OUTPUT_LEN, 1, 4, 6)
        }
        Connection::Bluetooth => {
            out[..BT_REPORT_LEN].fill(0);
            out[0] = 0x11;
            out[1] = 0xC0 | BT_POLL_MS;
            (BT_REPORT_LEN, 3, 6, 8)
        }
    };
    out[flag] = 0x01 | 0x02;
    if state.rumble_valid {
        out[motor] = state.rumble_right;
        out[motor + 1] = state.rumble_left;
    }
    out[led] = state.lightbar[0];
    out[led + 1] = state.lightbar[1];
    out[led + 2] = state.lightbar[2];
    if conn == Connection::Bluetooth {
        if let Some((mode, headphones, speaker)) = audio.fields() {
            out[2] = mode;
            out[flag] |= AUDIO_VOLUME_FLAGS;
            out[21] = headphones;
            out[22] = headphones;
            out[23] = 0; // microphone
            out[24] = speaker;
        }
        crc::seal(PREFIX_OUTPUT, &mut out[..n]);
    }
    &out[..n]
}

// ------------------------------------------------------------ Bluetooth audio
//
// Speaker and headphone audio over Bluetooth, as DS4 Sound Link
// (https://github.com/abh1shekbana/DS4-Sound-Link, by abh1shekbana, MIT)
// sends it: report `0x11` arms the audio path with byte 2 `0xA0` and the
// volume fields, then report `0x17` carries four SBC frames (16 ms of
// 32 kHz audio) to the speaker (`0x02`) or the headphone jack (`0x24`).
// Byte 2 `0xA0` rather than `0xA2` keeps the controller sending input while
// audio plays, per DS4 Sound Link's protocol notes. The `0x17` layout is the
// one asimonf's DS4AudioStreamer proof of concept sends (GPL-2.0, read as
// documentation). DS4 Sound Link's full notice is in
// `crates/ds-proto/DS4-Sound-Link-LICENSE`.

/// Bluetooth poll interval in ms, the low bits of the header byte.
pub const BT_POLL_MS: u8 = 4;
/// Report `0x17`: header, four SBC frames, CRC.
pub const AUDIO_REPORT_LEN: usize = 462;
/// SBC frames in one `0x17` report.
pub const AUDIO_FRAMES: usize = 4;
pub const AUDIO_RATE: u32 = 32_000;
/// One `0x17` report's worth of audio: 512 frames at 32 kHz.
pub const AUDIO_PERIOD_NS: u64 = 16_000_000;
/// Highest headphone or speaker volume byte.
pub const AUDIO_VOLUME_MAX: u8 = 127;
const AUDIO_MODE_ON: u8 = 0xA0;
/// Valid flags for the left and right headphone, microphone, and speaker
/// volume bytes.
const AUDIO_VOLUME_FLAGS: u8 = 0xF0;
const TARGET_SPEAKER: u8 = 0x02;
const TARGET_HEADPHONES: u8 = 0x24;

/// The audio fields of the Bluetooth `0x11` report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioOut {
    /// Not written: the report is the plain rumble and lightbar report.
    #[default]
    Untouched,
    /// Audio path off and both volumes 0.
    Off,
    /// Speaker at this volume byte, headphones silent.
    Speaker { volume: u8 },
    /// Headphone jack at this volume byte, speaker silent.
    Headphones { volume: u8 },
}

impl AudioOut {
    /// (byte 2, headphone volume, speaker volume), or `None` for untouched.
    fn fields(self) -> Option<(u8, u8, u8)> {
        let v = |x: u8| x.min(AUDIO_VOLUME_MAX);
        match self {
            AudioOut::Untouched => None,
            AudioOut::Off => Some((0, 0, 0)),
            AudioOut::Speaker { volume } => Some((AUDIO_MODE_ON, 0, v(volume))),
            AudioOut::Headphones { volume } => Some((AUDIO_MODE_ON, v(volume), 0)),
        }
    }

    pub fn is_on(self) -> bool {
        matches!(self, AudioOut::Speaker { .. } | AudioOut::Headphones { .. })
    }
}

/// Bluetooth audio is only tried on Sony's own DualShock 4 (v1 and v2);
/// clones and the USB adapter don't take these reports.
pub fn supports_bt_audio(vid: u16, pid: u16) -> bool {
    vid == SONY_VID && matches!(pid, 0x05C4 | 0x09CC)
}

/// Report `0x17`: four SBC frames for the speaker or the headphone jack.
/// `counter` is the running SBC frame number, which goes up by
/// [`AUDIO_FRAMES`] per report. `poll` is the poll interval in the header
/// byte, as in `0x11`. `out` must hold [`AUDIO_REPORT_LEN`] bytes.
pub fn audio_report<'a>(
    counter: u16,
    frames: &[[u8; crate::sbc::FRAME_LEN]; AUDIO_FRAMES],
    headphones: bool,
    poll: u8,
    out: &'a mut [u8],
) -> &'a [u8] {
    let out = &mut out[..AUDIO_REPORT_LEN];
    out.fill(0);
    out[0] = 0x17;
    out[1] = 0x40 | poll.min(16);
    out[2] = AUDIO_MODE_ON;
    out[3..5].copy_from_slice(&counter.to_le_bytes());
    out[5] = if headphones {
        TARGET_HEADPHONES
    } else {
        TARGET_SPEAKER
    };
    for (i, f) in frames.iter().enumerate() {
        let at = 6 + i * crate::sbc::FRAME_LEN;
        out[at..at + crate::sbc::FRAME_LEN].copy_from_slice(f);
    }
    crc::seal(PREFIX_OUTPUT, out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{parse, BatteryStatus, Button};
    use crate::virtual_pad::{ds4_usb_input, PadState};

    fn kind(vid: u16, pid: u16) -> Option<Model> {
        identify(vid, pid).map(|k| k.model)
    }

    #[test]
    fn identifies_sony_and_clones() {
        assert_eq!(kind(0x054C, 0x05C4), Some(Model::DualShock4));
        assert_eq!(kind(0x054C, 0x09CC), Some(Model::DualShock4));
        assert_eq!(kind(0x054C, 0x05C5), Some(Model::DualShock4));
        assert_eq!(kind(0x054C, 0x0BA0), Some(Model::DualShock4Dongle));
        assert_eq!(kind(0x054C, 0x0CE6), Some(Model::DualSense));
        assert_eq!(kind(0x054C, 0x0DF2), Some(Model::DualSenseEdge));
        // DualShock 3 and an Xbox pad are not this protocol.
        assert_eq!(kind(0x054C, 0x0268), None);
        assert_eq!(kind(0x045E, 0x028E), None);
        let knockoff = identify(0x0C12, 0x1E10).unwrap();
        assert!(!knockoff.has_motion);
        assert!(knockoff.has_output);
        let mini = identify(0x0F0D, 0x00EE).unwrap();
        assert!(!mini.has_motion && !mini.has_output);
        let nacon = identify(0x146B, 0x0D01).unwrap();
        assert!(nacon.has_motion && nacon.has_output);
    }

    #[test]
    fn connection_by_report_size() {
        assert_eq!(connection(Model::DualShock4, 64), Connection::Usb);
        assert_eq!(connection(Model::DualShock4, 547), Connection::Bluetooth);
        assert_eq!(connection(Model::DualShock4Dongle, 547), Connection::Usb);
    }

    #[test]
    fn hid_paths() {
        assert!(hid_path_matches(r"\\?\hid#vid_054c&pid_09cc#x"));
        assert!(hid_path_matches(r"\\?\hid#vid&0002054c_pid&000209cc#x"));
        assert!(hid_path_matches(r"hid#054c_09cc"));
        assert!(hid_path_matches(r"\\?\hid#vid_0c12&pid_1e10#x"));
        assert!(!hid_path_matches(r"\\?\hid#vid_045e&pid_028e#x"));
    }

    /// USB block with one touch sample. `status0` is the battery byte.
    fn usb_report() -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[0] = 0x01;
        r[1] = 10;
        r[2] = 20;
        r[3] = 30;
        r[4] = 40;
        r[5] = 0x01 | 0x20; // hat north-east, cross
        r[6] = 0x01 | 0x04; // L1, digital L2 (analog left at 0)
        r[7] = 0x01 | (9 << 2); // PS, counter 9
        r[8] = 0;
        r[9] = 200;
        r[10] = 0x10; // timestamp 0x0010
        r[11] = 0x00;
        r[13] = 0x10; // gyro pitch = 16
        r[19] = 0x00;
        r[20] = 0x20; // accel pitch = 8192
        r[30] = 0x15; // cable, 50%
        r[33] = 1;
        // Point 0: id 3, x 1000, y 471 (half of 942, scales to 540).
        r[35] = 3;
        r[36] = (1000 & 0xFF) as u8;
        r[37] = ((1000 >> 8) as u8) | (((471 & 0x0F) as u8) << 4);
        r[38] = (471 >> 4) as u8;
        r[39] = 0x80;
        r
    }

    #[test]
    fn usb_fields() {
        let s = parse(&usb_report(), Connection::Usb, Model::DualShock4, false).unwrap();
        assert_eq!(
            (s.lx, s.ly, s.rx, s.ry, s.l2, s.r2),
            (10, 20, 30, 40, 0, 200)
        );
        assert!(s.buttons.has(Button::DpadUp) && s.buttons.has(Button::DpadRight));
        assert!(s.buttons.has(Button::Cross) && s.buttons.has(Button::L1));
        assert!(s.buttons.has(Button::L2), "digital L2 counts");
        assert!(s.buttons.has(Button::R2) && s.buttons.has(Button::Ps));
        assert!(!s.buttons.has(Button::Mute));
        assert_eq!(s.seq, 9);
        assert_eq!(s.gyro[0], 16);
        assert_eq!(s.accel[0], 8192);
        assert_eq!(s.sensor_ts, 0x10 * 16);
        assert!(s.touch[0].active);
        assert_eq!(s.touch[0].x, 1000);
        assert_eq!(s.touch[0].y, 540, "942-high pad scaled into 1080");
        assert!(!s.touch[1].active);
        assert_eq!(s.battery_status, BatteryStatus::Charging);
        assert_eq!(s.battery_percent, 50);
        assert!(s.full && s.motion_valid);
    }

    #[test]
    fn battery_levels() {
        let mut r = usb_report();
        r[30] = 0x0A;
        let s = parse(&r, Connection::Usb, Model::DualShock4, false).unwrap();
        assert_eq!(
            (s.battery_percent, s.battery_status),
            (100, BatteryStatus::Discharging)
        );
        r[30] = 0x1B;
        let s = parse(&r, Connection::Usb, Model::DualShock4, false).unwrap();
        assert_eq!(
            (s.battery_percent, s.battery_status),
            (100, BatteryStatus::Full)
        );
        r[30] = 0x1E;
        let s = parse(&r, Connection::Usb, Model::DualShock4, false).unwrap();
        assert_eq!(s.battery_status, BatteryStatus::Error);
    }

    #[test]
    fn bluetooth_matches_usb_and_checks_crc() {
        let usb = usb_report();
        let mut bt = vec![0u8; 78];
        bt[0] = 0x11;
        bt[1] = 0x80;
        // USB block starts at byte 1; Bluetooth starts at byte 3.
        bt[3..3 + 63].copy_from_slice(&usb[1..64]);
        crc::seal(PREFIX_INPUT, &mut bt);
        let a = parse(&usb, Connection::Usb, Model::DualShock4, false).unwrap();
        let b = parse(&bt, Connection::Bluetooth, Model::DualShock4, true).unwrap();
        assert_eq!(a.buttons, b.buttons);
        assert_eq!(a.gyro, b.gyro);
        assert_eq!(a.touch, b.touch);
        assert_eq!(a.battery_percent, b.battery_percent);
        bt[10] ^= 0xFF;
        assert_eq!(
            parse(&bt, Connection::Bluetooth, Model::DualShock4, true),
            Err(ParseError::BadCrc)
        );
        // Padding past the 78-byte report must not move the CRC.
        bt[10] ^= 0xFF;
        let mut padded = bt.clone();
        padded.extend_from_slice(&[0u8; 400]);
        assert!(parse(&padded, Connection::Bluetooth, Model::DualShock4, true).is_ok());
    }

    #[test]
    fn bluetooth_without_the_hid_bit_is_not_state() {
        let mut bt = vec![0u8; 78];
        bt[0] = 0x12; // a later state id, same layout
        bt[1] = 0x00;
        crc::seal(PREFIX_INPUT, &mut bt);
        assert_eq!(
            parse(&bt, Connection::Bluetooth, Model::DualShock4, true),
            Err(ParseError::NotState(0))
        );
    }

    #[test]
    fn short_report_has_buttons_and_no_motion() {
        let mut r = vec![0u8; 10];
        r[0] = 0x01;
        r[1] = 128;
        r[5] = 0x28; // hat released, cross
        r[9] = 40;
        let s = parse(&r, Connection::Bluetooth, Model::DualShock4, true).unwrap();
        assert!(s.buttons.has(Button::Cross) && s.buttons.has(Button::R2));
        assert!(!s.buttons.has(Button::DpadUp));
        assert!(!s.full && !s.motion_valid);
        // Zero-padded out to the Bluetooth buffer size, still the short report.
        r.resize(547, 0);
        let s = parse(&r, Connection::Bluetooth, Model::DualShock4, false).unwrap();
        assert!(!s.full);
    }

    #[test]
    fn usb_shaped_bluetooth_report() {
        let usb = usb_report();
        let s = parse(&usb, Connection::Bluetooth, Model::DualShock4, false).unwrap();
        assert!(s.full);
        assert_eq!(s.gyro[0], 16);
        assert_eq!(s.touch[0].y, 540);
    }

    #[test]
    fn touch_off_the_pad_is_rejected() {
        let mut r = usb_report();
        r[35] = 1;
        r[36] = 0xFF;
        r[37] = 0x0F; // x = 4095
        assert_eq!(
            parse(&r, Connection::Usb, Model::DualShock4, false),
            Err(ParseError::Implausible)
        );
    }

    #[test]
    fn dongle_idle_is_not_a_pad() {
        let mut r = usb_report();
        assert!(parse(&r, Connection::Usb, Model::DualShock4Dongle, false).is_ok());
        r[31] = 0x04;
        assert_eq!(
            parse(&r, Connection::Usb, Model::DualShock4Dongle, false),
            Err(ParseError::NotConnected)
        );
        // The same bit on a normal pad is not the adapter flag.
        assert!(parse(&r, Connection::Usb, Model::DualShock4, false).is_ok());
    }

    #[test]
    fn timestamp_unwraps_across_the_16_bit_wrap() {
        let first = unwrap_timestamp(None, 100 * 16);
        assert_eq!(first, 100 * 16);
        let next = unwrap_timestamp(Some(first), 110 * 16);
        assert_eq!(next, 110 * 16);
        let before = unwrap_timestamp(None, 65534 * 16);
        let after = unwrap_timestamp(Some(before), 1 * 16);
        assert_eq!(after.wrapping_sub(before), 3 * 16);
    }

    fn calib(bluetooth: bool) -> Vec<u8> {
        let mut r = vec![0u8; if bluetooth { 41 } else { 37 }];
        r[0] = if bluetooth { 0x05 } else { 0x02 };
        let put = |r: &mut [u8], i: usize, v: i16| {
            r[i..i + 2].copy_from_slice(&v.to_le_bytes());
        };
        // Bias 0. USB: plus, minus per axis. Bluetooth: all plus, then all minus.
        if bluetooth {
            for i in 0..3 {
                put(&mut r, 7 + 2 * i, 200);
                put(&mut r, 13 + 2 * i, -200);
            }
        } else {
            for i in 0..3 {
                put(&mut r, 7 + 4 * i, 200);
                put(&mut r, 9 + 4 * i, -200);
            }
        }
        put(&mut r, 19, 16);
        put(&mut r, 21, 16);
        for i in 0..3 {
            put(&mut r, 23 + 4 * i, 8192);
            put(&mut r, 25 + 4 * i, -8192);
        }
        r
    }

    #[test]
    fn calibration_rejects_zeros_and_accepts_each_order() {
        assert!(!calibration_plausible(&vec![0u8; 37], false));
        assert!(calibration_plausible(&calib(false), false));
        assert!(calibration_plausible(&calib(true), true));
        // USB order read as Bluetooth (or the reverse) is not a real span
        // for every axis once the plus/minus grouping disagrees... both
        // orders above are symmetric, so check a one-sided USB report: a
        // zero minus on one axis fails, and the Bluetooth grouping of that
        // same byte array is not plausible either.
        // Plus and minus both sitting on the bias: the span is zero.
        let mut usb = calib(false);
        usb[7] = 0;
        usb[8] = 0;
        usb[9] = 0;
        usb[10] = 0;
        assert!(!calibration_plausible(&usb, false));
    }

    #[test]
    fn output_pairs_rumble_and_lightbar() {
        let s = OutputState {
            rumble_left: 0xAA,
            rumble_right: 0x55,
            lightbar: [1, 2, 3],
            ..Default::default()
        };
        let mut buf = [0u8; 96];
        let usb = output_report(&s, Connection::Usb, &mut buf).to_vec();
        assert_eq!(usb.len(), 32);
        assert_eq!(usb[0], 0x05);
        assert_eq!(usb[1], 0x03, "rumble and lightbar bits together");
        assert_eq!(usb[4], 0x55);
        assert_eq!(usb[5], 0xAA);
        assert_eq!(&usb[6..9], &[1, 2, 3]);
        let bt = output_report(&s, Connection::Bluetooth, &mut buf).to_vec();
        assert_eq!(bt.len(), 78);
        assert_eq!(&bt[..2], &[0x11, 0xC4]);
        assert_eq!(bt[3], 0x03);
        assert_eq!(bt[6], 0x55);
        assert_eq!(bt[7], 0xAA);
        assert_eq!(&bt[8..11], &[1, 2, 3]);
        assert!(crc::verify(PREFIX_OUTPUT, &bt));
        // Trigger bytes from the DualSense state are not in this report.
        let s = OutputState {
            rumble_valid: false,
            ..s
        };
        let usb = output_report(&s, Connection::Usb, &mut buf);
        assert_eq!(usb[1], 0x03);
        assert_eq!(usb[4], 0);
        assert_eq!(usb[5], 0);
    }

    #[test]
    fn dualsense_image_roundtrips_touch_through_the_virtual_pad() {
        let s = parse(&usb_report(), Connection::Usb, Model::DualShock4, false).unwrap();
        let image = as_dualsense_usb(&s);
        assert_eq!(image[16], 0x10);
        assert_eq!(
            u32::from_le_bytes(image[28..32].try_into().unwrap()),
            s.sensor_ts
        );
        let virt = ds4_usb_input(&PadState::default(), Some(&image), true, 0);
        let y = (virt[37] as u16 >> 4) | ((virt[38] as u16) << 4);
        assert_eq!(
            y, 471,
            "scaled up for the DualSense image, back down for a virtual DualShock 4"
        );
    }

    // Bytes from DS4 Sound Link's own builders
    // (DualShock4BluetoothAudioProtocol, commit 8267b6e), run under .NET 8:
    // the 0x17 reports carry the four tone frames pinned in `sbc::tests`.
    const DS4SL_17_HEADPHONES_1234: &str = "1740a03412249c7f306c14d9966666c765453480207d557fbb80dd7d5580bb85a4855583c38a2484918b4bb372c206aad6e0c09219f00c39a38555f6c3189e8155c43bc55f7d5571bbe79f7d5525bb3a9f7d55033b181f7d5519bbc51f7d555f3be79f7d55b3bb3a9f7d55f03b181f7d55f8bb9c7f303830d3000000c0000000c55b9832a6a1e7f0b91e63953aa2528add49180d35810a45c55b98056d21e7f0b51601253aa252ab2c09180d31bb3555c55b983ec161e7f0b5342f953aa252a05989180d358c5a85c55b98015f61e7f0b50472153aa252942b09180d31a951159c7f303830d3000000c0000000c55b983a3161e7f0b53f0a953aa252b5a489180d35a24f95c55b980deaa1e7f0b901d2553aa252839309180d35926015c55b98276d21e7f0b53912653aa252bf3449180d35b70495c55b982443a1e7f0b90f8d953aa252826349180d3582d0159c7f303830d3000000c0000000c55b9810a221e7f0b52581253aa252b7d949180d35bf3e55c55b98384ca1e7f0b92632953aa252914189180d35831345c55b98022b61e7f0b50ef5253aa252a39009180d31b68855c55b983f2961e7f0b5397b953aa252a81c89180d35930385000000000000000000000000000000005718a0d5";
    const DS4SL_17_SPEAKER_8_POLL4: &str = "1744a00800029c7f306c14d9966666c765453480207d557fbb80dd7d5580bb85a4855583c38a2484918b4bb372c206aad6e0c09219f00c39a38555f6c3189e8155c43bc55f7d5571bbe79f7d5525bb3a9f7d55033b181f7d5519bbc51f7d555f3be79f7d55b3bb3a9f7d55f03b181f7d55f8bb9c7f303830d3000000c0000000c55b9832a6a1e7f0b91e63953aa2528add49180d35810a45c55b98056d21e7f0b51601253aa252ab2c09180d31bb3555c55b983ec161e7f0b5342f953aa252a05989180d358c5a85c55b98015f61e7f0b50472153aa252942b09180d31a951159c7f303830d3000000c0000000c55b983a3161e7f0b53f0a953aa252b5a489180d35a24f95c55b980deaa1e7f0b901d2553aa252839309180d35926015c55b98276d21e7f0b53912653aa252bf3449180d35b70495c55b982443a1e7f0b90f8d953aa252826349180d3582d0159c7f303830d3000000c0000000c55b9810a221e7f0b52581253aa252b7d949180d35bf3e55c55b98384ca1e7f0b92632953aa252914189180d35831345c55b98022b61e7f0b50ef5253aa252a39009180d31b68855c55b983f2961e7f0b5397b953aa252a81c89180d35930385000000000000000000000000000000003f3b7fbe";
    const DS4SL_11_ARM_HEADPHONES_110: &str = "11c0a0f300000000000000000000000000000000006e6e0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001977aebd";
    const DS4SL_11_ARM_SPEAKER_90: &str = "11c0a0f300000000000000000000000000000000000000005a00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000d1ab0882";
    const DS4SL_11_DISARM: &str = "11c000f000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000858e4bfe";

    fn tone_frames() -> [[u8; crate::sbc::FRAME_LEN]; AUDIO_FRAMES] {
        let mut e = crate::sbc::Encoder::new();
        let mut frames = [[0u8; crate::sbc::FRAME_LEN]; AUDIO_FRAMES];
        for (i, f) in frames.iter_mut().enumerate() {
            let (l, r) = crate::sbc::tests::tone(i);
            e.encode(&l, &r, f);
        }
        frames
    }

    #[test]
    fn audio_report_matches_ds4_sound_link() {
        use crate::sbc::tests::unhex;
        let frames = tone_frames();
        let mut buf = [0u8; 600];
        let r = audio_report(0x1234, &frames, true, 0, &mut buf).to_vec();
        assert_eq!(r, unhex(DS4SL_17_HEADPHONES_1234));
        let r = audio_report(8, &frames, false, 4, &mut buf).to_vec();
        assert_eq!(r, unhex(DS4SL_17_SPEAKER_8_POLL4));
        assert_eq!(&r[..6], &[0x17, 0x44, 0xA0, 8, 0, 0x02]);
        assert!(crc::verify(PREFIX_OUTPUT, &r));
    }

    #[test]
    fn audio_fields_match_ds4_sound_link_arming() {
        use crate::sbc::tests::unhex;
        // DS4 Sound Link arms with rumble and the lightbar zeroed and a 0 ms
        // poll; DSZ keeps its own poll byte and CRC, everything else is theirs.
        let dark = OutputState {
            lightbar: [0, 0, 0],
            ..Default::default()
        };
        let mut buf = [0u8; 96];
        for (audio, want) in [
            (AudioOut::Headphones { volume: 110 }, DS4SL_11_ARM_HEADPHONES_110),
            (AudioOut::Speaker { volume: 90 }, DS4SL_11_ARM_SPEAKER_90),
        ] {
            let r = output_report_with_audio(&dark, Connection::Bluetooth, audio, &mut buf).to_vec();
            let want = unhex(want);
            assert_eq!(r.len(), want.len());
            assert_eq!(r[1], 0xC4);
            assert_eq!(&r[2..74], &want[2..74], "{audio:?}");
            assert!(crc::verify(PREFIX_OUTPUT, &r));
        }
        // Off: their disarm, with DSZ's rumble and lightbar bits still set.
        let r = output_report_with_audio(&dark, Connection::Bluetooth, AudioOut::Off, &mut buf).to_vec();
        let want = unhex(DS4SL_11_DISARM);
        assert_eq!(r[2], want[2]);
        assert_eq!(r[3], want[3] | 0x03);
        assert_eq!(&r[4..74], &want[4..74]);
        // Volumes are capped at the field's range.
        let r = output_report_with_audio(&dark, Connection::Bluetooth, AudioOut::Speaker { volume: 255 }, &mut buf);
        assert_eq!(r[24], AUDIO_VOLUME_MAX);
    }

    #[test]
    fn default_output_is_byte_for_byte_unchanged() {
        use crate::sbc::tests::unhex;
        // The 0x11 / 0x05 reports DSZ sent before Bluetooth audio existed.
        let s = OutputState {
            rumble_left: 0xAA,
            rumble_right: 0x55,
            lightbar: [1, 2, 3],
            ..Default::default()
        };
        let mut buf = [0u8; 96];
        let bt = "11c40003000055aa010203000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000912db8c3";
        assert_eq!(output_report(&s, Connection::Bluetooth, &mut buf).to_vec(), unhex(bt));
        assert_eq!(
            output_report_with_audio(&s, Connection::Bluetooth, AudioOut::Untouched, &mut buf).to_vec(),
            unhex(bt)
        );
        let usb = output_report(&s, Connection::Usb, &mut buf).to_vec();
        let mut want = vec![0u8; 32];
        want[0] = 0x05;
        want[1] = 0x03;
        want[4] = 0x55;
        want[5] = 0xAA;
        want[6..9].copy_from_slice(&[1, 2, 3]);
        assert_eq!(usb, want);
        // USB never carries the audio fields.
        let on = AudioOut::Headphones { volume: 100 };
        assert_eq!(output_report_with_audio(&s, Connection::Usb, on, &mut buf).to_vec(), want);
    }

    #[test]
    fn bluetooth_audio_only_on_sony_pads() {
        assert!(supports_bt_audio(0x054C, 0x05C4));
        assert!(supports_bt_audio(0x054C, 0x09CC));
        assert!(!supports_bt_audio(0x054C, 0x0BA0), "the USB adapter");
        assert!(!supports_bt_audio(0x054C, 0x0CE6), "DualSense");
        assert!(!supports_bt_audio(0x0C12, 0x0E10), "a clone");
    }
}
