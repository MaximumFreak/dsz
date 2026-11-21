//! Minimal direct WASAPI loopback capture and shared-mode playback via raw
//! Core Audio COM vtables, replacing `cpal` (and its duplicate `windows` crate).

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Com::*;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};

const CLSID_MM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);
const IID_IMM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xA95664D2_9614_4F35_A746_DE8DB63617E6);
const IID_I_AUDIO_CLIENT: GUID = GUID::from_u128(0x1CB9AD4C_DBFA_4C32_B178_C2F568A703B2);
const IID_I_AUDIO_RENDER_CLIENT: GUID = GUID::from_u128(0xF294ACFC_3146_4483_A7BF_ADDCA7C260E2);
const IID_I_AUDIO_CAPTURE_CLIENT: GUID = GUID::from_u128(0xC8ADBD64_E71E_48A0_A4DE_185C395CD317);

const PKEY_FRIENDLY_NAME: GUID = GUID::from_u128(0xA45C254E_DF1C_4EFD_8020_67D146A850E0);
const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00AA00389B71);
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
    GUID::from_u128(0x00000003_0000_0010_8000_00AA00389B71);

const E_RENDER: i32 = 0;
const E_CAPTURE: i32 = 1;
const E_CONSOLE: i32 = 0;
const DEVICE_STATE_ACTIVE: u32 = 1;
const VT_LPWSTR: u16 = 31;

const AUDCLNT_SHAREMODE_SHARED: i32 = 0;
const AUDCLNT_STREAMFLAGS_LOOPBACK: u32 = 0x0002_0000;
const AUDCLNT_STREAMFLAGS_EVENTCALLBACK: u32 = 0x0004_0000;
const AUDCLNT_BUFFERFLAGS_SILENT: u32 = 0x0000_0002;

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

#[repr(C, packed)]
struct WaveFormatEx {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    cb_size: u16,
}

#[repr(C, packed)]
struct WaveFormatExtensible {
    format: WaveFormatEx,
    valid_bits_per_sample: u16,
    channel_mask: u32,
    sub_format: GUID,
}

#[derive(Clone, Copy, Debug)]
enum SampleFmt {
    F32,
    I16,
    I32,
}

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

struct CoTaskMem(*mut c_void);

impl Drop for CoTaskMem {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(self.0) };
        }
    }
}

struct EventHandle(HANDLE);

impl EventHandle {
    fn new() -> Result<Self, String> {
        let h = unsafe { CreateEventW(null(), 0, 0, null()) };
        if h.is_null() {
            Err("CreateEventW failed".into())
        } else {
            Ok(EventHandle(h))
        }
    }
}

impl Drop for EventHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn create_enumerator() -> Option<Com> {
    unsafe {
        CoInitializeEx(null(), COINIT_MULTITHREADED as u32);
        let mut p = null_mut();
        let hr = CoCreateInstance(
            &CLSID_MM_DEVICE_ENUMERATOR,
            null_mut(),
            CLSCTX_ALL,
            &IID_IMM_DEVICE_ENUMERATOR,
            &mut p,
        );
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

fn friendly_name(dev: &Com) -> Option<String> {
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
            fmtid: PKEY_FRIENDLY_NAME,
            pid: 14,
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
        (v.vt == VT_LPWSTR && !v.ptr.is_null()).then(|| take_wide(v.ptr))
    }
}

fn enum_devices(en: &Com, flow: i32) -> Vec<(String, Com)> {
    let mut out = Vec::new();
    unsafe {
        let enum_ep: unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut c_void) -> i32 =
            en.method(3);
        let mut coll = null_mut();
        if enum_ep(en.0, flow, DEVICE_STATE_ACTIVE, &mut coll) < 0 || coll.is_null() {
            return out;
        }
        let coll = Com(coll);
        let count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 = coll.method(3);
        let item: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32 =
            coll.method(4);
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
            if let Some(name) = friendly_name(&dev) {
                out.push((name, dev));
            }
        }
    }
    out
}

fn default_device(en: &Com, flow: i32) -> Option<(String, Com)> {
    unsafe {
        let get: unsafe extern "system" fn(*mut c_void, i32, i32, *mut *mut c_void) -> i32 =
            en.method(4);
        let mut dev = null_mut();
        if get(en.0, flow, E_CONSOLE, &mut dev) < 0 || dev.is_null() {
            return None;
        }
        let dev = Com(dev);
        let name = friendly_name(&dev).unwrap_or_default();
        Some((name, dev))
    }
}

fn same_guid(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

unsafe fn parse_mix_format(ptr: *const WaveFormatEx) -> Result<(u32, u16, SampleFmt), String> {
    let wf = std::ptr::read_unaligned(ptr);
    let rate = wf.samples_per_sec;
    let channels = wf.channels.max(1);
    let bits = wf.bits_per_sample;
    let fmt = match wf.format_tag {
        WAVE_FORMAT_IEEE_FLOAT if bits == 32 => SampleFmt::F32,
        WAVE_FORMAT_PCM if bits == 16 => SampleFmt::I16,
        WAVE_FORMAT_PCM if bits == 32 => SampleFmt::I32,
        WAVE_FORMAT_EXTENSIBLE if wf.cb_size >= 22 => {
            let ext = std::ptr::read_unaligned(ptr as *const WaveFormatExtensible);
            let sub = ext.sub_format;
            if same_guid(&sub, &KSDATAFORMAT_SUBTYPE_IEEE_FLOAT) && bits == 32 {
                SampleFmt::F32
            } else if same_guid(&sub, &KSDATAFORMAT_SUBTYPE_PCM) && bits == 16 {
                SampleFmt::I16
            } else if same_guid(&sub, &KSDATAFORMAT_SUBTYPE_PCM) && bits == 32 {
                SampleFmt::I32
            } else {
                return Err(format!("unsupported WASAPI extensible format ({bits}-bit)"));
            }
        }
        tag => return Err(format!("unsupported WASAPI format tag {tag:#x} ({bits}-bit)")),
    };
    Ok((rate, channels, fmt))
}

unsafe fn activate_audio_client(dev: &Com) -> Result<Com, String> {
    let activate: unsafe extern "system" fn(
        *mut c_void,
        *const GUID,
        u32,
        *mut c_void,
        *mut *mut c_void,
    ) -> i32 = dev.method(3);
    let mut client = null_mut();
    let hr = activate(dev.0, &IID_I_AUDIO_CLIENT, CLSCTX_ALL, null_mut(), &mut client);
    if hr < 0 || client.is_null() {
        return Err(format!("IAudioClient::Activate failed ({hr:#010x})"));
    }
    Ok(Com(client))
}

unsafe fn get_mix_format(client: &Com) -> Result<(*mut WaveFormatEx, CoTaskMem), String> {
    let get_fmt: unsafe extern "system" fn(*mut c_void, *mut *mut WaveFormatEx) -> i32 =
        client.method(8);
    let mut fmt = null_mut();
    let hr = get_fmt(client.0, &mut fmt);
    if hr < 0 || fmt.is_null() {
        return Err(format!("IAudioClient::GetMixFormat failed ({hr:#010x})"));
    }
    Ok((fmt, CoTaskMem(fmt as *mut c_void)))
}

unsafe fn get_service(client: &Com, iid: &GUID) -> Result<Com, String> {
    let get_svc: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32 =
        client.method(14);
    let mut svc = null_mut();
    let hr = get_svc(client.0, iid, &mut svc);
    if hr < 0 || svc.is_null() {
        return Err(format!("IAudioClient::GetService failed ({hr:#010x})"));
    }
    Ok(Com(svc))
}

/// Active playback device friendly names.
pub fn output_device_names() -> Vec<String> {
    let Some(en) = create_enumerator() else {
        return Vec::new();
    };
    enum_devices(&en, E_RENDER)
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// Friendly name of the default playback (`capture = false`) or recording (`capture = true`) device.
pub fn default_device_name(capture: bool) -> String {
    let Some(en) = create_enumerator() else {
        return String::new();
    };
    default_device(&en, if capture { E_CAPTURE } else { E_RENDER })
        .map(|(n, _)| n)
        .unwrap_or_default()
}

/// Running WASAPI worker thread; dropping stops and joins it cleanly.
pub struct Stream {
    stop: Arc<AtomicBool>,
    wake_event: usize,
    join: Option<JoinHandle<()>>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if self.wake_event != 0 {
            unsafe {
                SetEvent(self.wake_event as HANDLE);
            }
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub struct StreamInfo {
    pub name: String,
    pub rate: u32,
    pub channels: u16,
}

/// Open a shared-mode WASAPI loopback capture stream on `want_name`
/// (empty = default render endpoint).
pub fn open_loopback(
    want_name: &str,
    mut on_frames: impl FnMut(&[[f32; 2]], u32) + Send + 'static,
    on_error: impl Fn(String) + Send + 'static,
) -> Result<(StreamInfo, Stream), String> {
    let want = want_name.to_string();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<StreamInfo, String>>(1);

    let join = std::thread::Builder::new()
        .name("wasapi-loopback".into())
        .spawn(move || {
            let init = (|| -> Result<(StreamInfo, Com, Com, SampleFmt), String> {
                let en = create_enumerator().ok_or("failed to create IMMDeviceEnumerator")?;
                let (name, dev) = if want.is_empty() {
                    default_device(&en, E_RENDER)
                } else {
                    enum_devices(&en, E_RENDER)
                        .into_iter()
                        .find(|(n, _)| n == &want)
                        .or_else(|| default_device(&en, E_RENDER))
                }
                .ok_or("no audio output device")?;

                unsafe {
                    let client = activate_audio_client(&dev)?;
                    let (fmt_ptr, _fmt_guard) = get_mix_format(&client)?;
                    let (rate, channels, sample_fmt) = parse_mix_format(fmt_ptr)?;

                    let init_fn: unsafe extern "system" fn(
                        *mut c_void,
                        i32,
                        u32,
                        i64,
                        i64,
                        *const WaveFormatEx,
                        *const GUID,
                    ) -> i32 = client.method(3);
                    // 20 ms shared buffer in 100 ns units.
                    let hr = init_fn(
                        client.0,
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_LOOPBACK,
                        200_000,
                        0,
                        fmt_ptr,
                        null(),
                    );
                    if hr < 0 {
                        return Err(format!("IAudioClient::Initialize(loopback) failed ({hr:#010x})"));
                    }
                    let capture = get_service(&client, &IID_I_AUDIO_CAPTURE_CLIENT)?;
                    let start: unsafe extern "system" fn(*mut c_void) -> i32 = client.method(10);
                    let hr = start(client.0);
                    if hr < 0 {
                        return Err(format!("IAudioClient::Start failed ({hr:#010x})"));
                    }
                    Ok((
                        StreamInfo {
                            name,
                            rate,
                            channels,
                        },
                        client,
                        capture,
                        sample_fmt,
                    ))
                }
            })();

            let (info, client, capture, sample_fmt) = match init {
                Ok(v) => {
                    let out = StreamInfo {
                        name: v.0.name.clone(),
                        rate: v.0.rate,
                        channels: v.0.channels,
                    };
                    let _ = tx.send(Ok(out));
                    v
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };

            let ch = info.channels as usize;
            let rate = info.rate;
            let mut scratch: Vec<[f32; 2]> = Vec::with_capacity(4096);

            unsafe {
                let next_packet: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
                    capture.method(5);
                let get_buf: unsafe extern "system" fn(
                    *mut c_void,
                    *mut *mut u8,
                    *mut u32,
                    *mut u32,
                    *mut u64,
                    *mut u64,
                ) -> i32 = capture.method(3);
                let rel_buf: unsafe extern "system" fn(*mut c_void, u32) -> i32 =
                    capture.method(4);

                while !stop_t.load(Ordering::Acquire) {
                    let mut pkt_frames = 0u32;
                    let hr = next_packet(capture.0, &mut pkt_frames);
                    if hr < 0 {
                        on_error(format!("GetNextPacketSize failed ({hr:#010x})"));
                        break;
                    }
                    if pkt_frames == 0 {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    while pkt_frames > 0 && !stop_t.load(Ordering::Acquire) {
                        let mut data = null_mut();
                        let mut num_frames = 0u32;
                        let mut flags = 0u32;
                        let hr = get_buf(
                            capture.0,
                            &mut data,
                            &mut num_frames,
                            &mut flags,
                            null_mut(),
                            null_mut(),
                        );
                        if hr < 0 {
                            on_error(format!("GetBuffer failed ({hr:#010x})"));
                            stop_t.store(true, Ordering::Release);
                            break;
                        }
                        let nf = num_frames as usize;
                        scratch.clear();
                        if nf > 0 {
                            if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
                                scratch.resize(nf, [0.0, 0.0]);
                            } else {
                                scratch.reserve(nf);
                                match sample_fmt {
                                    SampleFmt::F32 => {
                                        let samples = std::slice::from_raw_parts(
                                            data as *const f32,
                                            nf * ch,
                                        );
                                        for f in samples.chunks_exact(ch) {
                                            let r = if ch == 1 { f[0] } else { f[1] };
                                            scratch.push([f[0], r]);
                                        }
                                    }
                                    SampleFmt::I16 => {
                                        let samples = std::slice::from_raw_parts(
                                            data as *const i16,
                                            nf * ch,
                                        );
                                        for f in samples.chunks_exact(ch) {
                                            let l = f[0] as f32 / 32768.0;
                                            let r = if ch == 1 { l } else { f[1] as f32 / 32768.0 };
                                            scratch.push([l, r]);
                                        }
                                    }
                                    SampleFmt::I32 => {
                                        let samples = std::slice::from_raw_parts(
                                            data as *const i32,
                                            nf * ch,
                                        );
                                        for f in samples.chunks_exact(ch) {
                                            let l = f[0] as f32 / 2_147_483_648.0;
                                            let r = if ch == 1 {
                                                l
                                            } else {
                                                f[1] as f32 / 2_147_483_648.0
                                            };
                                            scratch.push([l, r]);
                                        }
                                    }
                                }
                            }
                            on_frames(&scratch, rate);
                        }
                        let hr = rel_buf(capture.0, num_frames);
                        if hr < 0 {
                            on_error(format!("ReleaseBuffer failed ({hr:#010x})"));
                            stop_t.store(true, Ordering::Release);
                            break;
                        }
                        let hr = next_packet(capture.0, &mut pkt_frames);
                        if hr < 0 {
                            on_error(format!("GetNextPacketSize failed ({hr:#010x})"));
                            stop_t.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
                let stop_fn: unsafe extern "system" fn(*mut c_void) -> i32 = client.method(11);
                let _ = stop_fn(client.0);
            }
        })
        .map_err(|e| e.to_string())?;

    match rx.recv() {
        Ok(Ok(info)) => Ok((
            info,
            Stream {
                stop,
                wake_event: 0,
                join: Some(join),
            },
        )),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            Err("loopback capture thread ended unexpectedly".into())
        }
    }
}

pub enum RenderError {
    Channels(String),
    Other(String),
}

/// Open an event-driven shared-mode WASAPI playback stream on the unique
/// active render device whose friendly name matches `match_name`.
pub fn open_render(
    match_name: impl Fn(&str) -> bool + Send + 'static,
    expected_label: String,
    min_channels: u16,
    make_fill: impl FnOnce(u32, usize) -> Box<dyn FnMut(&mut [f32]) + Send> + Send + 'static,
    on_error: impl Fn(String) + Send + 'static,
) -> Result<(StreamInfo, Stream), RenderError> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = stop.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<(StreamInfo, usize), RenderError>>(1);

    let join = std::thread::Builder::new()
        .name("wasapi-render".into())
        .spawn(move || {
            let init = (|| -> Result<
                (StreamInfo, Com, Com, EventHandle, u32, SampleFmt, Box<dyn FnMut(&mut [f32]) + Send>),
                RenderError,
            > {
                let en = create_enumerator()
                    .ok_or_else(|| RenderError::Other("failed to create IMMDeviceEnumerator".into()))?;
                let mut found: Vec<(String, Com)> = enum_devices(&en, E_RENDER)
                    .into_iter()
                    .filter(|(n, _)| match_name(n))
                    .collect();
                if found.len() != 1 {
                    return Err(RenderError::Other(format!(
                        "{} playback devices are named \"{expected_label}\"",
                        found.len()
                    )));
                }
                let (name, dev) = found.remove(0);

                unsafe {
                    let client = activate_audio_client(&dev).map_err(RenderError::Other)?;
                    let (fmt_ptr, _fmt_guard) = get_mix_format(&client).map_err(RenderError::Other)?;
                    let (rate, channels, sample_fmt) =
                        parse_mix_format(fmt_ptr).map_err(RenderError::Other)?;
                    if channels < min_channels {
                        return Err(RenderError::Channels(format!(
                            "\"{name}\" is set to {channels} channels; the actuators need {min_channels} (Sound settings → the controller → Configure → Quadraphonic)"
                        )));
                    }

                    let init_fn: unsafe extern "system" fn(
                        *mut c_void,
                        i32,
                        u32,
                        i64,
                        i64,
                        *const WaveFormatEx,
                        *const GUID,
                    ) -> i32 = client.method(3);
                    let hr = init_fn(
                        client.0,
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                        0,
                        0,
                        fmt_ptr,
                        null(),
                    );
                    if hr < 0 {
                        return Err(RenderError::Other(format!(
                            "IAudioClient::Initialize(render) failed ({hr:#010x})"
                        )));
                    }
                    let ev = EventHandle::new().map_err(RenderError::Other)?;
                    let set_ev: unsafe extern "system" fn(*mut c_void, HANDLE) -> i32 =
                        client.method(13);
                    let hr = set_ev(client.0, ev.0);
                    if hr < 0 {
                        return Err(RenderError::Other(format!(
                            "IAudioClient::SetEventHandle failed ({hr:#010x})"
                        )));
                    }
                    let get_size: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
                        client.method(4);
                    let mut buf_frames = 0u32;
                    let hr = get_size(client.0, &mut buf_frames);
                    if hr < 0 || buf_frames == 0 {
                        return Err(RenderError::Other(format!(
                            "IAudioClient::GetBufferSize failed ({hr:#010x})"
                        )));
                    }
                    let render =
                        get_service(&client, &IID_I_AUDIO_RENDER_CLIENT).map_err(RenderError::Other)?;
                    let fill = make_fill(rate, channels as usize);
                    let start: unsafe extern "system" fn(*mut c_void) -> i32 = client.method(10);
                    let hr = start(client.0);
                    if hr < 0 {
                        return Err(RenderError::Other(format!(
                            "IAudioClient::Start failed ({hr:#010x})"
                        )));
                    }
                    Ok((
                        StreamInfo {
                            name,
                            rate,
                            channels,
                        },
                        client,
                        render,
                        ev,
                        buf_frames,
                        sample_fmt,
                        fill,
                    ))
                }
            })();

            let (info, client, render, ev, buf_frames, sample_fmt, mut fill) = match init {
                Ok(v) => {
                    let out = StreamInfo {
                        name: v.0.name.clone(),
                        rate: v.0.rate,
                        channels: v.0.channels,
                    };
                    let ev_raw = v.3 .0 as usize;
                    let _ = tx.send(Ok((out, ev_raw)));
                    v
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            };

            let ch = info.channels as usize;
            let mut conv_buf: Vec<f32> = Vec::new();

            unsafe {
                let get_pad: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32 =
                    client.method(6);
                let get_buf: unsafe extern "system" fn(*mut c_void, u32, *mut *mut u8) -> i32 =
                    render.method(3);
                let rel_buf: unsafe extern "system" fn(*mut c_void, u32, u32) -> i32 =
                    render.method(4);

                while !stop_t.load(Ordering::Acquire) {
                    let w = WaitForSingleObject(ev.0, 50);
                    if stop_t.load(Ordering::Acquire) {
                        break;
                    }
                    if w != WAIT_OBJECT_0 {
                        continue;
                    }
                    let mut padding = 0u32;
                    let hr = get_pad(client.0, &mut padding);
                    if hr < 0 {
                        on_error(format!("GetCurrentPadding failed ({hr:#010x})"));
                        break;
                    }
                    let avail = buf_frames.saturating_sub(padding);
                    if avail == 0 {
                        continue;
                    }
                    let mut ptr = null_mut();
                    let hr = get_buf(render.0, avail, &mut ptr);
                    if hr < 0 || ptr.is_null() {
                        on_error(format!("IAudioRenderClient::GetBuffer failed ({hr:#010x})"));
                        break;
                    }
                    let samples = avail as usize * ch;
                    match sample_fmt {
                        SampleFmt::F32 => {
                            let dst = std::slice::from_raw_parts_mut(ptr as *mut f32, samples);
                            fill(dst);
                        }
                        SampleFmt::I16 => {
                            conv_buf.resize(samples, 0.0);
                            fill(&mut conv_buf);
                            let dst = std::slice::from_raw_parts_mut(ptr as *mut i16, samples);
                            for (d, &s) in dst.iter_mut().zip(&conv_buf) {
                                *d = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                            }
                        }
                        SampleFmt::I32 => {
                            conv_buf.resize(samples, 0.0);
                            fill(&mut conv_buf);
                            let dst = std::slice::from_raw_parts_mut(ptr as *mut i32, samples);
                            for (d, &s) in dst.iter_mut().zip(&conv_buf) {
                                *d = (s.clamp(-1.0, 1.0) * 2_147_483_647.0).round() as i32;
                            }
                        }
                    }
                    let hr = rel_buf(render.0, avail, 0);
                    if hr < 0 {
                        on_error(format!("IAudioRenderClient::ReleaseBuffer failed ({hr:#010x})"));
                        break;
                    }
                }
                let stop_fn: unsafe extern "system" fn(*mut c_void) -> i32 = client.method(11);
                let _ = stop_fn(client.0);
            }
        })
        .map_err(|e| RenderError::Other(e.to_string()))?;

    match rx.recv() {
        Ok(Ok((info, wake_event))) => Ok((
            info,
            Stream {
                stop,
                wake_event,
                join: Some(join),
            },
        )),
        Ok(Err(e)) => {
            let _ = join.join();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            Err(RenderError::Other("render thread ended unexpectedly".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_audio_endpoints_without_panicking() {
        let outputs = output_device_names();
        let def_out = default_device_name(false);
        let _def_in = default_device_name(true);
        if !def_out.is_empty() {
            assert!(
                outputs.iter().any(|n| n == &def_out),
                "default output {def_out:?} not in active outputs {outputs:?}"
            );
        }
    }
}
