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
//! - DualSense controller names that engines use for their haptics path.
//!
//! "DualSense Wireless Controller" is not a marker: SDL carries that name
//! in its controller list, so it is in nearly every SDL game, and those
//! games only rumble.
//!
//! Results are cached by path, size, and modification time in
//! `dualsense-detect.json`, so each exe is read once.

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
];

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
        if NOT_GAMES.contains(&name.as_str()) || is_system(&key) {
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
            if e.size == size && e.modified == modified {
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

fn is_system(path_lower: &str) -> bool {
    let windir = std::env::var("SystemRoot")
        .unwrap_or_else(|_| r"C:\Windows".into())
        .to_ascii_lowercase();
    path_lower.starts_with(&windir)
        || (path_lower.contains(r"\steam\") && !path_lower.contains(r"\steamapps\"))
        || path_lower.contains(r"\dualsense-rust\target\")
}

/// Look beside and inside `exe` for a DualSense library.
pub fn scan(exe: &Path) -> Option<String> {
    if let Some(dir) = exe.parent() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if n.ends_with(".dll") && LIBRARIES.iter().any(|l| n.starts_with(l)) {
                    return Some(format!("ships {}", e.file_name().to_string_lossy()));
                }
            }
        }
    }
    let f = std::fs::File::open(exe).ok()?;
    find_marker(f).map(|m| format!("\"{m}\" in the exe"))
}

/// Stream `r` once and return the first marker found.
fn find_marker(mut r: impl Read) -> Option<&'static str> {
    let longest = MARKERS.iter().map(|m| m.len()).max().unwrap_or(1);
    let mut buf = vec![0u8; (8 << 20) + longest];
    let mut keep = 0usize;
    loop {
        let n = r.read(&mut buf[keep..]).ok()?;
        if n == 0 {
            return None;
        }
        let data = &buf[..keep + n];
        // Every marker starts with one of a few letters; jump between those.
        let mut i = 0;
        while i < data.len() {
            match data[i] {
                b'l' | b's' | b'A' | b'd' | b'D' => {
                    for m in MARKERS {
                        let mb = m.as_bytes();
                        if data[i..].starts_with(mb) {
                            return Some(m);
                        }
                    }
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
    }

    #[test]
    fn skips_system_and_tools() {
        assert!(is_system(r"c:\windows\system32\svchost.exe"));
        assert!(is_system(r"c:\program files (x86)\steam\steam.exe"));
        assert!(!is_system(
            r"c:\program files (x86)\steam\steamapps\common\game\game.exe"
        ));
    }
}
