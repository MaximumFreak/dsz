//! Bluetooth wireless haptics.
//!
//! ```text
//! WASAPI loopback ──► AudioHub ──► per-controller FIFO
//!                                      │  every 32/3000 s (high-res timer)
//!                                      ▼
//!        high-pass + low-pass ─► 3 kHz ─► + event synth ─► int8 x 64 ─► 0x32 report
//! ```

pub mod audio;
pub mod dsp;
pub mod game;
pub mod streamer;
pub mod synth;
