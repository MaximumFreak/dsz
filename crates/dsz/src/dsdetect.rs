//! Spot games with native DualSense support so the game watcher can switch
//! to a DualSense profile for them without a per-game rule.
//!
//! A game that drives haptics and adaptive triggers itself ships a
//! DualSense library or links one in. The scan reads a newly seen program
//! once and looks for:
//! - Sony's PC pad library: `libScePad*.dll` beside the exe, or its imports
//!   and API names in the exe (`libScePad`, `scePadSetTriggerEffect`, ...);
//! - Wwise's DualSense haptics sink (`AkScePad...`, as in Alan Wake 2);
//! - the DualSenseWindows library (`ds5w`);
//! - DualSense controller names that engines use for their haptics path;
//! - a DirectInput mapping for the DualSense or Edge (its product id,
//!   `0CE6054C-0000-0000-0000...`, as in Final Fantasy XIV);
//! - a wide-string (UTF-16) "DualSense" together with `hid.dll`: the game
//!   names the pad in its own UI or device list and talks to it over HID
//!   (Animal Well, and Unreal's WinDualShock plugin);
//! - SDL with its PS5 driver (`SDL_JOYSTICK_HIDAPI_PS5`), built into the
//!   exe (Astral Ascent), or in an `SDL2.dll`/`SDL3.dll` beside it when the
//!   exe opens gamepads through it. SDL reads a DualSense natively (rumble,
//!   lightbar, player LEDs) and shows PlayStation prompts;
//! - Rewired, the Unity input plugin, with its DualSense driver
//!   (`DualSenseExtension` in `<game>_Data\Managed\Rewired_Core.dll`, as in
//!   Death Squared).
//!
//! "DualSense Wireless Controller" alone is not a marker: SDL builds carry
//! that name in their controller list whether or not the PS5 driver is in.
//! An SDL library beside the exe isn't either: some games use SDL for the
//! window or sound and read pads through XInput, and would lose the
//! controller in DualSense mode.
//!
//! Results are cached by path, size, and modification time in
//! `dualsense-detect.json`, so each exe is read once. A cached "no" from an
//! older scan ([`SCAN_VERSION`]) is checked again.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Byte strings that mean the program talks to a DualSense itself.
const MARKERS: &[&str] = &[
    "libScePad",
    "scePadOpen",
    "scePadSetTriggerEffect",
    "scePadSetVibrationMode",
    "AkScePad",
    "ds5w_",
    "DualSenseWindows",
    "DualSense Controller",
    // DirectInput product GUIDs (PID then VID) that a game maps buttons for.
    "0CE6054C-0000-0000-0000",
    "0DF2054C-0000-0000-0000",
    // SDL built in, with its PS5 driver (the driver's hint name).
    SDL_PS5,
];

/// SDL's hint for its PS5 driver: in any SDL build that has the driver.
const SDL_PS5: &str = "SDL_JOYSTICK_HIDAPI_PS5";
/// SDL 2 and 3 gamepad and joystick openers, as an exe imports them.
const SDL_OPENERS: &[&str] = &[
    "SDL_GameControllerOpen",
    "SDL_OpenGamepad",
    "SDL_JoystickOpen",
    "SDL_OpenJoystick",
];
/// Rewired's DualSense driver, in `Rewired_Core.dll`.
const REWIRED_DS: &str = "DualSenseExtension";

/// Bumped when the scan learns new signs, so programs it said no to are
/// read again.
const SCAN_VERSION: u32 = 3;

/// Reported for a UTF-16 "DualSense" next to `hid.dll`.
const WIDE_WITH_HID: &str = "DualSense (UTF-16) with hid.dll";

/// Library files beside the exe (lower case prefixes).
const LIBRARIES: &[&str] = &["libscepad", "scepad", "ds5w"];

/// Programs that know about DualSense without being games.
const NOT_GAMES: &[&str] = &[
    "dsz.exe",
    "ds4windows.exe",
    "steam.exe",
    "steamwebhelper.exe",
    "gamebarpresencewriter.exe",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub size: u64,
    pub modified: u64,
    /// What was found, when the program supports DualSense.
    pub found: Option<String>,
    /// [`SCAN_VERSION`] of the scan that found it (0 before versions).
    #[serde(default)]
    pub version: u32,
}

pub struct Detector {
    file: PathBuf,
    cache: Mutex<BTreeMap<String, Entry>>,
}

impl Detector {
    pub fn new(data_dir: &Path) -> Detector {
        let file = data_dir.join("dualsense-detect.json");
        let cache = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str(t.trim_start_matches(crate::settings::BOM)).ok())
            .unwrap_or_default();
        Detector {
            file,
            cache: Mutex::new(cache),
        }
    }

    /// Why `exe` counts as a DualSense game, scanning it if it is new or
    /// changed. `None` for programs without DualSense support.
    pub fn check(&self, exe: &Path) -> Option<String> {
        let key = exe.to_string_lossy().to_ascii_lowercase();
        let name = exe.file_name()?.to_string_lossy().to_ascii_lowercase();
        if NOT_GAMES.contains(&name.as_str()) || is_system(&key) || key == *own_exe() {
            return None;
        }
        let meta = std::fs::metadata(exe).ok()?;
        let size = meta.len();
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(e) = self.cache.lock().get(&key) {
            if e.size == size
                && e.modified == modified
                && (e.found.is_some() || e.version >= SCAN_VERSION)
            {
                return e.found.clone();
            }
        }
        let t0 = std::time::Instant::now();
        let found = scan(exe);
        if let Some(why) = &found {
            log::info!(
                "{name} supports DualSense ({why}); scanned in {} ms",
                t0.elapsed().as_millis()
            );
        }
        let mut c = self.cache.lock();
        c.insert(
            key,
            Entry {
                size,
                modified,
                found: found.clone(),
                version: SCAN_VERSION,
            },
        );
        if let Ok(t) = serde_json::to_string_pretty(&*c) {
            let _ = std::fs::write(&self.file, t);
        }
        found
    }

    /// Every program found to support DualSense, by path.
    pub fn detected(&self) -> Vec<(String, String)> {
        self.cache
            .lock()
            .iter()
            .filter_map(|(k, e)| e.found.clone().map(|f| (k.clone(), f)))
            .collect()
    }
}

/// This app's own exe, lower case. It holds the scan's needles, so under
/// any name but dsz.exe it would count as a DualSense game.
fn own_exe() -> &'static String {
    static OWN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    OWN.get_or_init(|| {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    })
}

fn is_system(path_lower: &str) -> bool {
    let windir = std::env::var("SystemRoot")
        .unwrap_or_else(|_| r"C:\Windows".into())
        .to_ascii_lowercase();
    path_lower.starts_with(&windir)
        || (path_lower.contains(r"\steam\") && !path_lower.contains(r"\steamapps\"))
        // DSZ's own builds (%TEMP%\cargo-target\dsz, dsz-pr13, ...): test
        // binaries hold the scan's needles too.
        || path_lower.contains(r"\cargo-target\dsz")
}

/// Look beside and inside `exe` for a DualSense library.
pub fn scan(exe: &Path) -> Option<String> {
    let mut sdl_dll = None;
    if let Some(dir) = exe.parent() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if n.ends_with(".dll") && LIBRARIES.iter().any(|l| n.starts_with(l)) {
                    return Some(format!("ships {}", e.file_name().to_string_lossy()));
                }
                if n == "sdl2.dll" || n == "sdl3.dll" {
                    sdl_dll = Some(e.path());
                }
            }
        }
        // A Unity game with Rewired's DualSense driver.
        if let Some(stem) = exe.file_stem() {
            let core = dir
                .join(format!("{}_Data", stem.to_string_lossy()))
                .join("Managed")
                .join("Rewired_Core.dll");
            if open_find(&core, &[REWIRED_DS]).is_some() {
                return Some("Rewired with its DualSense driver".into());
            }
        }
    }
    let f = std::fs::File::open(exe).ok()?;
    if let Some(m) = find_marker(f) {
        return Some(if m == SDL_PS5 {
            "SDL with its PS5 driver, built in".into()
        } else {
            format!("\"{m}\" in the exe")
        });
    }
    // SDL as a library: it needs the PS5 driver and the exe has to open
    // gamepads through it.
    let dll = sdl_dll?;
    open_find(&dll, &[SDL_PS5])?;
    open_find(exe, SDL_OPENERS)?;
    Some(format!(
        "SDL with its PS5 driver ({})",
        dll.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// The first of `needles` in the file at `path`.
fn open_find(path: &Path, needles: &[&'static str]) -> Option<&'static str> {
    find_any(std::fs::File::open(path).ok()?, needles)
}

/// Stream `r` once and return the first of `needles` found.
fn find_any(mut r: impl Read, needles: &[&'static str]) -> Option<&'static str> {
    let longest = needles.iter().map(|m| m.len()).max()?;
    let firsts: Vec<u8> = needles.iter().filter_map(|m| m.bytes().next()).collect();
    let mut buf = vec![0u8; (8 << 20) + longest];
    let mut keep = 0usize;
    loop {
        let n = r.read(&mut buf[keep..]).ok()?;
        if n == 0 {
            return None;
        }
        let data = &buf[..keep + n];
        for i in 0..data.len() {
            if firsts.contains(&data[i]) {
                if let Some(m) = needles.iter().find(|m| data[i..].starts_with(m.as_bytes())) {
                    return Some(m);
                }
            }
        }
        keep = (longest - 1).min(data.len());
        let len = data.len();
        buf.copy_within(len - keep..len, 0);
    }
}

/// Stream `r` once and return the first marker found, or
/// [`WIDE_WITH_HID`] when both halves of that pair turned up.
fn find_marker(mut r: impl Read) -> Option<&'static str> {
    const WIDE: &[u8] = b"D\0u\0a\0l\0S\0e\0n\0s\0e\0";
    const HID: &[u8] = b"hid.dll";
    let (mut wide, mut hid) = (false, false);
    let longest = MARKERS
        .iter()
        .map(|m| m.len())
        .chain([WIDE.len(), HID.len()])
        .max()
        .unwrap_or(1);
    let mut buf = vec![0u8; (8 << 20) + longest];
    let mut keep = 0usize;
    loop {
        let n = r.read(&mut buf[keep..]).ok()?;
        if n == 0 {
            return (wide && hid).then_some(WIDE_WITH_HID);
        }
        let data = &buf[..keep + n];
        // Every marker starts with one of a few letters; jump between those.
        let mut i = 0;
        while i < data.len() {
            match data[i] {
                b'l' | b's' | b'A' | b'd' | b'D' | b'0' | b'S' => {
                    for m in MARKERS {
                        let mb = m.as_bytes();
                        if data[i..].starts_with(mb) {
                            return Some(m);
                        }
                    }
                    wide |= data[i..].starts_with(WIDE);
                }
                b'h' | b'H' => {
                    hid |= data[i..]
                        .get(..HID.len())
                        .is_some_and(|w| w.eq_ignore_ascii_case(HID));
                }
                _ => {}
            }
            i += 1;
        }
        keep = (longest - 1).min(data.len());
        let len = data.len();
        buf.copy_within(len - keep..len, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_markers_across_chunk_edges() {
        let mut v = vec![0u8; (8 << 20) - 4];
        v.extend_from_slice(b"AkScePadRumbleSink");
        v.extend_from_slice(&[0u8; 100]);
        assert_eq!(find_marker(&v[..]), Some("AkScePad"));
        assert_eq!(find_marker(&b"just an ordinary program"[..]), None);
        assert_eq!(
            find_marker(&b"..DualSense Controller (User %d) (Haptics).."[..]),
            Some("DualSense Controller")
        );
        // SDL's controller name list alone is not DualSense support.
        assert_eq!(find_marker(&b"..DualSense Wireless Controller.."[..]), None);
        // Its PS5 driver is (Astral Ascent).
        assert_eq!(
            find_marker(&b"..#@SDL_JOYSTICK_HIDAPI_PS5\0SDL_JOYSTICK_HIDAPI_PS5_RUMBLE.."[..]),
            Some(SDL_PS5)
        );
        assert_eq!(
            find_marker(&b"..DualSense Wireless Controller..HID.DLL.."[..]),
            None
        );
    }

    #[test]
    fn finds_directinput_and_hid_games() {
        // Final Fantasy XIV's button tables.
        assert_eq!(
            find_marker(&b"..{0CE6054C-0000-0000-0000504944564944}.."[..]),
            Some("0CE6054C-0000-0000-0000")
        );
        // Animal Well: the name in UTF-16 and HID imports, far apart.
        let wide: Vec<u8> = "PS DualSense - Native"
            .encode_utf16()
            .flat_map(|c| c.to_le_bytes())
            .collect();
        let mut v = wide.clone();
        v.extend(vec![0u8; 9 << 20]);
        v.extend_from_slice(b"HID.DLL\0HidD_GetAttributes");
        assert_eq!(find_marker(&v[..]), Some(WIDE_WITH_HID));
        // Either half alone is not enough.
        assert_eq!(find_marker(&wide[..]), None);
        assert_eq!(find_marker(&b"..hid.dll..HidD_GetAttributes.."[..]), None);
    }

    #[test]
    fn finds_sdl_libraries_and_rewired() {
        let dir = std::env::temp_dir().join(format!("dsz-detect-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Game_Data").join("Managed")).unwrap();
        let exe = dir.join("Game.exe");
        std::fs::write(&exe, b"MZ..SDL2.dll\0SDL_Init\0SDL_PollEvent").unwrap();
        // SDL beside a game that only makes its window with it.
        std::fs::write(dir.join("SDL2.dll"), b"..SDL_JOYSTICK_HIDAPI_PS5..").unwrap();
        assert_eq!(scan(&exe), None);
        // The game opens gamepads through it.
        std::fs::write(&exe, b"MZ..SDL2.dll\0SDL_Init\0SDL_GameControllerOpen").unwrap();
        assert_eq!(scan(&exe).as_deref(), Some("SDL with its PS5 driver (SDL2.dll)"));
        // An SDL too old for the PS5 driver.
        std::fs::write(dir.join("SDL2.dll"), b"..SDL_JOYSTICK_HIDAPI_PS4..").unwrap();
        assert_eq!(scan(&exe), None);
        // Rewired with its DualSense driver.
        std::fs::write(
            dir.join("Game_Data").join("Managed").join("Rewired_Core.dll"),
            b"..IDriver_DualSense..DualSenseExtension..",
        )
        .unwrap();
        assert_eq!(scan(&exe).as_deref(), Some("Rewired with its DualSense driver"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_system_and_tools() {
        assert!(is_system(r"c:\windows\system32\svchost.exe"));
        assert!(is_system(r"c:\program files (x86)\steam\steam.exe"));
        assert!(!is_system(
            r"c:\program files (x86)\steam\steamapps\common\game\game.exe"
        ));
        assert!(is_system(
            r"d:\build\cargo-target\dsz\release\deps\dsz-70018ed8bbf277c8.exe"
        ));
        assert!(is_system(
            r"d:\build\cargo-target\dsz-pr13\debug\dsz.exe"
        ));
    }
}
