//! Keep the default playback and recording devices where the user wants
//! them while virtual DualSenses come and go (see [`Watcher`]).
//!
//! Core Audio (`IMMDeviceEnumerator`) reads the defaults; the long-standing
//! `IPolicyConfig` interface, which the Sound control panel itself uses,
//! sets them back. Both are called through raw vtables.

use std::collections::HashMap;
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

/// Ids of the active endpoints of one data flow.
fn active_endpoints(en: &Com, flow: i32) -> Vec<String> {
    const DEVICE_STATE_ACTIVE: u32 = 1;
    let mut out = Vec::new();
    unsafe {
        let enum_endpoints: unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut c_void) -> i32 =
            en.method(3);
        let mut coll = null_mut();
        if enum_endpoints(en.0, flow, DEVICE_STATE_ACTIVE, &mut coll) < 0 || coll.is_null() {
            return out;
        }
        let coll = Com(coll);
        let count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 = coll.method(3);
        let item: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 = coll.method(4);
        let mut n = 0u32;
        if count(coll.0, &mut n) < 0 {
            return out;
        }
        for i in 0..n {
            let mut dev = null_mut();
            if item(coll.0, i, &mut dev) < 0 || dev.is_null() {
                continue;
            }
            let dev = Com(dev);
            let get_id: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> i32 = dev.method(5);
            let mut id = null_mut();
            if get_id(dev.0, &mut id) >= 0 && !id.is_null() {
                out.push(take_wide(id));
            }
        }
    }
    out
}

fn is_ours(en: &Com, endpoint: &str) -> bool {
    super::usbip::is_ours_instance(&endpoint_node(en, endpoint))
}

/// Probe helpers: the defaults per (flow, role) slot with whether each is
/// ours, our active endpoints per flow, and setting a default.
pub mod probe {
    use super::*;

    pub fn snapshot() -> Vec<(Option<String>, bool)> {
        let Some(en) = create(&CLSID_MM_DEVICE_ENUMERATOR, &IID_IMM_DEVICE_ENUMERATOR) else {
            return vec![(None, false); 6];
        };
        SLOTS
            .iter()
            .map(|&(f, r)| {
                let id = default_id(&en, f, r);
                let ours = id.as_deref().map(|i| is_ours(&en, i)).unwrap_or(false);
                (id, ours)
            })
            .collect()
    }

    pub fn ours(flow: i32) -> Vec<String> {
        let Some(en) = create(&CLSID_MM_DEVICE_ENUMERATOR, &IID_IMM_DEVICE_ENUMERATOR) else {
            return Vec::new();
        };
        active_endpoints(&en, flow).into_iter().filter(|id| is_ours(&en, id)).collect()
    }

    pub fn set(id: &str, role: i32) -> bool {
        set_default(id, role)
    }
}

/// How soon after a virtual DualSense's endpoint shows up a switch to it
/// counts as Windows promoting the new device. Measured: Windows moves all
/// six slots within 0.1 s of the endpoint going active.
const ARRIVAL_WINDOW: Duration = Duration::from_secs(3);
/// A new endpoint whose device node isn't readable yet looks like someone
/// else's; it isn't taken for anyone's until it has been around this long.
const SETTLE: Duration = Duration::from_secs(2);

const SLOT_NAMES: [&str; 6] = [
    "output",
    "output (multimedia)",
    "output (communications)",
    "microphone",
    "microphone (multimedia)",
    "microphone (communications)",
];

/// Keeps the default audio devices where the user wants them while virtual
/// DualSenses come and go.
///
/// Windows makes a newly attached audio device the default for every slot
/// (render and capture, each of the three roles), which would send system
/// sound to the controller and make the default microphone silent. It also
/// remembers, per slot, a device the user picked, and makes it the default
/// again when it reappears. Both happen right as the endpoint appears, so
/// timing alone can't tell them apart: the watcher remembers which slots
/// the user gave to a virtual DualSense (`prefs`, kept in the settings).
///
/// Per slot: a switch to a virtual DualSense right as it appears is put
/// back, unless the user chose it for that slot; any other switch to one,
/// while the previous default still exists, is the user's choice and is
/// remembered; switching away from one while it still exists forgets it. A
/// switch made because the previous default disappeared is Windows'
/// fallback and teaches nothing.
pub struct Watcher {
    /// Slots the user set to a virtual DualSense.
    pub prefs: [bool; 6],
    /// The default seen last time, per slot.
    last: [Option<String>; 6],
    /// The latest default per slot that wasn't a virtual DualSense.
    home: [Option<String>; 6],
    /// The arriving endpoint each slot was last handled for.
    handled: [Option<String>; 6],
    /// When each endpoint was first seen active.
    seen: HashMap<String, Instant>,
    /// Endpoints known to be ours or not.
    known: HashMap<String, bool>,
    started: bool,
}

impl Watcher {
    pub fn new(prefs: [bool; 6]) -> Self {
        Watcher {
            prefs,
            last: Default::default(),
            home: Default::default(),
            handled: Default::default(),
            seen: HashMap::new(),
            known: HashMap::new(),
            started: false,
        }
    }

    /// Whether an endpoint appeared recently (worth looking often).
    pub fn busy(&self) -> bool {
        let now = Instant::now();
        self.seen
            .values()
            .any(|t| now.duration_since(*t) < ARRIVAL_WINDOW + SETTLE)
    }

    /// Look once and act. True when `prefs` changed.
    pub fn tick(&mut self) -> bool {
        let Some(en) = create(&CLSID_MM_DEVICE_ENUMERATOR, &IID_IMM_DEVICE_ENUMERATOR) else {
            return false;
        };
        let now = Instant::now();
        // Endpoints present before the first look count as old.
        let old = now.checked_sub(ARRIVAL_WINDOW + SETTLE).unwrap_or(now);
        let mut active: [HashMap<String, bool>; 2] = Default::default();
        // Too new to tell whose: a not-ours answer may just be early.
        let mut unsure: Vec<String> = Vec::new();
        for (f, map) in active.iter_mut().enumerate() {
            for id in active_endpoints(&en, f as i32) {
                let first = *self
                    .seen
                    .entry(id.clone())
                    .or_insert(if self.started { now } else { old });
                let ours = match self.known.get(&id) {
                    Some(&o) => o,
                    None => {
                        let o = is_ours(&en, &id);
                        if o || now.duration_since(first) >= SETTLE {
                            self.known.insert(id.clone(), o);
                        } else {
                            unsure.push(id.clone());
                        }
                        o
                    }
                };
                map.insert(id, ours);
            }
        }
        self.seen
            .retain(|id, _| active[0].contains_key(id) || active[1].contains_key(id));
        let seen = &self.seen;
        self.known.retain(|id, _| seen.contains_key(id));
        let fresh = |id: &str| {
            seen.get(id)
                .is_some_and(|t| now.duration_since(*t) < ARRIVAL_WINDOW)
        };

        let mut changed = false;
        for (i, &(flow, role)) in SLOTS.iter().enumerate() {
            let here = &active[flow as usize];
            let Some(cur) = default_id(&en, flow, role) else {
                continue;
            };
            if !self.started {
                if here.get(&cur) == Some(&false) {
                    self.home[i] = Some(cur.clone());
                }
                self.last[i] = Some(cur);
                continue;
            }
            // Not listed yet, or too new to tell whose: look again later.
            let Some(&cur_ours) = here.get(&cur) else {
                continue;
            };
            if !cur_ours && unsure.contains(&cur) {
                continue;
            }
            let moved = self.last[i].as_deref() != Some(cur.as_str());
            let home_alive = self.home[i].as_ref().is_some_and(|h| here.contains_key(h));
            let mut now_default = cur.clone();
            if cur_ours {
                let first_time = self.handled[i].as_deref() != Some(cur.as_str());
                if fresh(&cur) && !self.prefs[i] && first_time {
                    // Windows promoted the new device.
                    self.handled[i] = Some(cur.clone());
                    if let Some(h) = self.home[i].clone().filter(|_| home_alive) {
                        if set_default(&h, role) {
                            log::info!(
                                "kept your default {} (Windows had switched it to the virtual DualSense)",
                                SLOT_NAMES[i]
                            );
                            now_default = h;
                        }
                    }
                } else if moved && home_alive && !self.prefs[i] {
                    // Someone else picked it: the user.
                    self.prefs[i] = true;
                    changed = true;
                    log::info!(
                        "you chose the virtual DualSense as the default {}; DSZ will keep it there",
                        SLOT_NAMES[i]
                    );
                }
            } else {
                let left_ours = self.last[i]
                    .as_ref()
                    .is_some_and(|l| here.get(l) == Some(&true));
                if moved && left_ours && self.prefs[i] {
                    self.prefs[i] = false;
                    changed = true;
                    log::info!(
                        "you moved the default {} off the virtual DualSense; DSZ won't put it back",
                        SLOT_NAMES[i]
                    );
                }
                self.home[i] = Some(cur.clone());
                // A virtual DualSense just appeared and the user had chosen
                // one for this slot: put it back if Windows didn't.
                if self.prefs[i] {
                    let arriving = here
                        .iter()
                        .find(|(id, &o)| {
                            o && fresh(id) && self.handled[i].as_deref() != Some(id.as_str())
                        })
                        .map(|(id, _)| id.clone());
                    if let Some(id) = arriving {
                        self.handled[i] = Some(id.clone());
                        if set_default(&id, role) {
                            log::info!(
                                "made the virtual DualSense the default {} again, as you chose",
                                SLOT_NAMES[i]
                            );
                            now_default = id;
                        }
                    }
                }
            }
            self.last[i] = Some(now_default);
        }
        self.started = true;
        changed
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
