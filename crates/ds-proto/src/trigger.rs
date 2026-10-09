//! Adaptive-trigger effects, 11 bytes each.
//!
//! Out-of-range parameters are clamped into range instead of making the
//! encoder fail, so a slider can never produce a dead trigger.

use serde::{Deserialize, Serialize};

pub type EffectBytes = [u8; 11];

pub const OFF: EffectBytes = [0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TriggerPreset {
    GameCube,
    VerySoft,
    Soft,
    Medium,
    Hard,
    VeryHard,
    Hardest,
    Rigid,
    Choppy,
    VibratePulse,
}

impl TriggerPreset {
    pub const ALL: [TriggerPreset; 10] = [
        TriggerPreset::GameCube,
        TriggerPreset::VerySoft,
        TriggerPreset::Soft,
        TriggerPreset::Medium,
        TriggerPreset::Hard,
        TriggerPreset::VeryHard,
        TriggerPreset::Hardest,
        TriggerPreset::Rigid,
        TriggerPreset::Choppy,
        TriggerPreset::VibratePulse,
    ];

    pub fn name(self) -> &'static str {
        match self {
            TriggerPreset::GameCube => "GameCube click",
            TriggerPreset::VerySoft => "Very soft",
            TriggerPreset::Soft => "Soft",
            TriggerPreset::Medium => "Medium",
            TriggerPreset::Hard => "Hard",
            TriggerPreset::VeryHard => "Very hard",
            TriggerPreset::Hardest => "Hardest",
            TriggerPreset::Rigid => "Rigid",
            TriggerPreset::Choppy => "Choppy",
            TriggerPreset::VibratePulse => "Vibrate pulse",
        }
    }

    /// The preset as one of the effects below. The firmness presets are
    /// resistance over the whole pull on the 1..=8 scale, Very soft to Rigid.
    pub fn effect(self) -> TriggerEffect {
        let firm = |strength| TriggerEffect::Feedback {
            position: 0,
            strength,
        };
        match self {
            // A light pull that ends in a click, like a GameCube shoulder.
            TriggerPreset::GameCube => TriggerEffect::Weapon {
                start: 6,
                end: 8,
                strength: 6,
            },
            TriggerPreset::VerySoft => firm(2),
            TriggerPreset::Soft => firm(3),
            TriggerPreset::Medium => firm(4),
            TriggerPreset::Hard => firm(5),
            TriggerPreset::VeryHard => firm(6),
            TriggerPreset::Hardest => firm(7),
            TriggerPreset::Rigid => firm(8),
            // Resistance in every other zone: the pull catches and frees.
            TriggerPreset::Choppy => TriggerEffect::MultiFeedback {
                strengths: [6, 0, 6, 0, 6, 0, 6, 0, 6, 0],
            },
            // Vibration that swells and fades.
            TriggerPreset::VibratePulse => TriggerEffect::Machine {
                start: 0,
                end: 9,
                amp_a: 7,
                amp_b: 1,
                frequency: 30,
                period: 3,
            },
        }
    }

    pub fn encode(self) -> EffectBytes {
        self.effect().encode()
    }
}

/// One trigger's effect. Positions are zones 0..=9 along the pull;
/// strengths and amplitudes are 1..=8 unless noted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum TriggerEffect {
    #[default]
    Off,
    /// Constant resistance from `position` to the end.
    Feedback {
        position: u8,
        strength: u8,
    },
    /// Resistance between `start` and `end` that snaps free, like a trigger break.
    Weapon {
        start: u8,
        end: u8,
        strength: u8,
    },
    /// Vibration from `position` to the end. `frequency` in Hz (1..=255).
    Vibration {
        position: u8,
        amplitude: u8,
        frequency: u8,
    },
    /// Resistance that ramps from `start_strength` to `end_strength`.
    Slope {
        start: u8,
        end: u8,
        start_strength: u8,
        end_strength: u8,
    },
    /// Per-zone resistance, 0 = none.
    MultiFeedback {
        strengths: [u8; 10],
    },
    /// Per-zone vibration amplitude, 0 = none.
    MultiVibration {
        amplitudes: [u8; 10],
        frequency: u8,
    },
    /// Bow string: resistance then a snap back.
    Bow {
        start: u8,
        end: u8,
        strength: u8,
        snap: u8,
    },
    /// Galloping horse rhythm. Feet are 0..=7, `first_foot < second_foot`.
    Galloping {
        start: u8,
        end: u8,
        first_foot: u8,
        second_foot: u8,
        frequency: u8,
    },
    /// Two-amplitude machine vibration. Amplitudes 0..=7.
    Machine {
        start: u8,
        end: u8,
        amp_a: u8,
        amp_b: u8,
        frequency: u8,
        period: u8,
    },
    /// Custom trigger values: a raw mode byte plus seven forces.
    Custom {
        mode: u8,
        forces: [u8; 7],
    },
    Preset {
        preset: TriggerPreset,
    },
    Raw {
        bytes: EffectBytes,
    },
}

fn zones(values: &[u8; 10]) -> (u16, u32) {
    let mut mask = 0u16;
    let mut packed = 0u32;
    for (i, &v) in values.iter().enumerate() {
        if v > 0 {
            let v = (v.min(8) - 1) as u32 & 7;
            packed |= v << (3 * i);
            mask |= 1 << i;
        }
    }
    (mask, packed)
}

fn zone_effect(mode: u8, values: &[u8; 10], freq: u8) -> EffectBytes {
    let (mask, packed) = zones(values);
    if mask == 0 {
        return OFF;
    }
    let p = packed.to_le_bytes();
    let m = mask.to_le_bytes();
    [mode, m[0], m[1], p[0], p[1], p[2], p[3], 0, 0, freq, 0]
}

fn span(start: u8, end: u8, max_start: u8, max_end: u8, min_start: u8) -> (u8, u8) {
    let s = start.clamp(min_start, max_start);
    let e = end.clamp(s + 1, max_end);
    (s, e)
}

impl TriggerEffect {
    pub fn encode(&self) -> EffectBytes {
        match *self {
            TriggerEffect::Off => OFF,
            TriggerEffect::Feedback { position, strength } => {
                let pos = position.min(9) as usize;
                let mut v = [0u8; 10];
                for z in v.iter_mut().skip(pos) {
                    *z = strength.min(8);
                }
                zone_effect(0x21, &v, 0)
            }
            TriggerEffect::Weapon {
                start,
                end,
                strength,
            } => {
                if strength == 0 {
                    return OFF;
                }
                let (s, e) = span(start, end, 7, 8, 2);
                let mask = ((1u16 << s) | (1u16 << e)).to_le_bytes();
                [
                    0x25,
                    mask[0],
                    mask[1],
                    strength.min(8) - 1,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ]
            }
            TriggerEffect::Vibration {
                position,
                amplitude,
                frequency,
            } => {
                if frequency == 0 {
                    return OFF;
                }
                let pos = position.min(9) as usize;
                let mut v = [0u8; 10];
                for z in v.iter_mut().skip(pos) {
                    *z = amplitude.min(8);
                }
                zone_effect(0x26, &v, frequency)
            }
            TriggerEffect::Slope {
                start,
                end,
                start_strength,
                end_strength,
            } => {
                let (s, e) = span(start, end, 8, 9, 0);
                let ss = start_strength.clamp(1, 8) as f32;
                let es = end_strength.clamp(1, 8) as f32;
                let slope = (es - ss) / (e - s) as f32;
                let mut v = [0u8; 10];
                for (i, z) in v.iter_mut().enumerate().skip(s as usize) {
                    *z = if i as u8 <= e {
                        (ss + slope * (i as u8 - s) as f32).round() as u8
                    } else {
                        es as u8
                    };
                }
                zone_effect(0x21, &v, 0)
            }
            TriggerEffect::MultiFeedback { strengths } => zone_effect(0x21, &strengths, 0),
            TriggerEffect::MultiVibration {
                amplitudes,
                frequency,
            } => {
                if frequency == 0 {
                    return OFF;
                }
                zone_effect(0x26, &amplitudes, frequency)
            }
            TriggerEffect::Bow {
                start,
                end,
                strength,
                snap,
            } => {
                if strength == 0 || snap == 0 {
                    return OFF;
                }
                let (s, e) = span(start, end, 7, 8, 0);
                let mask = ((1u16 << s) | (1u16 << e)).to_le_bytes();
                let packed =
                    ((strength.min(8) - 1) & 7) as u16 | ((((snap.min(8) - 1) & 7) as u16) << 3);
                let p = packed.to_le_bytes();
                [0x22, mask[0], mask[1], p[0], p[1], 0, 0, 0, 0, 0, 0]
            }
            TriggerEffect::Galloping {
                start,
                end,
                first_foot,
                second_foot,
                frequency,
            } => {
                if frequency == 0 {
                    return OFF;
                }
                let (s, e) = span(start, end, 8, 9, 0);
                let second = second_foot.clamp(1, 7);
                let first = first_foot.min(second - 1);
                let mask = ((1u16 << s) | (1u16 << e)).to_le_bytes();
                let packed = (second & 7) | ((first & 7) << 3);
                [0x23, mask[0], mask[1], packed, frequency, 0, 0, 0, 0, 0, 0]
            }
            TriggerEffect::Machine {
                start,
                end,
                amp_a,
                amp_b,
                frequency,
                period,
            } => {
                if frequency == 0 {
                    return OFF;
                }
                let (s, e) = span(start, end, 8, 9, 0);
                let mask = ((1u16 << s) | (1u16 << e)).to_le_bytes();
                let packed = (amp_a.min(7) & 7) | ((amp_b.min(7) & 7) << 3);
                [
                    0x27, mask[0], mask[1], packed, frequency, period, 0, 0, 0, 0, 0,
                ]
            }
            TriggerEffect::Custom { mode, forces } => {
                let mut b = [0u8; 11];
                b[0] = mode;
                b[1..8].copy_from_slice(&forces);
                b
            }
            TriggerEffect::Preset { preset } => preset.encode(),
            TriggerEffect::Raw { bytes } => bytes,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            TriggerEffect::Off => "Off",
            TriggerEffect::Feedback { .. } => "Resistance",
            TriggerEffect::Weapon { .. } => "Weapon",
            TriggerEffect::Vibration { .. } => "Vibration",
            TriggerEffect::Slope { .. } => "Slope",
            TriggerEffect::MultiFeedback { .. } => "Zones: resistance",
            TriggerEffect::MultiVibration { .. } => "Zones: vibration",
            TriggerEffect::Bow { .. } => "Bow",
            TriggerEffect::Galloping { .. } => "Galloping",
            TriggerEffect::Machine { .. } => "Machine",
            TriggerEffect::Custom { .. } => "Custom bytes",
            TriggerEffect::Preset { .. } => "Preset",
            TriggerEffect::Raw { .. } => "Raw",
        }
    }

    /// Per-zone intensity 0..=1 for a preview strip. Approximate for effects
    /// that are not zone-based.
    pub fn zone_preview(&self) -> [f32; 10] {
        let mut out = [0f32; 10];
        let b = self.encode();
        match b[0] {
            0x21 | 0x26 => {
                let mask = u16::from_le_bytes([b[1], b[2]]);
                let packed = u32::from_le_bytes([b[3], b[4], b[5], b[6]]);
                for (i, o) in out.iter_mut().enumerate() {
                    if mask & (1 << i) != 0 {
                        *o = (((packed >> (3 * i)) & 7) + 1) as f32 / 8.0;
                    }
                }
            }
            0x25 | 0x22 | 0x23 | 0x27 => {
                let mask = u16::from_le_bytes([b[1], b[2]]);
                let s = mask.trailing_zeros() as usize;
                let e = 15 - mask.leading_zeros() as usize;
                let level = match b[0] {
                    0x25 => (b[3] + 1) as f32 / 8.0,
                    0x22 => ((b[3] & 7) + 1) as f32 / 8.0,
                    0x23 => 0.5,
                    _ => ((b[3] & 7).max((b[3] >> 3) & 7) + 1) as f32 / 8.0,
                };
                for (i, o) in out.iter_mut().enumerate() {
                    if i >= s && i <= e.min(9) {
                        *o = level;
                    }
                }
            }
            0x05 | 0x00 => {}
            _ => {
                // Legacy/simple modes: show a flat bar so the user sees "something".
                for o in out.iter_mut() {
                    *o = 0.6;
                }
            }
        }
        out
    }

    pub fn is_vibration(&self) -> bool {
        matches!(self.encode()[0], 0x26 | 0x23 | 0x27 | 0x06)
    }

    /// Effect from a UDP mod-API trigger update: the mod's trigger mode
    /// plus its parameters. Modes 0..=18 and their parameters are as game
    /// mods define them; 19..=26 follow Nielk1's trigger effect generator
    /// (MIT), which the mods that send them use. `None` for an unknown mode
    /// or a custom mode without a public definition (see [`custom_mode_byte`]).
    pub fn from_mod_legacy(kind: i64, p: &[u8]) -> Option<TriggerEffect> {
        let g = |i: usize, d: u8| p.get(i).copied().unwrap_or(d);
        let zones10 = || {
            let mut z = [0u8; 10];
            for (i, v) in z.iter_mut().enumerate() {
                *v = g(i, 0);
            }
            z
        };
        Some(match kind {
            0 | 20 => TriggerEffect::Off,
            1 => TriggerEffect::Preset {
                preset: TriggerPreset::GameCube,
            },
            2 => TriggerEffect::Preset {
                preset: TriggerPreset::VerySoft,
            },
            3 => TriggerEffect::Preset {
                preset: TriggerPreset::Soft,
            },
            4 => TriggerEffect::Preset {
                preset: TriggerPreset::Hard,
            },
            5 => TriggerEffect::Preset {
                preset: TriggerPreset::VeryHard,
            },
            6 => TriggerEffect::Preset {
                preset: TriggerPreset::Hardest,
            },
            7 => TriggerEffect::Preset {
                preset: TriggerPreset::Rigid,
            },
            // VibrateTrigger: param0 is the frequency in old mods.
            8 => TriggerEffect::Vibration {
                position: 0,
                amplitude: 8,
                frequency: g(0, 10).max(1),
            },
            9 => TriggerEffect::Preset {
                preset: TriggerPreset::Choppy,
            },
            10 => TriggerEffect::Preset {
                preset: TriggerPreset::Medium,
            },
            11 => TriggerEffect::Preset {
                preset: TriggerPreset::VibratePulse,
            },
            12 => {
                let mut forces = [0u8; 7];
                for (i, f) in forces.iter_mut().enumerate() {
                    *f = g(i + 1, 0);
                }
                TriggerEffect::Custom {
                    mode: custom_mode_byte(g(0, 0))?,
                    forces,
                }
            }
            // Resistance and FEEDBACK: [position, strength]
            13 | 21 => TriggerEffect::Feedback {
                position: g(0, 0),
                strength: g(1, 8),
            },
            14 => TriggerEffect::Bow {
                start: g(0, 0),
                end: g(1, 8),
                strength: g(2, 8),
                snap: g(3, 8),
            },
            15 => TriggerEffect::Galloping {
                start: g(0, 0),
                end: g(1, 9),
                first_foot: g(2, 2),
                second_foot: g(3, 5),
                frequency: g(4, 10),
            },
            16 | 22 => TriggerEffect::Weapon {
                start: g(0, 2),
                end: g(1, 6),
                strength: g(2, 8),
            },
            17 | 23 => TriggerEffect::Vibration {
                position: g(0, 0),
                amplitude: g(1, 8),
                frequency: g(2, 10),
            },
            18 => TriggerEffect::Machine {
                start: g(0, 0),
                end: g(1, 9),
                amp_a: g(2, 7),
                amp_b: g(3, 7),
                frequency: g(4, 5),
                period: g(5, 3),
            },
            19 => TriggerEffect::Vibration {
                position: 0,
                amplitude: 8,
                frequency: 10,
            },
            24 => TriggerEffect::Slope {
                start: g(0, 0),
                end: g(1, 9),
                start_strength: g(2, 1),
                end_strength: g(3, 8),
            },
            25 => TriggerEffect::MultiFeedback {
                strengths: zones10(),
            },
            // [frequency, then ten zone amplitudes]
            26 => TriggerEffect::MultiVibration {
                amplitudes: {
                    let mut z = [0u8; 10];
                    for (i, v) in z.iter_mut().enumerate() {
                        *v = g(i + 1, 0);
                    }
                    z
                },
                frequency: g(0, 10),
            },
            _ => return None,
        })
    }
}

/// Custom trigger mode index from a mod to the mode byte the firmware takes,
/// for the modes pydualsense (MIT) documents in `TriggerModes`: Off, then
/// Rigid and Pulse, each plain, A (`0x20`), B (`0x04`), and A+B. Mods number
/// them in that order. `None` for the other modes (9..=16).
pub fn custom_mode_byte(mode: u8) -> Option<u8> {
    const A: u8 = 0x20;
    const B: u8 = 0x04;
    const RIGID: u8 = 0x01;
    const PULSE: u8 = 0x02;
    Some(match mode {
        0 => 0x00,
        1 => RIGID,
        2 => RIGID | A,
        3 => RIGID | B,
        4 => RIGID | A | B,
        5 => PULSE,
        6 => PULSE | A,
        7 => PULSE | B,
        8 => PULSE | A | B,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_matches_known_bytes() {
        // position 3, strength 5: zones 3..9, value 4 each.
        let b = TriggerEffect::Feedback {
            position: 3,
            strength: 5,
        }
        .encode();
        let mut mask = 0u16;
        let mut packed = 0u32;
        for i in 3..10 {
            packed |= 4 << (3 * i);
            mask |= 1 << i;
        }
        assert_eq!(b[0], 0x21);
        assert_eq!(u16::from_le_bytes([b[1], b[2]]), mask);
        assert_eq!(u32::from_le_bytes([b[3], b[4], b[5], b[6]]), packed);
    }

    #[test]
    fn weapon_bytes() {
        let b = TriggerEffect::Weapon {
            start: 2,
            end: 6,
            strength: 8,
        }
        .encode();
        assert_eq!(b, [0x25, 0x44, 0x00, 7, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn zero_strength_is_off() {
        assert_eq!(
            TriggerEffect::Feedback {
                position: 0,
                strength: 0
            }
            .encode(),
            OFF
        );
        assert_eq!(
            TriggerEffect::Vibration {
                position: 0,
                amplitude: 8,
                frequency: 0
            }
            .encode(),
            OFF
        );
    }

    #[test]
    fn presets() {
        // Firmness rises from Very soft to Rigid, over the whole pull.
        let firm = [
            TriggerPreset::VerySoft,
            TriggerPreset::Soft,
            TriggerPreset::Medium,
            TriggerPreset::Hard,
            TriggerPreset::VeryHard,
            TriggerPreset::Hardest,
            TriggerPreset::Rigid,
        ];
        let mut last = 0.0;
        for p in firm {
            let z = TriggerEffect::Preset { preset: p }.zone_preview();
            assert!(z.iter().all(|&v| v == z[0]), "{p:?} covers the pull");
            assert!(z[0] > last, "{p:?} is firmer");
            last = z[0];
        }
        for p in TriggerPreset::ALL {
            assert_ne!(p.encode(), OFF, "{p:?}");
        }
    }

    #[test]
    fn custom_modes() {
        let all: Vec<u8> = (0..=8).map(|m| custom_mode_byte(m).unwrap()).collect();
        assert_eq!(all, [0, 0x01, 0x21, 0x05, 0x25, 0x02, 0x22, 0x06, 0x26]);
        for m in 9..=17 {
            assert_eq!(custom_mode_byte(m), None);
        }
    }

    #[test]
    fn clamps_instead_of_failing() {
        // Start 0 is invalid for weapon; we clamp to 2.
        let b = TriggerEffect::Weapon {
            start: 0,
            end: 0,
            strength: 3,
        }
        .encode();
        assert_eq!(b[0], 0x25);
        assert_eq!(u16::from_le_bytes([b[1], b[2]]), (1 << 2) | (1 << 3));
    }

    #[test]
    fn slope() {
        let b = TriggerEffect::Slope {
            start: 0,
            end: 9,
            start_strength: 1,
            end_strength: 8,
        }
        .encode();
        assert_eq!(b[0], 0x21);
        assert_eq!(u16::from_le_bytes([b[1], b[2]]), 0x3FF);
    }
}
