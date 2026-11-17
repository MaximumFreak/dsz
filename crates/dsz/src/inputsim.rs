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

/// Number pad digits and decimal point share their scan codes with the
/// navigation keys (Numpad 1 is End), and Windows picks between them by Num
/// Lock.
fn is_numpad(vk: u16) -> bool {
    matches!(vk, 0x60..=0x69 | 0x6E)
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
    // work as virtual keys. Number pad keys go as virtual keys too, with
    // their scan code alongside, so Num Lock off doesn't turn them into the
    // navigation keys.
    if !media && !is_numpad(vk) && scan != 0 {
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

/// How one character of a typed text goes out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stroke {
    /// Its key on the keyboard layout, with or without Shift.
    Key { vk: u16, shift: bool },
    /// The character itself, for ones the layout has no plain key for.
    Unicode,
}

/// `scan` is `VkKeyScanW`: the key in the low byte, the modifiers it needs
/// in the high byte (1 Shift, 2 Ctrl, 4 Alt), -1 when the layout lacks it.
fn stroke(ch: char, scan: impl Fn(u16) -> i16, caps_lock: bool, shift_held: bool, mods_held: bool) -> Stroke {
    let mut units = [0u16; 2];
    let units = ch.encode_utf16(&mut units);
    // A held Ctrl, Alt or Win would turn a key into a shortcut.
    if units.len() != 1 || mods_held {
        return Stroke::Unicode;
    }
    let r = scan(units[0]);
    if r == -1 {
        return Stroke::Unicode;
    }
    let vk = (r as u16) & 0xFF;
    let mods = ((r as u16) >> 8) & 0xFF;
    // AltGr characters (Ctrl+Alt) go out as Unicode.
    if vk == 0 || mods & !1 != 0 {
        return Stroke::Unicode;
    }
    let mut shift = mods & 1 != 0;
    // Caps Lock flips the case of letters.
    if caps_lock && (0x41..=0x5A).contains(&vk) {
        shift = !shift;
    }
    // A Shift a mapping holds can't be let go for a lowercase letter.
    if shift_held && !shift {
        return Stroke::Unicode;
    }
    Stroke::Key { vk, shift: shift && !shift_held }
}

fn unicode_input(unit: u16, down: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: KEYEVENTF_UNICODE | if down { 0 } else { KEYEVENTF_KEYUP },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Type `text` into the focused window, on its own thread so input keeps
/// flowing. Characters the keyboard layout has go out as their keys, which
/// games that read raw keys also see; the rest as Unicode characters. Each
/// key is held a moment, since games that poll the keyboard once a frame
/// miss a press and release sent together. One text types at a time.
pub fn type_text(text: &str) {
    if text.is_empty() {
        return;
    }
    let text = text.to_string();
    std::thread::spawn(move || {
        static TYPING: Mutex<()> = Mutex::new(());
        let _one = TYPING.lock();
        let gap = std::time::Duration::from_millis(15);
        let step = |i: INPUT| {
            send(&[i]);
            std::thread::sleep(gap);
        };
        for ch in text.chars() {
            let (shift_held, mods_held) = {
                let h = held().lock();
                (
                    h.contains_key(&0xA0) || h.contains_key(&0xA1),
                    [0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C].iter().any(|k| h.contains_key(k)),
                )
            };
            let caps_lock = unsafe { GetKeyState(VK_CAPITAL as i32) } & 1 != 0;
            match stroke(ch, |u| unsafe { VkKeyScanW(u) }, caps_lock, shift_held, mods_held) {
                Stroke::Key { vk, shift } => {
                    if shift {
                        step(key_input(0xA0, true));
                    }
                    step(key_input(vk, true));
                    step(key_input(vk, false));
                    if shift {
                        step(key_input(0xA0, false));
                    }
                }
                Stroke::Unicode => {
                    let mut buf = [0u16; 2];
                    let units = ch.encode_utf16(&mut buf);
                    for u in units.iter() {
                        send(&[unicode_input(*u, true)]);
                    }
                    std::thread::sleep(gap);
                    for u in units.iter() {
                        send(&[unicode_input(*u, false)]);
                    }
                    std::thread::sleep(gap);
                }
            }
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    /// US layout for the few characters the tests type.
    fn us(u: u16) -> i16 {
        match char::from_u32(u as u32).unwrap() {
            c @ 'a'..='z' => c.to_ascii_uppercase() as i16,
            c @ 'A'..='Z' => 0x100 | c as i16,
            c @ '0'..='9' => c as i16,
            '!' => 0x100 | 0x31,
            _ => -1,
        }
    }

    fn flags(i: &INPUT) -> u32 {
        unsafe { i.Anonymous.ki.dwFlags }
    }

    #[test]
    fn numpad_keys_stay_numpad_keys() {
        // Numpad 1 goes as its virtual key, not scan code 0x4F (End with
        // Num Lock off); End itself still goes as an extended scan code.
        let n1 = key_input(0x61, true);
        assert_eq!(flags(&n1) & KEYEVENTF_SCANCODE, 0);
        assert_eq!(unsafe { n1.Anonymous.ki.wVk }, 0x61);
        let end = key_input(0x23, true);
        assert_ne!(flags(&end) & KEYEVENTF_SCANCODE, 0);
        assert_ne!(flags(&end) & KEYEVENTF_EXTENDEDKEY, 0);
        // Letters keep their scan codes.
        assert_ne!(flags(&key_input(0x41, true)) & KEYEVENTF_SCANCODE, 0);
    }

    #[test]
    fn text_types_layout_keys_with_shift() {
        let plan: Vec<Stroke> = "Jg2!".chars().map(|c| stroke(c, us, false, false, false)).collect();
        assert_eq!(
            plan,
            [
                Stroke::Key { vk: 0x4A, shift: true },
                Stroke::Key { vk: 0x47, shift: false },
                Stroke::Key { vk: 0x32, shift: false },
                Stroke::Key { vk: 0x31, shift: true },
            ]
        );
    }

    #[test]
    fn text_follows_caps_lock_and_held_keys() {
        // Caps Lock flips letters only.
        assert_eq!(stroke('J', us, true, false, false), Stroke::Key { vk: 0x4A, shift: false });
        assert_eq!(stroke('g', us, true, false, false), Stroke::Key { vk: 0x47, shift: true });
        assert_eq!(stroke('2', us, true, false, false), Stroke::Key { vk: 0x32, shift: false });
        // A Shift a mapping holds: uppercase rides on it, lowercase can't.
        assert_eq!(stroke('J', us, false, true, false), Stroke::Key { vk: 0x4A, shift: false });
        assert_eq!(stroke('g', us, false, true, false), Stroke::Unicode);
        // A held Ctrl/Alt/Win, or a character the layout lacks.
        assert_eq!(stroke('g', us, false, false, true), Stroke::Unicode);
        assert_eq!(stroke('é', us, false, false, false), Stroke::Unicode);
        assert_eq!(stroke('😀', us, false, false, false), Stroke::Unicode);
        // AltGr characters.
        assert_eq!(stroke('@', |_| 0x600 | 0x51, false, false, false), Stroke::Unicode);
    }
}
