//! SBC encoder for the DualShock 4's Bluetooth audio: 32 kHz, 16 blocks,
//! joint stereo, SNR allocation, 8 subbands, bitpool 48, which is 109 bytes
//! for 128 frames of stereo PCM.
//!
//! Ported from DS4 Sound Link's `DualShock4SbcEncoder.cs`, `SbcBitStream.cs`
//! and `SbcEncoderTables.cs` (https://github.com/abh1shekbana/DS4-Sound-Link,
//! by abh1shekbana), which follow the Bluetooth A2DP SBC specification.
//! Copyright (c) 2026 ControllerAudioSwitcher Contributors, MIT License; the
//! full notice is in `crates/ds-proto/DS4-Sound-Link-LICENSE`. The arithmetic
//! is kept step for step, so a frame comes out byte for byte the same as
//! theirs (the tests pin frames their encoder produced).

/// Encoded frame length.
pub const FRAME_LEN: usize = 109;
/// Stereo frames of PCM per SBC frame.
pub const SAMPLES: usize = 128;

const CHANNELS: usize = 2;
const SUBBANDS: usize = 8;
const BLOCKS: usize = 16;
const BITPOOL: i32 = 48;
const HISTORY: usize = 80;
/// 32 kHz, 16 blocks, joint stereo, SNR, 8 subbands.
const CONFIG: u8 = 0x7F;

/// Analysis window, A2DP SBC specification, 8 subbands.
const PROTOTYPE8: [f64; HISTORY] = [
    0.00000000e+00, 1.56575398e-04, 3.43256425e-04, 5.54620202e-04,
    8.23919506e-04, 1.13992507e-03, 1.47640169e-03, 1.78371725e-03,
    2.01182542e-03, 2.10371989e-03, 1.99454554e-03, 1.61656283e-03,
    9.02154502e-04, -1.78805361e-04, -1.64973098e-03, -3.49717454e-03,
    5.65949473e-03, 8.02941163e-03, 1.04584443e-02, 1.27472335e-02,
    1.46525263e-02, 1.59045603e-02, 1.62208471e-02, 1.53184106e-02,
    1.29371806e-02, 8.85757540e-03, 2.92408442e-03, -4.91578024e-03,
    -1.46404076e-02, -2.61098752e-02, -3.90751381e-02, -5.31873032e-02,
    6.79989431e-02, 8.29847578e-02, 9.75753918e-02, 1.11196689e-01,
    1.23264548e-01, 1.33264415e-01, 1.40753505e-01, 1.45389847e-01,
    1.46955068e-01, 1.45389847e-01, 1.40753505e-01, 1.33264415e-01,
    1.23264548e-01, 1.11196689e-01, 9.75753918e-02, 8.29847578e-02,
    -6.79989431e-02, -5.31873032e-02, -3.90751381e-02, -2.61098752e-02,
    -1.46404076e-02, -4.91578024e-03, 2.92408442e-03, 8.85757540e-03,
    1.29371806e-02, 1.53184106e-02, 1.62208471e-02, 1.59045603e-02,
    1.46525263e-02, 1.27472335e-02, 1.04584443e-02, 8.02941163e-03,
    -5.65949473e-03, -3.49717454e-03, -1.64973098e-03, -1.78805361e-04,
    9.02154502e-04, 1.61656283e-03, 1.99454554e-03, 2.10371989e-03,
    2.01182542e-03, 1.78371725e-03, 1.47640169e-03, 1.13992507e-03,
    8.23919506e-04, 5.54620202e-04, 3.43256425e-04, 1.56575398e-04,
];

pub struct Encoder {
    matrix: Box<[[f64; HISTORY]; SUBBANDS]>,
    history: [[f64; HISTORY]; CHANNELS],
    samples: [[f64; SAMPLES]; CHANNELS],
    scale: [[i32; SUBBANDS]; CHANNELS],
    bits: [[i32; SUBBANDS]; CHANNELS],
    joined: [bool; SUBBANDS],
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub fn new() -> Encoder {
        let mut matrix = Box::new([[0f64; HISTORY]; SUBBANDS]);
        for (band, row) in matrix.iter_mut().enumerate() {
            for (tap, m) in row.iter_mut().enumerate() {
                let sign = if (tap / 16) & 1 == 0 { 1.0 } else { -1.0 };
                let prototype = PROTOTYPE8[tap] * sign;
                *m = prototype
                    * ((band as f64 + 0.5) * (tap as f64 - SUBBANDS as f64 / 2.0) * std::f64::consts::PI
                        / SUBBANDS as f64)
                        .cos();
            }
        }
        Encoder {
            matrix,
            history: [[0.0; HISTORY]; CHANNELS],
            samples: [[0.0; SAMPLES]; CHANNELS],
            scale: [[0; SUBBANDS]; CHANNELS],
            bits: [[0; SUBBANDS]; CHANNELS],
            joined: [false; SUBBANDS],
        }
    }

    /// Forget the filter history, for a new stream.
    pub fn reset(&mut self) {
        self.history = [[0.0; HISTORY]; CHANNELS];
        self.samples = [[0.0; SAMPLES]; CHANNELS];
    }

    /// Encode 128 stereo frames into one SBC frame.
    pub fn encode(&mut self, left: &[i16; SAMPLES], right: &[i16; SAMPLES], out: &mut [u8; FRAME_LEN]) {
        self.analyze(0, left);
        self.analyze(1, right);
        self.select_joint();
        self.scale_factors();
        self.allocate();
        self.write(out);
    }

    fn analyze(&mut self, ch: usize, pcm: &[i16; SAMPLES]) {
        let history = &mut self.history[ch];
        let out = &mut self.samples[ch];
        for block in 0..BLOCKS {
            let at = block * SUBBANDS;
            history.copy_within(0..HISTORY - SUBBANDS, SUBBANDS);
            for s in 0..SUBBANDS {
                history[SUBBANDS - 1 - s] = pcm[at + s] as f64;
            }
            for band in 0..SUBBANDS {
                let mut v = 0.0;
                for tap in 0..HISTORY {
                    v += self.matrix[band][tap] * history[tap];
                }
                out[at + band] = v;
            }
        }
    }

    fn select_joint(&mut self) {
        for band in 0..SUBBANDS - 1 {
            let (mut ml, mut mr, mut ms, mut md) = (0f64, 0f64, 0f64, 0f64);
            for block in 0..BLOCKS {
                let i = block * SUBBANDS + band;
                let (l, r) = (self.samples[0][i], self.samples[1][i]);
                ml = ml.max(l.abs());
                mr = mr.max(r.abs());
                ms = ms.max(((l + r) * 0.5).abs());
                md = md.max(((l - r) * 0.5).abs());
            }
            let joint = scale_factor(ms) + scale_factor(md) < scale_factor(ml) + scale_factor(mr);
            self.joined[band] = joint;
            if !joint {
                continue;
            }
            for block in 0..BLOCKS {
                let i = block * SUBBANDS + band;
                let (l, r) = (self.samples[0][i], self.samples[1][i]);
                self.samples[0][i] = (l + r) * 0.5;
                self.samples[1][i] = (l - r) * 0.5;
            }
        }
        self.joined[SUBBANDS - 1] = false;
    }

    fn scale_factors(&mut self) {
        for ch in 0..CHANNELS {
            for band in 0..SUBBANDS {
                let mut m = 0f64;
                for block in 0..BLOCKS {
                    m = m.max(self.samples[ch][block * SUBBANDS + band].abs());
                }
                self.scale[ch][band] = scale_factor(m);
            }
        }
    }

    /// SNR bit allocation, as the specification's pseudocode.
    fn allocate(&mut self) {
        let mut max_need = 0;
        for ch in 0..CHANNELS {
            for band in 0..SUBBANDS {
                max_need = max_need.max(self.scale[ch][band]);
                self.bits[ch][band] = 0;
            }
        }
        let mut bitcount = 0;
        let mut slicecount = 0;
        let mut bitslice = max_need + 1;
        loop {
            bitslice -= 1;
            bitcount += slicecount;
            slicecount = 0;
            for ch in 0..CHANNELS {
                for band in 0..SUBBANDS {
                    let need = self.scale[ch][band];
                    if need > bitslice + 1 && need < bitslice + 16 {
                        slicecount += 1;
                    } else if need == bitslice + 1 {
                        slicecount += 2;
                    }
                }
            }
            if bitcount + slicecount >= BITPOOL {
                break;
            }
        }
        if bitcount + slicecount == BITPOOL {
            bitcount += slicecount;
            bitslice -= 1;
        }
        for ch in 0..CHANNELS {
            for band in 0..SUBBANDS {
                let need = self.scale[ch][band];
                self.bits[ch][band] = if need < bitslice + 2 { 0 } else { (need - bitslice).min(16) };
            }
        }
        let (mut ch, mut band) = (0, 0);
        while bitcount < BITPOOL && band < SUBBANDS {
            let b = self.bits[ch][band];
            if (2..16).contains(&b) {
                self.bits[ch][band] += 1;
                bitcount += 1;
            } else if self.scale[ch][band] == bitslice + 1 && bitcount + 1 < BITPOOL {
                self.bits[ch][band] = 2;
                bitcount += 2;
            }
            advance(&mut ch, &mut band);
        }
        let (mut ch, mut band) = (0, 0);
        while bitcount < BITPOOL && band < SUBBANDS {
            if self.bits[ch][band] < 16 {
                self.bits[ch][band] += 1;
                bitcount += 1;
            }
            advance(&mut ch, &mut band);
        }
    }

    fn write(&self, out: &mut [u8; FRAME_LEN]) {
        out.fill(0);
        let mut w = BitWriter { buf: out, bit: 0 };
        w.put(0x9C, 8);
        w.put(CONFIG as u32, 8);
        w.put(BITPOOL as u32, 8);
        w.put(0, 8);
        for band in 0..SUBBANDS - 1 {
            w.put(self.joined[band] as u32, 1);
        }
        w.put(0, 1);
        for ch in 0..CHANNELS {
            for band in 0..SUBBANDS {
                w.put(self.scale[ch][band] as u32, 4);
            }
        }
        for block in 0..BLOCKS {
            for ch in 0..CHANNELS {
                for band in 0..SUBBANDS {
                    let bits = self.bits[ch][band];
                    if bits == 0 {
                        continue;
                    }
                    let levels = (1u32 << bits) - 1;
                    let limit = (1i32 << (self.scale[ch][band] + 1)) as f64;
                    let normalized = self.samples[ch][block * SUBBANDS + band] / limit;
                    let q = ((normalized + 1.0) * levels as f64 * 0.5) as i64;
                    w.put(q.clamp(0, levels as i64) as u32, bits as u32);
                }
            }
        }
        out[3] = header_crc(out);
    }
}

fn scale_factor(max: f64) -> i32 {
    let mut f = 0;
    while f < 15 && (1i32 << (f + 1)) as f64 <= max {
        f += 1;
    }
    f
}

fn advance(ch: &mut usize, band: &mut usize) {
    *ch += 1;
    if *ch == CHANNELS {
        *ch = 0;
        *band += 1;
    }
}

/// MSB-first bit writer. The frame is sized for exactly the bits written.
struct BitWriter<'a> {
    buf: &'a mut [u8; FRAME_LEN],
    bit: usize,
}

impl BitWriter<'_> {
    fn put(&mut self, value: u32, count: u32) {
        for i in (0..count).rev() {
            let at = self.bit / 8;
            if at >= FRAME_LEN {
                return;
            }
            if (value >> i) & 1 != 0 {
                self.buf[at] |= 0x80 >> (self.bit % 8);
            }
            self.bit += 1;
        }
    }
}

/// CRC-8 (poly 0x1D, init 0x0F) over header bytes 1-2 and the joint and
/// scale factor bits, bytes 4..=12.
fn header_crc(frame: &[u8; FRAME_LEN]) -> u8 {
    let mut crc = 0x0Fu8;
    for &b in [frame[1], frame[2]].iter().chain(&frame[4..=12]) {
        for shift in (0..8).rev() {
            let high = ((b >> shift) & 1) ^ (crc >> 7) != 0;
            crc <<= 1;
            if high {
                crc ^= 0x1D;
            }
        }
    }
    crc
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The test signal the vectors were made with: 1 kHz at 16000 on the
    /// left, 440 Hz at 8000 on the right, rounded as .NET's `Math.Round`.
    pub(crate) fn tone(frame: usize) -> ([i16; SAMPLES], [i16; SAMPLES]) {
        let mut l = [0i16; SAMPLES];
        let mut r = [0i16; SAMPLES];
        for i in 0..SAMPLES {
            let n = (frame * SAMPLES + i) as f64;
            let pi = std::f64::consts::PI;
            l[i] = ((2.0 * pi * 1000.0 * n / 32000.0).sin() * 16000.0).round_ties_even() as i16;
            r[i] = ((2.0 * pi * 440.0 * n / 32000.0).sin() * 8000.0).round_ties_even() as i16;
        }
        (l, r)
    }

    // Frames from DS4 Sound Link's own encoder (DualShock4SbcEncoder,
    // commit 8267b6e), run under .NET 8 on the same signal.
    const SILENCE: &str = "9c7f30210000000000000000006db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db6db";
    pub(crate) const TONE: [&str; 4] = [
        "9c7f306c14d9966666c765453480207d557fbb80dd7d5580bb85a4855583c38a2484918b4bb372c206aad6e0c09219f00c39a38555f6c3189e8155c43bc55f7d5571bbe79f7d5525bb3a9f7d55033b181f7d5519bbc51f7d555f3be79f7d55b3bb3a9f7d55f03b181f7d55f8bb",
        "9c7f303830d3000000c0000000c55b9832a6a1e7f0b91e63953aa2528add49180d35810a45c55b98056d21e7f0b51601253aa252ab2c09180d31bb3555c55b983ec161e7f0b5342f953aa252a05989180d358c5a85c55b98015f61e7f0b50472153aa252942b09180d31a95115",
        "9c7f303830d3000000c0000000c55b983a3161e7f0b53f0a953aa252b5a489180d35a24f95c55b980deaa1e7f0b901d2553aa252839309180d35926015c55b98276d21e7f0b53912653aa252bf3449180d35b70495c55b982443a1e7f0b90f8d953aa252826349180d3582d015",
        "9c7f303830d3000000c0000000c55b9810a221e7f0b52581253aa252b7d949180d35bf3e55c55b98384ca1e7f0b92632953aa252914189180d35831345c55b98022b61e7f0b50ef5253aa252a39009180d31b68855c55b983f2961e7f0b5397b953aa252a81c89180d35930385",
    ];

    #[test]
    fn silence_matches_ds4_sound_link() {
        let mut e = Encoder::new();
        let mut out = [0u8; FRAME_LEN];
        e.encode(&[0; SAMPLES], &[0; SAMPLES], &mut out);
        assert_eq!(out.to_vec(), unhex(SILENCE));
        assert_eq!(&out[..3], &[0x9C, 0x7F, 48]);
    }

    #[test]
    fn tone_matches_ds4_sound_link_frame_for_frame() {
        let mut e = Encoder::new();
        let mut out = [0u8; FRAME_LEN];
        for (f, want) in TONE.iter().enumerate() {
            let (l, r) = tone(f);
            e.encode(&l, &r, &mut out);
            assert_eq!(out.to_vec(), unhex(want), "frame {f}");
        }
        // Reset starts the filter over: the first frame again.
        e.reset();
        let (l, r) = tone(0);
        e.encode(&l, &r, &mut out);
        assert_eq!(out.to_vec(), unhex(TONE[0]));
    }

    #[test]
    fn header_crc_is_the_sbc_crc8() {
        let mut e = Encoder::new();
        let mut out = [0u8; FRAME_LEN];
        let (l, r) = tone(1);
        e.encode(&l, &r, &mut out);
        let crc = out[3];
        out[3] = 0;
        assert_eq!(header_crc(&out), crc);
    }
}
