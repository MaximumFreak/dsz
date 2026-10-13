//! The speaker self-test's ears: while a speaker test plays, the controller
//! streams its microphone back (`0x31` reports of type 2, see
//! `ds_proto::input::ParseError::NotState`). Each packet is a counter byte
//! and a 71-byte Opus frame. Decoded, the microphone hears the speaker a
//! centimetre away, so a click or a dropout in the speaker shows up as a
//! short burst of high-frequency energy.

use std::fmt::Write as _;

use super::opus::Decoder;

/// Opus bytes in one microphone packet (78-byte report less id, type,
/// counter and CRC).
pub const MIC_PACKET: usize = 71;

pub type Packet = (u8, [u8; MIC_PACKET]);

/// Analysis block: 2.5 ms at 48 kHz.
const BLOCK: usize = 120;
/// Blocks either side for the local reference level (25 ms).
const SPAN: usize = 10;
/// A block this many times its neighbourhood's median is a click.
const RATIO: f32 = 8.0;

pub struct Report {
    pub summary: String,
    /// Decoded microphone audio, mono 48 kHz.
    pub audio: Vec<f32>,
}

/// Decode the packets (silence for lost ones) and find clicks.
pub fn analyze(packets: &[Packet]) -> Report {
    let Some(mut dec) = Decoder::new() else {
        return Report { summary: "no Opus decoder".into(), audio: Vec::new() };
    };
    let mut audio: Vec<f32> = Vec::with_capacity(packets.len() * 480);
    // Sample ranges that stand in for lost packets.
    let mut lost: Vec<(usize, usize)> = Vec::new();
    let mut buf = [0i16; 960 * 2];
    let mut prev: Option<u8> = None;
    let mut bad = 0;
    for (ctr, data) in packets {
        if let Some(p) = prev {
            let gap = ctr.wrapping_sub(p).saturating_sub(1) as usize;
            if gap > 0 && gap < 50 {
                let at = audio.len();
                audio.resize(at + gap * 480, 0.0);
                lost.push((at, audio.len()));
            }
        }
        prev = Some(*ctr);
        let n = dec.decode(data, &mut buf);
        if n == 0 {
            bad += 1;
            continue;
        }
        audio.extend(buf[..n * 2].chunks_exact(2).map(|f| (f[0] as f32 + f[1] as f32) / 65536.0));
    }
    // High-pass by second difference, energy per block.
    let blocks: Vec<f32> = audio
        .chunks(BLOCK)
        .enumerate()
        .map(|(b, c)| {
            let base = b * BLOCK;
            let mut e = 0f32;
            for i in 0..c.len() {
                let g = base + i;
                if g < 2 {
                    continue;
                }
                let d = audio[g] - 2.0 * audio[g - 1] + audio[g - 2];
                e += d * d;
            }
            e / c.len().max(1) as f32
        })
        .collect();
    let floor = 1e-7;
    let mut clicks: Vec<(f32, f32)> = Vec::new();
    let mut last_click: Option<usize> = None;
    for b in 0..blocks.len() {
        let lo = b.saturating_sub(SPAN);
        let hi = (b + SPAN + 1).min(blocks.len());
        let mut near: Vec<f32> = blocks[lo..hi].to_vec();
        near.sort_by(|a, b| a.total_cmp(b));
        let median = near[near.len() / 2].max(floor);
        let ratio = blocks[b] / median;
        let at = b * BLOCK;
        let in_lost = lost.iter().any(|&(s, e)| at + BLOCK > s && at < e + BLOCK);
        if ratio > RATIO && blocks[b] > floor * 10.0 && !in_lost {
            // One event per burst.
            if last_click.is_none_or(|l| b > l + 2) {
                clicks.push((at as f32 / 48_000.0, ratio));
            }
            last_click = Some(b);
        }
    }
    // Dropouts: a block whose level falls under a quarter of the median
    // around it while the neighbourhood is clearly sounding.
    let level: Vec<f32> = audio
        .chunks(BLOCK)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len().max(1) as f32).sqrt())
        .collect();
    let loud = {
        let mut l = level.clone();
        l.sort_by(|a, b| a.total_cmp(b));
        l[l.len() * 9 / 10] * 0.25
    };
    let mut dips: Vec<f32> = Vec::new();
    let mut last_dip: Option<usize> = None;
    for b in 0..level.len() {
        let lo = b.saturating_sub(SPAN);
        let hi = (b + SPAN + 1).min(level.len());
        let mut near: Vec<f32> = level[lo..hi].to_vec();
        near.sort_by(|a, b| a.total_cmp(b));
        let median = near[near.len() / 2];
        let at = b * BLOCK;
        let in_lost = lost.iter().any(|&(s, e)| at + BLOCK > s && at < e + BLOCK);
        // Both sides must be sounding, so a note's own gap doesn't count.
        let before = level[lo..b].iter().cloned().fold(0f32, f32::max);
        let after = level[(b + 1).min(hi)..hi].iter().cloned().fold(0f32, f32::max);
        if median > loud && before > loud && after > loud && level[b] < median * 0.25 && !in_lost {
            if last_dip.is_none_or(|l| b > l + 2) {
                dips.push(at as f32 / 48_000.0);
            }
            last_dip = Some(b);
        }
    }
    let rms = (audio.iter().map(|v| v * v).sum::<f32>() / audio.len().max(1) as f32).sqrt();
    let mut summary = format!(
        "{} dropout(s), {} click(s) in {:.1} s of microphone audio ({} packets, {} lost, {bad} undecodable, level {:.3} rms)",
        dips.len(),
        clicks.len(),
        audio.len() as f32 / 48_000.0,
        packets.len(),
        lost.iter().map(|(s, e)| (e - s) / 480).sum::<usize>(),
        rms
    );
    if !dips.is_empty() {
        summary.push_str("; dropouts at");
        for t in dips.iter().take(12) {
            let _ = write!(summary, " {t:.2}s");
        }
    }
    if !clicks.is_empty() {
        summary.push_str(";");
        summary.push_str(" clicks at");
        for (t, r) in clicks.iter().take(12) {
            let _ = write!(summary, " {t:.2}s (×{r:.0})");
        }
    }
    Report { summary, audio }
}

/// Mono 16-bit 48 kHz WAV.
pub fn wav(audio: &[f32]) -> Vec<u8> {
    let data = (audio.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&48_000u32.to_le_bytes());
    w.extend_from_slice(&96_000u32.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for v in audio {
        w.extend_from_slice(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    w
}
