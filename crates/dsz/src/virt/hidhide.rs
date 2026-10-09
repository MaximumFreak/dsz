//! HidHide control through `\\.\HidHide` (the driver's public IOCTLs, as
//! `Nefarius.Drivers.HidHide` uses them). While a virtual pad is up, the
//! physical controller is added to the block list and cloaking turns on, so
//! games and Steam bind the virtual device only. This app whitelists itself
//! first. Every change is recorded and undone when the virtual pad goes
//! away, and leftovers from a crash are undone at the next start.

use std::io;
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;

use super::win::{ctl_code, dos_device_path, ioctl, wide, Handle};

const DEVICE_TYPE: u32 = 32769;
const METHOD_BUFFERED: u32 = 0;
const FILE_READ_DATA: u32 = 1;
const fn code(f: u32) -> u32 {
    ctl_code(DEVICE_TYPE, f, METHOD_BUFFERED, FILE_READ_DATA)
}
const GET_WHITELIST: u32 = code(2048);
const SET_WHITELIST: u32 = code(2049);
const GET_BLACKLIST: u32 = code(2050);
const SET_BLACKLIST: u32 = code(2051);
const GET_ACTIVE: u32 = code(2052);
const SET_ACTIVE: u32 = code(2053);
const GET_INVERSE: u32 = code(2054);

fn open() -> io::Result<Handle> {
    let w = wide(r"\\.\HidHide");
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    };
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle::from_raw(h))
}

/// Is the HidHide driver installed and reachable?
pub fn available() -> bool {
    open().is_ok()
}

fn get_list(h: &Handle, code: u32) -> io::Result<Vec<String>> {
    let need = ioctl(h.raw(), code, &[], &mut [])? as usize;
    if need == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; need.min(65536)];
    let n = ioctl(h.raw(), code, &[], &mut buf)? as usize;
    let w: Vec<u16> = buf[..n.min(buf.len())]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(String::from_utf16_lossy(&w)
        .split('\0')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .collect())
}

fn set_list(h: &Handle, code: u32, items: &[String]) -> io::Result<()> {
    let mut w: Vec<u16> = Vec::new();
    for s in items.iter().filter(|s| !s.trim().is_empty()) {
        w.extend(s.encode_utf16());
        w.push(0);
    }
    w.push(0);
    let bytes: Vec<u8> = w.iter().flat_map(|c| c.to_le_bytes()).collect();
    ioctl(h.raw(), code, &bytes, &mut [])?;
    Ok(())
}

fn get_bool(h: &Handle, code: u32) -> io::Result<bool> {
    let mut b = [0u8; 1];
    ioctl(h.raw(), code, &[], &mut b)?;
    Ok(b[0] != 0)
}

fn set_bool(h: &Handle, code: u32, v: bool) -> io::Result<()> {
    ioctl(h.raw(), code, &[v as u8], &mut [])?;
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub active: bool,
    pub inverse: bool,
    pub apps: Vec<String>,
    pub devices: Vec<String>,
}

pub fn snapshot() -> io::Result<Snapshot> {
    let h = open()?;
    Ok(Snapshot {
        active: get_bool(&h, GET_ACTIVE)?,
        inverse: get_bool(&h, GET_INVERSE).unwrap_or(false),
        apps: get_list(&h, GET_WHITELIST)?,
        devices: get_list(&h, GET_BLACKLIST)?,
    })
}

/// What this app changed, so it can be put back exactly.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Changes {
    pub devices_added: Vec<String>,
    pub app_added: Option<String>,
    pub activated: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.devices_added.is_empty() && self.app_added.is_none() && !self.activated
    }
}

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Hide `instance_ids` from everything but this app. Returns what changed.
pub fn hide(instance_ids: &[String], exe: &Path) -> io::Result<Changes> {
    let h = open()?;
    let mut ch = Changes::default();

    // Whitelist ourselves first, or a reconnect would lose the controller.
    if let Some(dos) = dos_device_path(&exe.display().to_string()) {
        let mut apps = get_list(&h, GET_WHITELIST)?;
        if !apps.iter().any(|a| eq(a, &dos)) {
            apps.push(dos.clone());
            set_list(&h, SET_WHITELIST, &apps)?;
            ch.app_added = Some(dos);
        }
    }
    let mut devs = get_list(&h, GET_BLACKLIST)?;
    for id in instance_ids {
        if !devs.iter().any(|d| eq(d, id)) {
            devs.push(id.clone());
            ch.devices_added.push(id.clone());
        }
    }
    if !ch.devices_added.is_empty() {
        set_list(&h, SET_BLACKLIST, &devs)?;
    }
    if !get_bool(&h, GET_ACTIVE)? {
        set_bool(&h, SET_ACTIVE, true)?;
        ch.activated = true;
    }
    Ok(ch)
}

/// Undo [`hide`]. Entries that were already there before stay.
pub fn restore(ch: &Changes) -> io::Result<()> {
    if ch.is_empty() {
        return Ok(());
    }
    let h = open()?;
    if !ch.devices_added.is_empty() {
        let devs: Vec<String> = get_list(&h, GET_BLACKLIST)?
            .into_iter()
            .filter(|d| !ch.devices_added.iter().any(|x| eq(x, d)))
            .collect();
        set_list(&h, SET_BLACKLIST, &devs)?;
    }
    if ch.activated {
        set_bool(&h, SET_ACTIVE, false)?;
    }
    // The whitelist entry stays: it is harmless and keeps a hidden pad
    // visible to us if another tool turns cloaking on.
    Ok(())
}
