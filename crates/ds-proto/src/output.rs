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
    /// Where the controller plays the audio stream.
    pub audio: AudioRoute,
    /// Explicit microphone input path and mute, independent of speaker output.
    pub microphone: Option<Microphone>,
}

/// Audio-control bits 1:0: auto prefers an attached headset microphone.
/// Source: stealth-alex's DualSense HID notes; DS5Dongle's `MicSelect`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MicrophoneSource {
    #[default]
    Auto,
    BuiltIn,
    Headset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Microphone {
    pub source: MicrophoneSource,
    pub muted: bool,
}

/// Where the controller plays its audio stream. The fields are Linux
/// `hid-playstation`'s: headphone volume (flag0 bit 4), speaker volume
/// (flag0 bit 5), audio control (flag0 bit 7) with the output path in bits
/// 5:4, and audio control 2 (flag1 bit 7) with the speaker preamp gain in
/// bits 2:0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AudioRoute {
    /// Audio fields not marked valid: the controller keeps what it has.
    #[default]
    Untouched,
    /// Path 3: the right channel to the speaker, the headset muted.
    /// Volume is clamped to 0x3D..=0x64, the range Linux found accepted.
    Speaker { volume: u8 },
    /// The headset jack. Tested on an Edge over Bluetooth with a speaker
    /// stream running: the path is ignored, with or without input path
    /// `0x40` (the stream's Opus packet type picks the sink, see
    /// `stream::build_audio`), so the speaker is silenced by volume 0 and
    /// the headphones get their own volume, which is 0 after power-on. Path
    /// 0 is still sent, as Linux does over USB.
    Headset { volume: u8 },
    /// Path 0 with nothing streaming: the speaker is silent.
    Muted,
}

/// Lowest and highest speaker volume (Linux `hid-playstation`).
pub const SPEAKER_VOLUME_MIN: u8 = 0x3D;
pub const SPEAKER_VOLUME_MAX: u8 = 0x64;
/// Highest headphone volume, the value in the PS5's speaker-stream config.
pub const HEADSET_VOLUME_MAX: u8 = 0x7F;

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
            audio: AudioRoute::Untouched,
            microphone: None,
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
        let mut flag1 = 0x01 | 0x04 | 0x10 | 0x40;
        match self.audio {
            AudioRoute::Untouched => {}
            AudioRoute::Speaker { volume } => {
                flag0 |= 0x20 | 0x80;
                flag1 |= 0x80;
                b[5] = volume.clamp(SPEAKER_VOLUME_MIN, SPEAKER_VOLUME_MAX);
                b[7] = 3 << 4;
                // Speaker preamp +6 dB, as Linux sets it.
                b[37] = 0x02;
            }
            AudioRoute::Headset { volume } => {
                flag0 |= 0x10 | 0x20 | 0x80;
                b[4] = volume.min(HEADSET_VOLUME_MAX);
                b[5] = 0;
                b[7] = 0;
            }
            AudioRoute::Muted => {
                flag0 |= 0x80;
                b[7] = 0;
            }
        }
        if let Some(mic) = self.microphone {
            // flag0 bit6 microphone volume, bit7 audio control;
            // flag1 bit1 audio/microphone mute. Keep output path bits intact.
            flag0 |= 0x40 | 0x80;
            flag1 |= 0x02;
            b[6] = 0x40;
            b[7] |= match mic.source {
                MicrophoneSource::Auto => 0,
                MicrophoneSource::BuiltIn => 1,
                MicrophoneSource::Headset => 2,
            };
        }
        b[0] = flag0;
        b[1] = flag1;
        b[2] = self.rumble_right;
        b[3] = self.rumble_left;
        b[8] = self.mute_led.min(2);
        // Power-save byte stays 0 and its enable flag unset. Tested on an
        // Edge over Bluetooth: the motion bit does not stop the sensors, so
        // there is no battery to gain here.
        b[9] = if self.microphone.is_some_and(|m| m.muted) {
            0x10
        } else {
            0
        };
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
    fn microphone_source_and_mute_preserve_speaker_output_and_triggers() {
        for (source, bits) in [
            (MicrophoneSource::Auto, 0),
            (MicrophoneSource::BuiltIn, 1),
            (MicrophoneSource::Headset, 2),
        ] {
            for muted in [false, true] {
                let s = OutputState {
                    audio: AudioRoute::Speaker { volume: 0x50 },
                    microphone: Some(Microphone { source, muted }),
                    rumble_left: 87,
                    right_trigger: [1; 11],
                    ..Default::default()
                };
                let mut buf = [0; 100];
                let r = s.build(Connection::Bluetooth, &mut buf);
                assert_eq!(r[2] & 0xc0, 0xc0);
                assert_eq!(r[3] & 2, 2);
                assert_eq!(r[8], 0x40, "microphone gain");
                assert_eq!(r[9], 0x30 | bits, "output and input path coexist");
                assert_eq!(r[11], if muted { 0x10 } else { 0 });
                assert_eq!(r[5], 87);
                assert_eq!(&r[12..23], &[1; 11]);
                assert!(crc::verify(crc::PREFIX_OUTPUT, r));
            }
        }
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
    fn audio_routes() {
        let mut a = [0u8; 100];
        let r = OutputState::default().build(Connection::Bluetooth, &mut a).to_vec();
        assert_eq!(r[2] & 0xE0, 0, "untouched: no audio flags");
        let s = OutputState {
            audio: AudioRoute::Speaker { volume: 0x10 },
            ..Default::default()
        };
        let r = s.build(Connection::Bluetooth, &mut a).to_vec();
        assert_eq!(r[2] & 0xA0, 0xA0);
        assert_eq!(r[3] & 0x80, 0x80);
        assert_eq!(r[2 + 5], SPEAKER_VOLUME_MIN);
        assert_eq!(r[2 + 7], 0x30);
        assert_eq!(r[2 + 37], 0x02);
        assert!(crc::verify(crc::PREFIX_OUTPUT, &r));
        let s = OutputState {
            audio: AudioRoute::Headset { volume: 0xFF },
            ..Default::default()
        };
        let r = s.build(Connection::Bluetooth, &mut a).to_vec();
        assert_eq!(r[2] & 0xB0, 0xB0);
        assert_eq!(r[2 + 4], HEADSET_VOLUME_MAX);
        assert_eq!(r[2 + 5], 0, "speaker silenced by volume");
        assert_eq!(r[2 + 7], 0x00);
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
