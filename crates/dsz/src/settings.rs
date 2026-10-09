//! App-wide settings, `settings.json` in the data directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    /// Keep a virtual controller plugged in while its physical controller is
    /// off, and plug one in when the app starts, so games keep the same pad
    /// (and XInput slot) throughout.
    pub persistent_virtual: bool,
    /// MAC of the controller connected most recently; picks the profile for
    /// the virtual pad made at startup.
    pub last_controller: String,
    /// Controller MAC -> the device nodes HidHide hid for it, so the next
    /// start can hide it before it connects.
    pub hide_nodes: BTreeMap<String, Vec<String>>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
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
            ui_scale: 1.0,
            check_input_crc: true,
            show_tips: true,
            hidhide_control: true,
            default_on_start: false,
            auto_dualsense: true,
            dualsense_profile: "DualSense".into(),
            persistent_virtual: true,
            last_controller: String::new(),
            hide_nodes: BTreeMap::new(),
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
