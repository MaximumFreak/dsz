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
    let h = &p.haptics;
    // What else plays on the actuators or the speaker: each of these keeps
    // the haptic stream running beside a game's rumble (issue 43).
    let mut stream = Vec::new();
    if h.source == crate::profile::AudioSource::SystemAudio {
        stream.push("audio haptics");
    }
    if h.pc_audio_on_speaker {
        stream.push("PC audio on the speaker");
    }
    if vo.game_speaker && h.speaker_volume > 0 {
        stream.push("game speaker");
    }
    if h.buttons.enabled {
        stream.push("button clicks");
    }
    if h.triggers.enabled {
        stream.push("trigger haptics");
    }
    log::info!(
        "{what}; profile \"{}\": game haptics {} at {} %, haptic rumble strength {} %, game rumble {}{}, rumble power {} %, improved rumble {}, haptics {}{}, on {}",
        engine.profile_name_for(device),
        if vo.game_haptics { "on" } else { "off" },
        vo.game_haptics_level,
        (h.rumble_intensity * 100.0).round(),
        if vo.game_rumble { "on" } else { "off" },
        if p.rumble.always_classic { " (classic rumble always)" } else { "" },
        100.0 - 12.5 * p.rumble.power_reduction.min(7) as f32,
        if p.rumble.enhanced { "on" } else { "off" },
        if h.enabled { "on" } else { "off" },
        if stream.is_empty() { String::new() } else { format!(" ({})", stream.join(", ")) },
        if device.is_bluetooth() { "Bluetooth" } else { "USB" },
    );
}
