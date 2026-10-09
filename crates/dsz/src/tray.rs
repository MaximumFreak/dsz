//! Notification-area icon on its own thread with its own message loop, so
//! it keeps working while the main window is hidden.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::engine::Engine;
use crate::platform::wide;

const WM_TRAY: u32 = WM_APP + 1;
const ID_SHOW: usize = 1;
const ID_QUIT: usize = 2;
const ID_PROFILE_BASE: usize = 100;

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();
static ON_SHOW: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

pub fn set_main_window(h: isize) {
    MAIN_HWND.store(h, Ordering::Release);
}

pub fn main_window() -> HWND {
    MAIN_HWND.load(Ordering::Acquire) as HWND
}

pub fn show_main_window() {
    let h = main_window();
    if !h.is_null() {
        unsafe {
            ShowWindow(h, SW_SHOW);
            if IsIconic(h) != 0 {
                ShowWindow(h, SW_RESTORE);
            }
            SetForegroundWindow(h);
        }
    }
    if let Some(f) = ON_SHOW.get() {
        f();
    }
}

pub fn hide_main_window() {
    let h = main_window();
    if !h.is_null() {
        unsafe {
            ShowWindow(h, SW_HIDE);
        }
    }
}

pub use crate::icon_art::icon_rgba;

/// The tray icon at the size Windows wants: the multi-size icon embedded
/// in the exe (resource 1, see `build.rs`), else drawn here.
unsafe fn make_hicon() -> HICON {
    let cx = GetSystemMetrics(SM_CXSMICON).max(16);
    let cy = GetSystemMetrics(SM_CYSMICON).max(16);
    let h = LoadImageW(
        GetModuleHandleW(null()),
        1 as _,
        IMAGE_ICON,
        cx,
        cy,
        LR_DEFAULTCOLOR,
    );
    if !h.is_null() {
        return h as HICON;
    }
    let size = cx as usize;
    let rgba = icon_rgba(size);
    // CreateIcon wants BGRA for the color plane and a 1-bpp AND mask.
    let mut bgra = vec![0u8; size * size * 4];
    for i in 0..size * size {
        bgra[i * 4] = rgba[i * 4 + 2];
        bgra[i * 4 + 1] = rgba[i * 4 + 1];
        bgra[i * 4 + 2] = rgba[i * 4];
        bgra[i * 4 + 3] = rgba[i * 4 + 3];
    }
    // Mask rows are padded to 16 bits.
    let and_mask = vec![0u8; size.div_ceil(16) * 2 * size];
    CreateIcon(
        GetModuleHandleW(null()),
        size as i32,
        size as i32,
        1,
        32,
        and_mask.as_ptr(),
        bgra.as_ptr(),
    )
}

fn set_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    let w: Vec<u16> = text.encode_utf16().take(127).collect();
    nid.szTip = [0; 128];
    nid.szTip[..w.len()].copy_from_slice(&w);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            let ev = (lp & 0xFFFF) as u32;
            if ev == WM_LBUTTONUP || ev == WM_LBUTTONDBLCLK {
                show_main_window();
            } else if ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU {
                show_menu(hwnd);
            }
            0
        }
        WM_COMMAND => {
            let id = wp & 0xFFFF;
            if id == ID_SHOW {
                show_main_window();
            } else if id == ID_QUIT {
                quit();
            } else if id >= ID_PROFILE_BASE {
                if let Some(e) = ENGINE.get() {
                    let names = e.profiles.names();
                    if let Some(name) = names.get(id - ID_PROFILE_BASE) {
                        let devs = e.device_list();
                        if devs.is_empty() {
                            e.settings.write().default_profile = name.clone();
                            e.settings_changed();
                        }
                        for d in devs {
                            e.set_device_profile(&d, name);
                            d.identify_light();
                        }
                    }
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    let show = wide("Open DSZ");
    AppendMenuW(menu, MF_STRING, ID_SHOW, show.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    let mut current = String::new();
    if let Some(e) = ENGINE.get() {
        if let Some(d) = e.device_list().first() {
            current = e.profile_name_for(d);
        } else {
            current = e.settings.read().default_profile.clone();
        }
        let sub = CreatePopupMenu();
        let mut labels = Vec::new();
        for (i, n) in e.profiles.names().iter().enumerate().take(50) {
            let w = wide(n);
            let flags = if *n == current {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            AppendMenuW(sub, flags, ID_PROFILE_BASE + i, w.as_ptr());
            labels.push(w);
        }
        let p = wide("Profile");
        AppendMenuW(menu, MF_POPUP, sub as usize, p.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
    }
    let quit = wide("Quit");
    AppendMenuW(menu, MF_STRING, ID_QUIT, quit.as_ptr());
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        null(),
    );
    DestroyMenu(menu);
    let _ = current;
}

pub fn quit() -> ! {
    if let Some(e) = ENGINE.get() {
        e.shutdown();
    }
    std::process::exit(0);
}

/// Spawn the tray thread. `on_show` runs after the window is shown (used to
/// wake the UI).
pub fn start(engine: Arc<Engine>, on_show: impl Fn() + Send + Sync + 'static) {
    let _ = ENGINE.set(engine.clone());
    let _ = ON_SHOW.set(Box::new(on_show));
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || unsafe {
            let class = wide("DSZTray");
            let hinst = GetModuleHandleW(null());
            let wc = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: null_mut(),
                hCursor: null_mut(),
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: class.as_ptr(),
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                hinst,
                null(),
            );
            if hwnd.is_null() {
                log::warn!("tray window creation failed");
                return;
            }
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY;
            nid.hIcon = make_hicon();
            set_tip(&mut nid, "DSZ");
            Shell_NotifyIconW(NIM_ADD, &nid);

            // Update the tooltip with battery levels now and then.
            SetTimer(hwnd, 1, 5000, None);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                if msg.message == WM_TIMER {
                    let devs = engine.device_list();
                    let tip = if devs.is_empty() {
                        "DSZ — no controller".to_string()
                    } else {
                        let parts: Vec<String> = devs
                            .iter()
                            .map(|d| {
                                format!(
                                    "{} {}%",
                                    d.model.name(),
                                    d.live.lock().input.battery_percent
                                )
                            })
                            .collect();
                        format!("DSZ — {}", parts.join(", "))
                    };
                    set_tip(&mut nid, &tip);
                    Shell_NotifyIconW(NIM_MODIFY, &nid);
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            Shell_NotifyIconW(NIM_DELETE, &nid);
        })
        .unwrap();
}
