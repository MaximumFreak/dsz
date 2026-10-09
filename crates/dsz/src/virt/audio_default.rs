//! Keep the user's default playback and recording devices when a virtual
//! DualSense appears. Windows promotes a newly attached USB audio device to
//! default (a wired DualSense does the same), which would send system sound
//! to the controller and make the default microphone silent.
//!
//! Core Audio (`IMMDeviceEnumerator`) reads the defaults; the long-standing
//! `IPolicyConfig` interface, which the Sound control panel itself uses,
//! sets them back. Both are called through raw vtables.

use std::ffi::c_void;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

use windows_sys::core::GUID;
use windows_sys::Win32::System::Com::*;

use super::win;

const CLSID_MM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);
const IID_IMM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xA95664D2_9614_4F35_A746_DE8DB63617E6);
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870AF99C_171D_4F9E_AF0D_E63DF40C2BC9);
const IID_IPOLICY_CONFIG: GUID = GUID::from_u128(0xF8679F50_850A_41CF_9C72_430F290290C8);
/// `PKEY_Device_InstanceId`.
const PKEY_INSTANCE_ID: GUID = GUID::from_u128(0x78C34FC8_104A_4ACA_9EA4_524D52996E57);

/// (data flow, role): render/capture × console/multimedia/communications.
const SLOTS: [(i32, i32); 6] = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)];

/// How long after attaching we treat a switch to the virtual device as
/// Windows' doing rather than the user's.
pub const GUARD_FOR: Duration = Duration::from_secs(15);

/// An owned COM interface pointer.
struct Com(*mut c_void);

impl Com {
    unsafe fn method<T: Copy>(&self, index: usize) -> T {
        let vtbl = *(self.0 as *const *const usize);
        std::mem::transmute_copy(&*vtbl.add(index))
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            let release: unsafe extern "system" fn(*mut c_void) -> u32 = self.method(2);
            release(self.0);
        }
    }
}

fn create(clsid: &GUID, iid: &GUID) -> Option<Com> {
    unsafe {
        // Fine if COM is already initialized on this thread in either model.
        CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32);
        let mut p = null_mut();
        let hr = CoCreateInstance(clsid, null_mut(), CLSCTX_ALL, iid, &mut p);
        (hr >= 0 && !p.is_null()).then(|| Com(p))
    }
}

unsafe fn take_wide(p: *mut u16) -> String {
    let mut n = 0;
    while *p.add(n) != 0 {
        n += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
    CoTaskMemFree(p as *const c_void);
    s
}

fn default_id(en: &Com, flow: i32, role: i32) -> Option<String> {
    unsafe {
        let get: unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> i32 =
            en.method(4);
        let mut dev = null_mut();
        if get(en.0, flow, role, &mut dev) < 0 || dev.is_null() {
            return None;
        }
        let dev = Com(dev);
        let get_id: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> i32 = dev.method(5);
        let mut id = null_mut();
        (get_id(dev.0, &mut id) >= 0 && !id.is_null()).then(|| take_wide(id))
    }
}

/// PnP instance id behind an audio endpoint (`SWD\MMDEVAPI\...`).
fn endpoint_instance(en: &Com, endpoint: &str) -> Option<String> {
    #[repr(C)]
    struct PropertyKey {
        fmtid: GUID,
        pid: u32,
    }
    #[repr(C)]
    struct PropVariant {
        vt: u16,
        _r: [u16; 3],
        ptr: *mut u16,
        _pad: usize,
    }
    unsafe {
        let get_device: unsafe extern "system" fn(
            *mut c_void,
            *const u16,
            *mut *mut c_void,
        ) -> i32 = en.method(5);
        let w = win::wide(endpoint);
        let mut dev = null_mut();
        if get_device(en.0, w.as_ptr(), &mut dev) < 0 || dev.is_null() {
            return None;
        }
        let dev = Com(dev);
        let open: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
            dev.method(4);
        let mut store = null_mut();
        if open(dev.0, 0, &mut store) < 0 || store.is_null() {
            return None;
        }
        let store = Com(store);
        let get_value: unsafe extern "system" fn(
            *mut c_void,
            *const PropertyKey,
            *mut PropVariant,
        ) -> i32 = store.method(5);
        let key = PropertyKey {
            fmtid: PKEY_INSTANCE_ID,
            pid: 256,
        };
        let mut v = PropVariant {
            vt: 0,
            _r: [0; 3],
            ptr: null_mut(),
            _pad: 0,
        };
        if get_value(store.0, &key, &mut v) < 0 {
            return None;
        }
        const VT_LPWSTR: u16 = 31;
        (v.vt == VT_LPWSTR && !v.ptr.is_null()).then(|| take_wide(v.ptr))
    }
}

/// Instance id of an endpoint's device node; the property when Windows
/// has it, else the `SWD\MMDEVAPI\<endpoint id>` it is always named.
fn endpoint_node(en: &Com, endpoint: &str) -> String {
    endpoint_instance(en, endpoint).unwrap_or_else(|| format!(r"SWD\MMDEVAPI\{endpoint}"))
}

fn set_default(id: &str, role: i32) -> bool {
    let Some(pc) = create(&CLSID_POLICY_CONFIG, &IID_IPOLICY_CONFIG) else {
        return false;
    };
    unsafe {
        let set: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32 = pc.method(13);
        let w = win::wide(id);
        set(pc.0, w.as_ptr(), role) >= 0
    }
}

/// The defaults before a virtual DualSense was attached.
pub struct Guard {
    before: [Option<String>; 6],
    since: Instant,
    restored: bool,
}

impl Guard {
    pub fn take() -> Option<Guard> {
        let en = create(&CLSID_MM_DEVICE_ENUMERATOR, &IID_IMM_DEVICE_ENUMERATOR)?;
        let before = SLOTS.map(|(f, r)| default_id(&en, f, r));
        Some(Guard {
            before,
            since: Instant::now(),
            restored: false,
        })
    }

    pub fn expired(&self) -> bool {
        self.since.elapsed() > GUARD_FOR
    }

    /// Put back any default that moved to one of our virtual devices.
    /// Returns true when something was restored.
    pub fn check(&mut self) -> bool {
        let Some(en) = create(&CLSID_MM_DEVICE_ENUMERATOR, &IID_IMM_DEVICE_ENUMERATOR) else {
            return false;
        };
        let mut fixed = false;
        for (i, (flow, role)) in SLOTS.iter().enumerate() {
            let Some(before) = &self.before[i] else {
                continue;
            };
            let Some(now) = default_id(&en, *flow, *role) else {
                continue;
            };
            if &now == before {
                continue;
            }
            let ours = super::usbip::is_ours_instance(&endpoint_node(&en, &now));
            if ours && set_default(before, *role) {
                fixed = true;
            }
        }
        if fixed && !self.restored {
            log::info!("kept your default audio devices (Windows had switched them to the virtual DualSense)");
            self.restored = true;
        }
        fixed
    }
}

/// Print the current defaults and whether each is ours (diagnostics).
pub fn describe() {
    let Some(en) = create(&CLSID_MM_DEVICE_ENUMERATOR, &IID_IMM_DEVICE_ENUMERATOR) else {
        println!("no device enumerator");
        return;
    };
    for (flow, role) in SLOTS {
        let id = default_id(&en, flow, role);
        let inst = id.as_deref().map(|i| endpoint_node(&en, i));
        let mut chain = Vec::new();
        let mut cur = inst.clone();
        while let Some(c) = cur {
            chain.push(c.clone());
            if chain.len() > 4 {
                break;
            }
            cur = win::parent_instance_id(&c);
        }
        println!(
            "flow {flow} role {role}: {id:?}
   ours {} chain {chain:?}",
            inst.as_deref()
                .map(super::usbip::is_ours_instance)
                .unwrap_or(false)
        );
    }
}
