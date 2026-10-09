//! Small Windows helpers: thread priority, timer resolution, Bluetooth
//! disconnect, autostart, process list, single instance.

use std::ffi::c_void;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::*;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn raise_thread_priority(critical: bool) {
    unsafe {
        let p = if critical {
            THREAD_PRIORITY_TIME_CRITICAL
        } else {
            THREAD_PRIORITY_HIGHEST
        };
        SetThreadPriority(GetCurrentThread(), p);
    }
}

/// 1 ms system timer for the whole process, requested once. The writer's
/// 8 ms tick and every timed wait depend on it.
pub fn timer_resolution_1ms() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        windows_sys::Win32::Media::timeBeginPeriod(1);
    });
}

/// Keep timers and scheduling at full speed while the window is hidden.
/// Windows 11 ignores a hidden or minimized process's timer-resolution
/// request (so 8 ms waits become 15.6 ms in the tray) and may run it under
/// EcoQoS; a controller driver has to stay on time either way.
pub fn opt_out_of_power_throttling() {
    unsafe {
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            StateMask: 0,
        };
        let ok = SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
        if ok == 0 {
            log::debug!(
                "power throttling opt-out failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// A high-resolution waitable timer for the haptics packet clock.
pub struct PreciseTimer(HANDLE);
unsafe impl Send for PreciseTimer {}

impl PreciseTimer {
    pub fn new() -> PreciseTimer {
        unsafe {
            // CREATE_WAITABLE_TIMER_HIGH_RESOLUTION = 0x2
            let mut h = CreateWaitableTimerExW(null(), null(), 0x0000_0002, TIMER_ALL_ACCESS);
            if h.is_null() {
                h = CreateWaitableTimerExW(null(), null(), 0, TIMER_ALL_ACCESS);
            }
            PreciseTimer(h)
        }
    }

    /// Sleep until `deadline` (falls back to `thread::sleep`).
    pub fn sleep_until(&self, deadline: std::time::Instant) {
        let now = std::time::Instant::now();
        if deadline <= now {
            return;
        }
        let d = deadline - now;
        if self.0.is_null() {
            std::thread::sleep(d);
            return;
        }
        unsafe {
            let due: i64 = -((d.as_nanos() / 100) as i64).max(1);
            if SetWaitableTimer(self.0, &due, 0, None, null(), 0) != 0 {
                WaitForSingleObject(self.0, INFINITE);
            } else {
                std::thread::sleep(d);
            }
        }
    }
}

impl Drop for PreciseTimer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// Drop a Bluetooth controller's link (it powers off). `mac` like `AA:BB:..`.
pub fn bt_disconnect(mac: &str) -> bool {
    use windows_sys::Win32::Devices::Bluetooth::*;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let hex: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let Ok(addr) = u64::from_str_radix(&hex, 16) else {
        return false;
    };
    if hex.len() != 12 {
        return false;
    }
    const IOCTL_BTH_DISCONNECT_DEVICE: u32 = 0x0041_000C;
    let mut ok = false;
    unsafe {
        let params = BLUETOOTH_FIND_RADIO_PARAMS {
            dwSize: std::mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
        };
        let mut radio: HANDLE = null_mut();
        let find = BluetoothFindFirstRadio(&params, &mut radio);
        if find.is_null() {
            return false;
        }
        loop {
            let mut ret = 0u32;
            if DeviceIoControl(
                radio,
                IOCTL_BTH_DISCONNECT_DEVICE,
                &addr as *const u64 as *const c_void,
                8,
                null_mut(),
                0,
                &mut ret,
                null_mut(),
            ) != 0
            {
                ok = true;
            }
            CloseHandle(radio);
            if ok || BluetoothFindNextRadio(find, &mut radio) == 0 {
                break;
            }
        }
        BluetoothFindRadioClose(find);
    }
    if ok {
        log::info!("disconnected Bluetooth controller {mac}");
    } else {
        log::warn!("could not disconnect {mac}");
    }
    ok
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const APP_VALUE: &str = "DSZ";

pub fn set_autostart(on: bool) -> bool {
    unsafe {
        let key = wide(RUN_KEY);
        let name = wide(APP_VALUE);
        if on {
            let Ok(exe) = std::env::current_exe() else {
                return false;
            };
            let cmd = format!("\"{}\" --minimized", exe.display());
            let val = wide(&cmd);
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                val.as_ptr() as *const c_void,
                (val.len() * 2) as u32,
            ) == 0
        } else {
            let r = RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr());
            r == 0 || r == ERROR_FILE_NOT_FOUND
        }
    }
}

/// Lower-cased exe names of running processes.
pub fn process_names() -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut e) != 0 {
            loop {
                let n = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                out.insert(String::from_utf16_lossy(&e.szExeFile[..n]).to_ascii_lowercase());
                if Process32NextW(snap, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// Running processes as (process id, lower-case exe name).
pub fn process_list() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut e) != 0 {
            loop {
                let n = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                out.push((
                    e.th32ProcessID,
                    String::from_utf16_lossy(&e.szExeFile[..n]).to_ascii_lowercase(),
                ));
                if Process32NextW(snap, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// Full image path of a process, when we may query it.
pub fn process_path(pid: u32) -> Option<std::path::PathBuf> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        (ok != 0).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

/// Returns false if another instance already holds the mutex.
pub fn single_instance() -> bool {
    unsafe {
        let name = wide("Local\\DSZ.SingleInstance");
        let h = CreateMutexW(null(), 0, name.as_ptr());
        if h.is_null() {
            return true;
        }
        // Leak the handle: it lives as long as the process.
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

/// Bring an already-running instance's window to the front.
pub fn activate_existing(title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let t = wide(title);
        let h = FindWindowW(null(), t.as_ptr());
        if !h.is_null() {
            ShowWindow(h, SW_SHOW);
            ShowWindow(h, SW_RESTORE);
            SetForegroundWindow(h);
        }
    }
}

/// Outer rectangle of the main window, in screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowFrame {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// What the UI should do after [`maintain_window`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowCare {
    /// The client area is large enough to lay out.
    pub draw: bool,
    /// `Some(0)` repaints immediately. `Some(ms)` waits. `None` leaves pacing
    /// to the caller.
    pub wake_ms: Option<u64>,
}

/// Remember a real window size and put it back when restore-from-minimize
/// leaves only the title-bar frame.
///
/// While a window is minimized, Windows reports a 0×0 client area. winit then
/// applies that size with `SetWindowPos`, which replaces the restored
/// rectangle. The taskbar restore comes back as an empty frame (about 14×38)
/// and the UI has nothing to draw.
pub fn maintain_window(
    hwnd: isize,
    saved: &mut Option<WindowFrame>,
    logged: &mut bool,
) -> WindowCare {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClientRect, GetWindowRect, IsIconic, IsWindowVisible,
    };
    if hwnd == 0 {
        return WindowCare {
            draw: true,
            wake_ms: None,
        };
    }
    let hwnd = hwnd as HWND;
    unsafe {
        let visible = IsWindowVisible(hwnd) != 0;
        let iconic = IsIconic(hwnd) != 0;
        let mut client: RECT = std::mem::zeroed();
        if GetClientRect(hwnd, &mut client) == 0 {
            return WindowCare {
                draw: true,
                wake_ms: None,
            };
        }
        let cw = client.right - client.left;
        let ch = client.bottom - client.top;
        let client_ok = cw >= 200 && ch >= 200;
        if client_ok && !iconic {
            let mut outer: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut outer) != 0 {
                *saved = Some(WindowFrame {
                    x: outer.left,
                    y: outer.top,
                    w: outer.right - outer.left,
                    h: outer.bottom - outer.top,
                });
            }
            *logged = false;
        }

        if iconic {
            if placement_collapsed(hwnd) {
                place_window(hwnd, saved, logged, cw, ch, true);
            }
            // Keep looking. A later `SetWindowPos` from winit can still
            // overwrite the restored rectangle after this frame.
            return WindowCare {
                draw: false,
                wake_ms: Some(400),
            };
        }

        if !client_ok {
            // Shown: the restored window is an empty frame. Hidden: the
            // client never got a real size, so the next show would be too.
            place_window(hwnd, saved, logged, cw, ch, visible);
            return WindowCare {
                draw: false,
                wake_ms: visible.then_some(0),
            };
        }

        WindowCare {
            draw: true,
            wake_ms: None,
        }
    }
}

fn placement_collapsed(hwnd: HWND) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, WINDOWPLACEMENT};
    unsafe {
        let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
        wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        if GetWindowPlacement(hwnd, &mut wp) == 0 {
            return false;
        }
        let r = wp.rcNormalPosition;
        r.right - r.left < 400 || r.bottom - r.top < 300
    }
}

fn place_window(
    hwnd: HWND,
    saved: &Option<WindowFrame>,
    logged: &mut bool,
    cw: i32,
    ch: i32,
    announce: bool,
) {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        let frame = if GetMonitorInfoW(monitor, &mut info) != 0 {
            let work = info.rcWork;
            let want = saved.unwrap_or_else(|| {
                default_frame(
                    work.left,
                    work.top,
                    work.right - work.left,
                    work.bottom - work.top,
                )
            });
            fit_frame(
                want,
                work.left,
                work.top,
                work.right - work.left,
                work.bottom - work.top,
            )
        } else {
            saved.unwrap_or(WindowFrame {
                x: 80,
                y: 60,
                w: 1280,
                h: 860,
            })
        };
        if announce && !*logged {
            log::info!(
                "window collapsed to {cw}x{ch} client, restoring to {}x{}",
                frame.w,
                frame.h
            );
        }
        *logged = true;
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            frame.x,
            frame.y,
            frame.w,
            frame.h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

fn default_frame(work_x: i32, work_y: i32, work_w: i32, work_h: i32) -> WindowFrame {
    let work_w = work_w.max(1);
    let work_h = work_h.max(1);
    let w = ((work_w * 3) / 4).clamp(960, 1560).min(work_w);
    let h = ((work_h * 4) / 5).clamp(680, 1000).min(work_h);
    fit_frame(
        WindowFrame {
            x: work_x + (work_w - w) / 2,
            y: work_y + (work_h - h) / 2,
            w,
            h,
        },
        work_x,
        work_y,
        work_w,
        work_h,
    )
}

fn fit_frame(
    frame: WindowFrame,
    work_x: i32,
    work_y: i32,
    work_w: i32,
    work_h: i32,
) -> WindowFrame {
    let work_w = work_w.max(1);
    let work_h = work_h.max(1);
    let w = frame.w.clamp(1, work_w);
    let h = frame.h.clamp(1, work_h);
    WindowFrame {
        x: frame.x.clamp(work_x, work_x + work_w - w),
        y: frame.y.clamp(work_y, work_y + work_h - h),
        w,
        h,
    }
}

pub fn data_dir() -> std::path::PathBuf {
    // `DSZ_DATA_DIR` keeps a test run away from the real settings.
    if let Some(d) = std::env::var_os("DSZ_DATA_DIR") {
        let d = std::path::PathBuf::from(d);
        let _ = std::fs::create_dir_all(&d);
        return d;
    }
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let d = base.join("DSZ");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn open_in_explorer(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

/// Reuse the parent console (release builds have none of their own), so
/// diagnostics printed from the command line are visible.
pub fn attach_console() {
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

#[cfg(test)]
mod window_frame_tests {
    use super::{default_frame, fit_frame, WindowFrame};

    #[test]
    fn fit_keeps_a_frame_that_is_already_inside() {
        let frame = WindowFrame {
            x: 100,
            y: 80,
            w: 1400,
            h: 900,
        };
        assert_eq!(fit_frame(frame, 0, 0, 2560, 1440), frame);
    }

    #[test]
    fn fit_clamps_a_collapsed_frame_into_the_work_area() {
        let frame = WindowFrame {
            x: 4000,
            y: -40,
            w: 4000,
            h: 20,
        };
        let fit = fit_frame(frame, 0, 0, 1920, 1080);
        assert_eq!(fit.w, 1920);
        assert_eq!(fit.h, 20);
        assert_eq!(fit.x, 0);
        assert!(fit.y >= 0);
    }

    #[test]
    fn default_frame_sits_inside_a_small_work_area() {
        let frame = default_frame(10, 20, 1280, 800);
        assert!(frame.w <= 1280 && frame.h <= 800);
        assert!(frame.x >= 10 && frame.y >= 20);
        assert!(frame.x + frame.w <= 1290);
        assert!(frame.y + frame.h <= 820);
    }
}
