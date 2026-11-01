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
//!
//! Task View opened by a Guide hold counts too (issue 31, found and first
//! patched by matheusbcbg): explorer.exe reads the parked pad for it, but
//! only Guide reached that pad, so while a game ran the switcher opened and
//! couldn't be moved. Unlike the overlay it is the foreground window, an
//! explorer.exe `XamlExplorerHostIslandWindow` (Windows 11; Windows 10's is
//! `MultitaskingViewFrame`). Alt+Tab, Snap Assist and other shell popups use
//! that class too, so it only counts when Guide was down on the parked pad
//! just before it came to the front, and only while that window stays in
//! front: Win+Tab or Alt+Tab from the keyboard are left as they were.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId,
    WindowFromPoint, SM_CXSCREEN, SM_CYSCREEN,
};

use crate::engine::Engine;

static OPEN: AtomicBool = AtomicBool::new(false);
static TASK_VIEW: AtomicBool = AtomicBool::new(false);
/// When Guide was last down on a parked pad, in ms after [`epoch`]; 0 never.
static GUIDE_AT: AtomicU64 = AtomicU64::new(0);

/// Check this often while a controller has a parked Xbox pad, else idle.
const EVERY: Duration = Duration::from_millis(50);
const IDLE_EVERY: Duration = Duration::from_millis(500);
/// Forget which processes are Game Bar this often (process ids get reused).
const NAMES_FOR: Duration = Duration::from_secs(30);
/// A shell switcher that comes to the front this soon after Guide was let
/// go is the one Guide opened (Task View shows up within about a second).
const GUIDE_OPENS_WITHIN: Duration = Duration::from_secs(2);
/// Foreground window classes of Task View in explorer.exe.
const TASK_VIEW_CLASSES: [&str; 2] = ["XamlExplorerHostIslandWindow", "MultitaskingViewFrame"];

/// The overlay or a Task View opened by Guide is open, as of the last check
/// (at most [`EVERY`] old).
pub fn open() -> bool {
    OPEN.load(Ordering::Relaxed) || TASK_VIEW.load(Ordering::Relaxed)
}

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Guide is down on a parked Xbox pad (called with every such report).
pub fn guide_down() {
    let ms = epoch().elapsed().as_millis() as u64;
    GUIDE_AT.store(ms.max(1), Ordering::Relaxed);
}

/// How long ago Guide was last down on a parked pad.
fn guide_age() -> Option<Duration> {
    let at = GUIDE_AT.load(Ordering::Relaxed);
    (at != 0).then(|| epoch().elapsed().saturating_sub(Duration::from_millis(at)))
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

fn is_task_view_class(class: &str) -> bool {
    TASK_VIEW_CLASSES.contains(&class)
}

fn is_explorer_exe(name: &str) -> bool {
    name.eq_ignore_ascii_case("explorer.exe")
}

/// The foreground window, when it is one of explorer.exe's Task View
/// classes. `names` caches process id → explorer.exe.
fn switcher_in_front(names: &mut HashMap<u32, bool>) -> Option<usize> {
    let mut class = [0u16; 64];
    let mut pid = 0u32;
    let (hwnd, len) = unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        GetWindowThreadProcessId(hwnd, &mut pid);
        (hwnd as usize, GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32))
    };
    if pid == 0 || len <= 0 || !is_task_view_class(&String::from_utf16_lossy(&class[..len as usize])) {
        return None;
    }
    let explorer = *names.entry(pid).or_insert_with(|| {
        crate::platform::process_path(pid)
            .and_then(|p| p.file_name().map(|n| is_explorer_exe(&n.to_string_lossy())))
            .unwrap_or(false)
    });
    explorer.then_some(hwnd)
}

/// The Task View window the controller drives now: `front` (the switcher
/// in front, if any) when it is the one already driven, or when Guide was
/// down on the parked pad within [`GUIDE_OPENS_WITHIN`]. Anything else in
/// front ends it at once, so the game gets the pad back as Task View closes.
fn task_view_follows(driven: Option<usize>, front: Option<usize>, guide_age: Option<Duration>) -> Option<usize> {
    let front = front?;
    if driven == Some(front) || guide_age.is_some_and(|a| a <= GUIDE_OPENS_WITHIN) {
        Some(front)
    } else {
        None
    }
}

/// The overlay watcher's thread; runs until the app exits.
pub fn run(engine: Arc<Engine>) {
    let mut names = HashMap::new();
    let mut explorers = HashMap::new();
    let mut task_view: Option<usize> = None;
    let mut names_at = Instant::now();
    while !engine.shutdown.load(Ordering::Acquire) {
        // Only a parked Xbox pad acts on it.
        let wanted = engine.device_list().iter().any(|d| d.virt.has_parked());
        let now = if wanted {
            if names_at.elapsed() >= NAMES_FOR {
                names.clear();
                explorers.clear();
                names_at = Instant::now();
            }
            probe(&mut names)
        } else {
            false
        };
        if OPEN.swap(now, Ordering::Relaxed) != now {
            log::info!("Game Bar {}", if now { "opened: controller input goes to it" } else { "closed" });
        }
        task_view = if wanted {
            task_view_follows(task_view, switcher_in_front(&mut explorers), guide_age())
        } else {
            None
        };
        let tv = task_view.is_some();
        if TASK_VIEW.swap(tv, Ordering::Relaxed) != tv {
            log::info!("Task View {}", if tv { "opened by Guide: controller input goes to it" } else { "closed" });
        }
        std::thread::sleep(if wanted { EVERY } else { IDLE_EVERY });
    }
    OPEN.store(false, Ordering::Relaxed);
    TASK_VIEW.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamebar_processes() {
        assert!(is_gamebar_exe("GameBar.exe"));
        assert!(is_gamebar_exe("XboxGameBarWidgets.exe"));
        assert!(!is_gamebar_exe("GameBarFTServer.exe"));
        assert!(!is_gamebar_exe("explorer.exe"));
    }

    #[test]
    fn task_view_windows() {
        assert!(is_task_view_class("XamlExplorerHostIslandWindow"));
        assert!(is_task_view_class("MultitaskingViewFrame"));
        assert!(!is_task_view_class("Shell_TrayWnd"));
        assert!(!is_task_view_class("ForegroundStaging"));
        assert!(!is_task_view_class("xamlexplorerhostislandwindow"));
        assert!(is_explorer_exe("explorer.exe"));
        assert!(is_explorer_exe("EXPLORER.EXE"));
        assert!(!is_explorer_exe("StartMenuExperienceHost.exe"));
        assert!(!is_explorer_exe("ShellExperienceHost.exe"));
    }

    #[test]
    fn task_view_needs_guide_to_open() {
        let ms = Duration::from_millis;
        // Guide held or just let go: the switcher in front is driven.
        assert_eq!(task_view_follows(None, Some(7), Some(ms(0))), Some(7));
        assert_eq!(task_view_follows(None, Some(7), Some(ms(2000))), Some(7));
        // Win+Tab or Alt+Tab from the keyboard, Guide long ago or never.
        assert_eq!(task_view_follows(None, Some(7), Some(ms(2001))), None);
        assert_eq!(task_view_follows(None, Some(7), None), None);
        // Nothing of the shell in front (the game): never.
        assert_eq!(task_view_follows(None, None, Some(ms(0))), None);
    }

    #[test]
    fn task_view_lasts_while_in_front() {
        let ms = Duration::from_millis;
        // Stays driven however long it stays open.
        assert_eq!(task_view_follows(Some(7), Some(7), Some(ms(60_000))), Some(7));
        assert_eq!(task_view_follows(Some(7), Some(7), None), Some(7));
        // Closed (the game or a picked window in front): the game gets the
        // pad back on the next check.
        assert_eq!(task_view_follows(Some(7), None, Some(ms(60_000))), None);
        // Another shell popup later, without Guide, isn't driven.
        assert_eq!(task_view_follows(Some(7), Some(8), Some(ms(60_000))), None);
    }
}
