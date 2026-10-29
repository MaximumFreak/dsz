//! App-wide settings, `settings.json` in the data directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::profile::VirtualKind;

/// Virtual controller to present while a game without native DualSense
/// support is running. [`FallbackPad::Off`] keeps the profile's own choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FallbackPad {
    Off,
    Xbox360,
    DualShock4,
}

impl Default for FallbackPad {
    fn default() -> Self {
        FallbackPad::Off
    }
}

impl FallbackPad {
    pub fn label(self) -> &'static str {
        match self {
            FallbackPad::Off => "Same as the profile",
            FallbackPad::Xbox360 => "Xbox 360",
            FallbackPad::DualShock4 => "DualShock 4",
        }
    }
}

/// Which virtual controller games should see.
///
/// A running game with a profile rule, or native DualSense support, already
/// picked `profile_kind` (`profile_forced`). Otherwise a game with no
/// DualSense support can ask for `fallback` instead of the profile's pad.
pub fn effective_virtual_kind(
    profile_kind: VirtualKind,
    profile_forced: bool,
    fallback_active: bool,
    fallback: FallbackPad,
) -> VirtualKind {
    if profile_forced || !fallback_active {
        return profile_kind;
    }
    match fallback {
        FallbackPad::Off => profile_kind,
        FallbackPad::Xbox360 => VirtualKind::Xbox360,
        FallbackPad::DualShock4 => VirtualKind::DualShock4,
    }
}

/// UTF-8 byte-order mark that Notepad and PowerShell put at the start of
/// JSON files; skipped when reading settings and profiles.
pub const BOM: char = '\u{feff}';

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameRule {
    pub enabled: bool,
    /// Executable file name, e.g. `eldenring.exe`.
    pub exe: String,
    pub profile: String,
}

impl Default for GameRule {
    fn default() -> Self {
        GameRule {
            enabled: true,
            exe: String::new(),
            profile: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub language: crate::i18n::Language,
    pub start_with_windows: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    /// The minimize button hides the window to the tray.
    pub minimize_to_tray: bool,
    /// Profile used by controllers with no assignment.
    pub default_profile: String,
    /// Controller MAC -> profile name.
    pub device_profiles: BTreeMap<String, String>,
    /// Controller MAC -> friendly name.
    pub device_names: BTreeMap<String, String>,
    /// UDP mod API for game mods.
    pub udp_enabled: bool,
    pub udp_port: u16,
    pub auto_profiles: bool,
    pub games: Vec<GameRule>,
    /// Power off a Bluetooth controller after this many idle minutes (0 = never).
    pub idle_off_minutes: u32,
    /// Quick Charge: drop a controller's Bluetooth link when the same
    /// controller (by MAC) is plugged in over USB, so only USB is left.
    pub quick_charge: bool,
    pub ui_scale: f32,
    /// Drop Bluetooth input reports whose CRC does not match.
    pub check_input_crc: bool,
    /// Show the welcome card on the overview page.
    pub show_tips: bool,
    /// Let the app use HidHide to hide a controller while a virtual one is up.
    pub hidhide_control: bool,
    /// Start every controller on `default_profile` when the app starts,
    /// ignoring saved per-controller assignments until one is picked.
    pub default_on_start: bool,
    /// Switch to `dualsense_profile` while a game with native DualSense
    /// support runs (found by scanning its exe), unless a game rule applies.
    pub auto_dualsense: bool,
    pub dualsense_profile: String,
    /// Virtual controller for a Steam, GOG, or Xbox app game that has no
    /// native DualSense support and no rule of its own.
    pub fallback_pad: FallbackPad,
    /// Keep a virtual controller plugged in while its physical controller is
    /// off, and plug one in when the app starts, so games keep the same pad
    /// (and XInput slot) throughout.
    pub persistent_virtual: bool,
    /// Keep the virtual Xbox pad plugged in (idle, hidden from games) while
    /// a DualSense or native profile is active, so it stays XInput player 1
    /// and presses Xbox buttons. Off: it is unplugged and a helper pad
    /// plugged in for Xbox buttons.
    pub keep_xbox_pad: bool,
    /// MAC of the controller connected most recently; picks the profile for
    /// the virtual pad made at startup.
    pub last_controller: String,
    /// Controller MAC -> the device nodes HidHide hid for it, so the next
    /// start can hide it before it connects.
    pub hide_nodes: BTreeMap<String, Vec<String>>,
    /// Helper Xbox pad USB serial -> the nodes Windows made below it, so
    /// the next plug-in hides them before they appear.
    pub helper_nodes: BTreeMap<String, Vec<String>>,
    /// Default audio slots (output, then microphone; each for console,
    /// multimedia and communications) the user set to a virtual DualSense.
    /// DSZ keeps them there when one is plugged in, and keeps Windows from
    /// moving the others to it.
    pub audio_on_virtual: [bool; 6],
    /// Ask GitHub once a day whether a newer release is out.
    pub check_updates: bool,
    /// Motion server for emulators (cemuhook/DSU, UDP 26760).
    pub dsu_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            language: crate::i18n::Language::English,
            start_with_windows: false,
            start_minimized: false,
            close_to_tray: true,
            minimize_to_tray: false,
            default_profile: "Xbox".into(),
            device_profiles: BTreeMap::new(),
            device_names: BTreeMap::new(),
            udp_enabled: true,
            udp_port: 6969,
            auto_profiles: true,
            games: Vec::new(),
            idle_off_minutes: 15,
            quick_charge: false,
            ui_scale: 1.0,
            check_input_crc: true,
            show_tips: true,
            hidhide_control: true,
            default_on_start: false,
            auto_dualsense: true,
            dualsense_profile: "DualSense".into(),
            fallback_pad: FallbackPad::Off,
            persistent_virtual: true,
            keep_xbox_pad: true,
            last_controller: String::new(),
            hide_nodes: BTreeMap::new(),
            helper_nodes: BTreeMap::new(),
            audio_on_virtual: [false; 6],
            check_updates: true,
            dsu_enabled: false,
        }
    }
}

impl Settings {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("settings.json")
    }

    pub fn load(dir: &Path) -> Settings {
        let p = Self::path(dir);
        match std::fs::read_to_string(&p) {
            Ok(t) => match serde_json::from_str(t.trim_start_matches(BOM)) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("settings.json did not parse ({e}); using defaults");
                    let _ = std::fs::copy(&p, dir.join("settings.bad.json"));
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self, dir: &Path) {
        let p = Self::path(dir);
        let tmp = p.with_extension("json.tmp");
        if let Ok(t) = serde_json::to_string_pretty(self) {
            if std::fs::write(&tmp, t).is_ok() {
                let _ = std::fs::rename(&tmp, &p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_pad_defaults_off_and_round_trips() {
        assert_eq!(Settings::default().fallback_pad, FallbackPad::Off);
        let parsed: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.fallback_pad, FallbackPad::Off);
        let parsed: Settings = serde_json::from_str(r#"{"fallback_pad":"DualShock4"}"#).unwrap();
        assert_eq!(parsed.fallback_pad, FallbackPad::DualShock4);
    }

    #[test]
    fn quick_charge_defaults_off_and_old_settings_load_with_it_off() {
        assert!(!Settings::default().quick_charge);
        let parsed: Settings = serde_json::from_str("{}").unwrap();
        assert!(!parsed.quick_charge);
        // A settings.json saved before the option existed.
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old.as_object_mut().unwrap().remove("quick_charge");
        old["idle_off_minutes"] = 5.into();
        let parsed: Settings = serde_json::from_value(old).unwrap();
        assert!(!parsed.quick_charge);
        assert_eq!(parsed, Settings { idle_off_minutes: 5, ..Settings::default() });
        let parsed: Settings = serde_json::from_str(r#"{"quick_charge":true}"#).unwrap();
        assert!(parsed.quick_charge);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert!(back.quick_charge);
    }

    #[test]
    fn fallback_replaces_only_an_unforced_profile() {
        use crate::profile::VirtualKind;
        assert_eq!(
            effective_virtual_kind(VirtualKind::DualSense, false, true, FallbackPad::DualShock4),
            VirtualKind::DualShock4
        );
        assert_eq!(
            effective_virtual_kind(VirtualKind::DualSense, true, true, FallbackPad::DualShock4),
            VirtualKind::DualSense,
            "a DualSense game's profile keeps its own pad"
        );
        assert_eq!(
            effective_virtual_kind(VirtualKind::DualShock4, false, false, FallbackPad::Xbox360),
            VirtualKind::DualShock4
        );
        assert_eq!(
            effective_virtual_kind(VirtualKind::Xbox360, false, true, FallbackPad::Off),
            VirtualKind::Xbox360
        );
    }
}
