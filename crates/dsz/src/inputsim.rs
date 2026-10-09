//! Keyboard and mouse output through `SendInput`.
//!
//! Presses are reference counted process-wide, so two sources holding the
//! same key (a mapping and a stick, or two controllers) release it only
//! when both let go.

use std::collections::HashMap;
use std::sync::OnceLock;

use parking_lot::Mutex;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

use crate::profile::MouseButton;

fn held() -> &'static Mutex<HashMap<u32, u32>> {
    static H: OnceLock<Mutex<HashMap<u32, u32>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

fn send(inputs: &[INPUT]) {
    if inputs.is_empty() {
        return;
    }
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

fn mouse_input(dx: i32, dy: i32, data: i32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub fn mouse_move(dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    send(&[mouse_input(dx, dy, 0, MOUSEEVENTF_MOVE)]);
}

/// Wheel delta in 1/120 notches. Positive is up / right.
pub fn wheel(delta: i32, horizontal: bool) {
    if delta == 0 {
        return;
    }
    let f = if horizontal {
        MOUSEEVENTF_HWHEEL
    } else {
        MOUSEEVENTF_WHEEL
    };
    send(&[mouse_input(0, 0, delta, f)]);
}

fn button_flags(b: MouseButton, down: bool) -> (u32, i32) {
    match (b, down) {
        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, 1),
        (MouseButton::Back, false) => (MOUSEEVENTF_XUP, 1),
        (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
        (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, 2),
    }
}

fn mouse_key(b: MouseButton) -> u32 {
    0x1_0000 | b as u32
}

pub fn mouse_button(b: MouseButton, down: bool) {
    let k = mouse_key(b);
    if !refcount(k, down) {
        return;
    }
    let (f, data) = button_flags(b, down);
    send(&[mouse_input(0, 0, data, f)]);
}

/// Let go of every key and mouse button this app is holding down. For
/// emergencies (a crashed input thread), when per-mapping state is lost.
pub fn release_everything() {
    let held: Vec<u32> = held().lock().drain().map(|(k, _)| k).collect();
    for k in held {
        if k & 0x1_0000 != 0 {
            let b = match k & 0xFFFF {
                x if x == MouseButton::Left as u32 => MouseButton::Left,
                x if x == MouseButton::Right as u32 => MouseButton::Right,
                x if x == MouseButton::Middle as u32 => MouseButton::Middle,
                x if x == MouseButton::Back as u32 => MouseButton::Back,
                _ => MouseButton::Forward,
            };
            let (f, data) = button_flags(b, false);
            send(&[mouse_input(0, 0, data, f)]);
        } else {
            send(&[key_input(k as u16, false)]);
        }
    }
}

/// Returns true when the transition should be sent.
fn refcount(k: u32, down: bool) -> bool {
    let mut h = held().lock();
    let c = h.entry(k).or_insert(0);
    if down {
        *c += 1;
        *c == 1
    } else if *c == 0 {
        false
    } else {
        *c -= 1;
        if *c == 0 {
            h.remove(&k);
            true
        } else {
            false
        }
    }
}

fn is_extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // page up/down, end, home, arrows
            | 0x2D | 0x2E // insert, delete
            | 0x5B | 0x5C | 0x5D // win keys, apps
            | 0x6F // numpad divide
            | 0x90 // num lock
            | 0xA3 | 0xA5 // right ctrl, right alt
            | 0xAD..=0xB7 // media and volume keys
    )
}

fn key_input(vk: u16, down: bool) -> INPUT {
    let media = (0xA6..=0xB7).contains(&vk);
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    let mut flags = 0;
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    // Scan codes reach games that read raw keyboard input; media keys only
    // work as virtual keys.
    if !media && scan != 0 {
        flags |= KEYEVENTF_SCANCODE;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

pub fn key(vk: u16, down: bool) {
    if vk == 0 || !refcount(vk as u32, down) {
        return;
    }
    send(&[key_input(vk, down)]);
}

/// Name for a virtual-key code, for the UI.
pub fn key_name(vk: u16) -> String {
    match vk {
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x10 => "Shift".into(),
        0x11 => "Ctrl".into(),
        0x12 => "Alt".into(),
        0x13 => "Pause".into(),
        0x14 => "Caps Lock".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x21 => "Page Up".into(),
        0x22 => "Page Down".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2C => "Print Screen".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x30..=0x39 | 0x41..=0x5A => (vk as u8 as char).to_string(),
        0x5B => "Win".into(),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x6A => "Num *".into(),
        0x6B => "Num +".into(),
        0x6D => "Num -".into(),
        0x6E => "Num .".into(),
        0x6F => "Num /".into(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0xA0 => "Left Shift".into(),
        0xA1 => "Right Shift".into(),
        0xA2 => "Left Ctrl".into(),
        0xA3 => "Right Ctrl".into(),
        0xA4 => "Left Alt".into(),
        0xA5 => "Right Alt".into(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        _ => format!("Key 0x{vk:02X}"),
    }
}

/// Keys offered in the picker, in a sensible order.
pub fn key_catalog() -> Vec<u16> {
    let mut v: Vec<u16> = Vec::new();
    v.extend(0x41..=0x5A); // A-Z
    v.extend(0x30..=0x39); // 0-9
    v.extend([
        0x20, 0x0D, 0x1B, 0x09, 0x08, 0x2E, 0x2D, 0x24, 0x23, 0x21, 0x22,
    ]);
    v.extend([0x26, 0x28, 0x25, 0x27]);
    v.extend([0xA0, 0xA2, 0xA4, 0xA1, 0xA3, 0xA5, 0x5B, 0x14]);
    v.extend(0x70..=0x7B); // F1-F12
    v.extend(0x60..=0x69);
    v.extend([0x6A, 0x6B, 0x6D, 0x6E, 0x6F]);
    v.extend([
        0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC0, 0xDB, 0xDC, 0xDD, 0xDE, 0x2C, 0x13,
    ]);
    v
}

/// Translate an egui key to a virtual-key code.
pub fn vk_from_egui(k: eframe::egui::Key) -> Option<u16> {
    use eframe::egui::Key as K;
    let name = k.name();
    if name.len() == 1 {
        let c = name.chars().next().unwrap().to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Some(c as u16);
        }
    }
    Some(match k {
        K::Space => 0x20,
        K::Enter => 0x0D,
        K::Escape => 0x1B,
        K::Tab => 0x09,
        K::Backspace => 0x08,
        K::Delete => 0x2E,
        K::Insert => 0x2D,
        K::Home => 0x24,
        K::End => 0x23,
        K::PageUp => 0x21,
        K::PageDown => 0x22,
        K::ArrowUp => 0x26,
        K::ArrowDown => 0x28,
        K::ArrowLeft => 0x25,
        K::ArrowRight => 0x27,
        K::F1 => 0x70,
        K::F2 => 0x71,
        K::F3 => 0x72,
        K::F4 => 0x73,
        K::F5 => 0x74,
        K::F6 => 0x75,
        K::F7 => 0x76,
        K::F8 => 0x77,
        K::F9 => 0x78,
        K::F10 => 0x79,
        K::F11 => 0x7A,
        K::F12 => 0x7B,
        K::Minus => 0xBD,
        K::Equals => 0xBB,
        K::Comma => 0xBC,
        K::Period => 0xBE,
        K::Slash => 0xBF,
        K::Semicolon => 0xBA,
        K::OpenBracket => 0xDB,
        K::CloseBracket => 0xDD,
        K::Backslash => 0xDC,
        K::Backtick => 0xC0,
        K::Quote => 0xDE,
        _ => return None,
    })
}
