//! Whether the Xbox Game Bar overlay is open, so a controller with a parked
//! Xbox pad can drive it (Game Bar no longer reads DualSense input, tested
//! 2026-10-20 on Game Bar 7.326.10141).
//!
//! The open overlay is a full-screen CoreWindow owned by GameBar.exe that
//! never becomes the foreground window and that `EnumWindows` never lists;
//! it is destroyed when the overlay closes. So the screen is hit-tested
//! instead: open when at least 4 of 5 points on the primary screen land on
//! Game Bar (a pinned widget is small). Same probe as the Global Hotkeys
//! scripts (GameBar\docs\OVERLAY_STATE_DETECTION.md), verified open and
//! closed on 7.326.10141.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, GetWindowThreadProcessId, WindowFromPoint, SM_CXSCREEN, SM_CYSCREEN,
};

use crate::engine::Engine;

static OPEN: AtomicBool = AtomicBool::new(false);

/// Check this often while a controller has a parked Xbox pad, else idle.
const EVERY: Duration = Duration::from_millis(50);
const IDLE_EVERY: Duration = Duration::from_millis(500);
/// Forget which processes are Game Bar this often (process ids get reused).
const NAMES_FOR: Duration = Duration::from_secs(30);

/// The overlay is open, as of the last check (at most [`EVERY`] old).
pub fn open() -> bool {
    OPEN.load(Ordering::Relaxed)
}

fn is_gamebar_exe(name: &str) -> bool {
    name.eq_ignore_ascii_case("gamebar.exe") || name.eq_ignore_ascii_case("xboxgamebarwidgets.exe")
}

/// Hit-test the primary screen. `names` caches process id → Game Bar.
fn probe(names: &mut HashMap<u32, bool>) -> bool {
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    if w <= 0 || h <= 0 {
        return false;
    }
    let points = [
        (w / 2, h / 2),
        (w / 4, h / 4),
        (w * 3 / 4, h / 4),
        (w / 4, h * 3 / 4),
        (w * 3 / 4, h * 3 / 4),
    ];
    let mut hits = 0;
    for (i, (x, y)) in points.into_iter().enumerate() {
        let mut pid = 0u32;
        unsafe {
            let hwnd = WindowFromPoint(POINT { x, y });
            if hwnd.is_null() {
                continue;
            }
            GetWindowThreadProcessId(hwnd, &mut pid);
        }
        if pid == 0 {
            continue;
        }
        let gb = *names.entry(pid).or_insert_with(|| {
            crate::platform::process_path(pid)
                .and_then(|p| p.file_name().map(|n| is_gamebar_exe(&n.to_string_lossy())))
                .unwrap_or(false)
        });
        if gb {
            hits += 1;
        }
        // Two misses: it can't reach 4.
        if i + 1 - hits >= 2 {
            return false;
        }
    }
    hits >= 4
}

/// The overlay watcher's thread; runs until the app exits.
pub fn run(engine: Arc<Engine>) {
    let mut names = HashMap::new();
    let mut names_at = Instant::now();
    while !engine.shutdown.load(Ordering::Acquire) {
        // Only a parked Xbox pad acts on it.
        let wanted = engine.device_list().iter().any(|d| d.virt.has_parked());
        let now = if wanted {
            if names_at.elapsed() >= NAMES_FOR {
                names.clear();
                names_at = Instant::now();
            }
            probe(&mut names)
        } else {
            false
        };
        if OPEN.swap(now, Ordering::Relaxed) != now {
            log::info!("Game Bar {}", if now { "opened: controller input goes to it" } else { "closed" });
        }
        std::thread::sleep(if wanted { EVERY } else { IDLE_EVERY });
    }
    OPEN.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::is_gamebar_exe;

    #[test]
    fn gamebar_processes() {
        assert!(is_gamebar_exe("GameBar.exe"));
        assert!(is_gamebar_exe("XboxGameBarWidgets.exe"));
        assert!(!is_gamebar_exe("GameBarFTServer.exe"));
        assert!(!is_gamebar_exe("explorer.exe"));
    }
}
