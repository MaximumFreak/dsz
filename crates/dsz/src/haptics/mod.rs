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
pub mod ds4;
pub mod dsp;
pub mod game;
pub mod mic;
pub mod opus;
pub mod probe;
pub mod streamer;
pub mod synth;
pub mod usb;

/// One log line with the game feedback settings a controller plays with, so
/// a log shows which profile and levels were in force (issue 29).
pub fn log_game_settings(
    engine: &crate::engine::Engine,
    device: &crate::device::Device,
    p: &crate::profile::Profile,
    what: &str,
) {
    let vo = &p.virtual_out;
    log::info!(
        "{what}; profile \"{}\": game haptics {} at {} %, haptic rumble strength {} %, game rumble {}, on {}",
        engine.profile_name_for(device),
        if vo.game_haptics { "on" } else { "off" },
        vo.game_haptics_level,
        (p.haptics.rumble_intensity * 100.0).round(),
        if vo.game_rumble { "on" } else { "off" },
        if device.is_bluetooth() { "Bluetooth" } else { "USB" },
    );
}
