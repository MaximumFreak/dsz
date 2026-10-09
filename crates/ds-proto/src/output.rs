//! Classic control reports: USB `0x02` and Bluetooth `0x31`.
//!
//! One `OutputState` builds either report. The body is the 47-byte common
//! block of Linux's `hid-playstation` (`dualsense_output_report_common`).
//! USB puts it after the report id; Bluetooth after `0x31, 0x02`, as
//! pydualsense (MIT) writes it, with a CRC-32 at byte 74. Field meanings
//! not in Linux (player LED brightness, motor power) are pydualsense's and
//! Monado's `pssense_protocol.h` (Boost).

use crate::{crc, trigger, Connection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputState {
    /// Classic rumble. Left is the heavy motor.
    pub rumble_left: u8,
    pub rumble_right: u8,
    /// When false the motor fields are not marked valid, so the firmware
    /// leaves the actuators alone (needed while PCM haptics stream).
    pub rumble_valid: bool,
    pub right_trigger: trigger::EffectBytes,
    pub left_trigger: trigger::EffectBytes,
    pub lightbar: [u8; 3],
    /// Player LED bits 0..=4.
    pub player_leds: u8,
    /// 0 high, 1 medium, 2 low.
    pub led_brightness: u8,
    /// 0 off, 1 on, 2 pulse.
    pub mute_led: u8,
    /// Rumble strength reduction 0..=7 in 12.5 % steps, 0 is full strength.
    pub rumble_reduction: u8,
    /// Improved rumble emulation (firmware 2.24+).
    pub enhanced_rumble: bool,
}

impl Default for OutputState {
    fn default() -> Self {
        OutputState {
            rumble_left: 0,
            rumble_right: 0,
            rumble_valid: true,
            right_trigger: trigger::OFF,
            left_trigger: trigger::OFF,
            lightbar: [0, 0, 255],
            player_leds: 0x04,
            led_brightness: 0,
            mute_led: 0,
            rumble_reduction: 0,
            enhanced_rumble: true,
        }
    }
}

/// `lightbar_setup`: fade out the startup light and take the color we send
/// (Linux's `DS_OUTPUT_LIGHTBAR_SETUP_LIGHT_OUT`).
const LIGHTBAR_LIGHT_OUT: u8 = 0x02;

impl OutputState {
    /// Write the 47-byte common block into `b`.
    fn write_body(&self, b: &mut [u8]) {
        // valid_flag0: bit0 compatible vibration, bit1 haptics select,
        // bit2 right trigger, bit3 left trigger.
        let mut flag0 = 0x04 | 0x08;
        if self.rumble_valid {
            flag0 |= 0x01 | 0x02;
        }
        // valid_flag1: bit0 mute LED, bit2 lightbar, bit4 player LEDs,
        // bit6 motor power.
        let flag1 = 0x01 | 0x04 | 0x10 | 0x40;
        b[0] = flag0;
        b[1] = flag1;
        b[2] = self.rumble_right;
        b[3] = self.rumble_left;
        b[8] = self.mute_led.min(2);
        // Power-save byte stays 0 and its enable flag unset. Tested on an
        // Edge over Bluetooth: the motion bit does not stop the sensors, so
        // there is no battery to gain here.
        b[9] = 0;
        b[10..21].copy_from_slice(&self.right_trigger);
        b[21..32].copy_from_slice(&self.left_trigger);
        // Motor power: low nibble haptics/rumble reduction, high nibble
        // trigger reduction (left at full).
        b[36] = self.rumble_reduction.min(7);
        // valid_flag2: bit0 player LED brightness, bit1 lightbar setup,
        // bit2 improved rumble emulation.
        b[38] = if self.enhanced_rumble { 0x07 } else { 0x03 };
        b[41] = LIGHTBAR_LIGHT_OUT;
        b[42] = self.led_brightness.min(2);
        b[43] = self.player_leds & 0x1F;
        b[44] = self.lightbar[0];
        b[45] = self.lightbar[1];
        b[46] = self.lightbar[2];
    }

    /// Build the report. `out` must be at least the report length; the
    /// returned slice is the meaningful part (the writer pads it to the
    /// controller's output report length).
    pub fn build<'a>(&self, conn: Connection, out: &'a mut [u8]) -> &'a [u8] {
        match conn {
            Connection::Bluetooth => {
                let n = crate::BT_OUTPUT_LEN;
                out[..n].fill(0);
                out[0] = 0x31;
                out[1] = 0x02;
                self.write_body(&mut out[2..n - 4]);
                crc::seal(crc::PREFIX_OUTPUT, &mut out[..n]);
                &out[..n]
            }
            Connection::Usb => {
                let n = crate::USB_OUTPUT_LEN;
                out[..n].fill(0);
                out[0] = 0x02;
                self.write_body(&mut out[1..n]);
                &out[..n]
            }
        }
    }
}

/// Standard player-indicator patterns (centre LED for player 1, and so on),
/// pydualsense's `PlayerID` values.
pub fn player_pattern(player: u8) -> u8 {
    match player {
        0 => 0x00,
        1 => 0x04,
        2 => 0x0A,
        3 => 0x15,
        4 => 0x1B,
        _ => 0x1F,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bt_layout() {
        let s = OutputState {
            rumble_left: 0xAA,
            rumble_right: 0xBB,
            lightbar: [1, 2, 3],
            player_leds: 0x15,
            mute_led: 1,
            ..Default::default()
        };
        let mut buf = [0u8; 100];
        let r = s
            .build(Connection::Bluetooth, &mut buf)
            .to_vec();
        assert_eq!(r.len(), 78);
        assert_eq!(r[0], 0x31);
        assert_eq!(r[1], 0x02);
        assert_eq!(r[2] & 0x0F, 0x0F);
        assert_eq!(r[4], 0xBB);
        assert_eq!(r[5], 0xAA);
        assert_eq!(r[10], 1);
        assert_eq!(&r[12..23], &trigger::OFF);
        assert_eq!(&r[23..34], &trigger::OFF);
        assert_eq!(r[40], 0x07);
        assert_eq!(r[43], 2);
        assert_eq!(r[45], 0x15);
        assert_eq!(&r[46..49], &[1, 2, 3]);
        assert!(crc::verify(crc::PREFIX_OUTPUT, &r));
    }

    #[test]
    fn usb_is_bt_minus_one() {
        let s = OutputState::default();
        let mut a = [0u8; 100];
        let mut b = [0u8; 100];
        let bt = s
            .build(Connection::Bluetooth, &mut a)
            .to_vec();
        let usb = s.build(Connection::Usb, &mut b).to_vec();
        assert_eq!(usb[0], 0x02);
        assert_eq!(&usb[1..48], &bt[2..49]);
    }

    #[test]
    fn rumble_invalid_clears_bits() {
        let s = OutputState {
            rumble_valid: false,
            ..Default::default()
        };
        let mut a = [0u8; 100];
        let r = s.build(Connection::Bluetooth, &mut a);
        assert_eq!(r[2] & 0x03, 0);
    }
}
