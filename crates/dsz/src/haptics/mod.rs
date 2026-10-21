//! Bluetooth wireless haptics and controller-speaker audio.
//!
//! ```text
//! WASAPI loopback ──► AudioHub ──► per-controller FIFO
//!                                      │  every 32/3000 s (high-res timer)
//!                                      ▼
//!        high-pass + low-pass ─► 3 kHz ─► + event synth ─► int8 x 64 ─► 0x32 report
//!
//! While there is speaker sound (a game's speaker channels, or PC audio sent
//! to the speaker), every 64/3000 s instead:
//!
//!        2 haptic blocks + 1024 speaker frames ─► 16:15 squeeze ─► 2 Opus frames ─► 0x39 report
//! ```

pub mod audio;
pub mod capture;
pub mod dsp;
pub mod game;
pub mod mic;
pub mod opus;
pub mod probe;
pub mod streamer;
pub mod synth;
