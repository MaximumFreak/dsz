//! Minimal Win32 HID: enumerate, open, overlapped read/write with timeouts,
//! feature reports.
//!
//! Reads keep one overlapped request pending across timeouts, so a quiet
//! radio never tears the stream down. Writes are padded to the device's
//! output report length, which Windows requires of `WriteFile` on HID.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::HumanInterfaceDevice::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub input_len: usize,
    pub output_len: usize,
    pub feature_len: usize,
    pub usage_page: u16,
    pub usage: u16,
    pub serial: String,
    pub product: String,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// Owned Win32 handle.
pub struct Handle(HANDLE);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// Take ownership of a handle; it is closed on drop.
    pub fn from_raw(h: HANDLE) -> Handle {
        Handle(h)
    }
    pub fn raw(&self) -> HANDLE {
        self.0
    }
    fn valid(&self) -> bool {
        !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.valid() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

fn open_path(path: &str, access: u32, overlapped: bool) -> io::Result<Handle> {
    let w = wide(path);
    let flags = if overlapped { FILE_FLAG_OVERLAPPED } else { 0 };
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            flags,
            null_mut(),
        )
    };
    let h = Handle(h);
    if !h.valid() {
        return Err(io::Error::last_os_error());
    }
    Ok(h)
}

fn query(path: &str) -> Option<DeviceInfo> {
    // Access 0: attributes and caps without contending for the device.
    let h = open_path(path, 0, false).ok()?;
    unsafe {
        let mut attr: HIDD_ATTRIBUTES = std::mem::zeroed();
        attr.Size = std::mem::size_of::<HIDD_ATTRIBUTES>() as u32;
        if HidD_GetAttributes(h.raw(), &mut attr) == 0 {
            return None;
        }
        let mut info = DeviceInfo {
            path: path.to_string(),
            vid: attr.VendorID,
            pid: attr.ProductID,
            input_len: 0,
            output_len: 0,
            feature_len: 0,
            usage_page: 0,
            usage: 0,
            serial: String::new(),
            product: String::new(),
        };
        if info.vid != ds_proto::SONY_VID || ds_proto::Model::from_pid(info.pid).is_none() {
            return Some(info);
        }
        let mut pp: PHIDP_PREPARSED_DATA = std::mem::zeroed();
        if HidD_GetPreparsedData(h.raw(), &mut pp) != 0 {
            let mut caps: HIDP_CAPS = std::mem::zeroed();
            if HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS {
                info.input_len = caps.InputReportByteLength as usize;
                info.output_len = caps.OutputReportByteLength as usize;
                info.feature_len = caps.FeatureReportByteLength as usize;
                info.usage_page = caps.UsagePage;
                info.usage = caps.Usage;
            }
            HidD_FreePreparsedData(pp);
        }
        let mut buf = [0u16; 128];
        if HidD_GetSerialNumberString(
            h.raw(),
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * 2) as u32,
        ) != 0
        {
            info.serial = from_wide(&buf);
        }
        let mut buf = [0u16; 128];
        if HidD_GetProductString(
            h.raw(),
            buf.as_mut_ptr() as *mut c_void,
            (buf.len() * 2) as u32,
        ) != 0
        {
            info.product = from_wide(&buf);
        }
        Some(info)
    }
}

/// All present DualSense / DualSense Edge game-pad collections, leaving
/// out the app's own virtual controllers.
pub fn enumerate() -> Vec<DeviceInfo> {
    enumerate_with(false)
}

/// Like [`enumerate`]; `virtual_only` returns just our virtual controllers.
pub fn enumerate_with(virtual_only: bool) -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    unsafe {
        let mut guid: GUID = std::mem::zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = SetupDiGetClassDevsW(
            &guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if set as isize == -1 || set as isize == 0 {
            return out;
        }
        let mut index = 0u32;
        loop {
            let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
            ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, null(), &guid, index, &mut ifd) == 0 {
                break;
            }
            index += 1;
            let mut required = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut required, null_mut());
            if required == 0 {
                continue;
            }
            // u64 backing keeps the struct aligned.
            let mut buf = vec![0u64; (required as usize).div_ceil(8) + 1];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, detail, required, null_mut(), null_mut())
                == 0
            {
                continue;
            }
            let path_ptr = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let max = (required as usize).saturating_sub(4) / 2;
            let path_slice = std::slice::from_raw_parts(path_ptr, max);
            let path = from_wide(path_slice);
            let lower = path.to_ascii_lowercase();
            // Cheap pre-filter on the path before opening anything.
            if !lower.contains("vid_054c")
                && !lower.contains("vid&0002054c")
                && !lower.contains("054c")
            {
                continue;
            }
            if let Some(info) = query(&path) {
                if info.vid == ds_proto::SONY_VID
                    && ds_proto::Model::from_pid(info.pid).is_some()
                    && info.usage_page == 0x01
                    && info.usage == 0x05
                    && info.input_len > 0
                    && crate::virt::usbip::is_virtual_path(&path) == virtual_only
                {
                    out.push(info);
                }
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// An open controller. Reader and writer each own their own OVERLAPPED.
pub struct HidDevice {
    io: Handle,
    ctl: Handle,
    pub info: DeviceInfo,
}

impl HidDevice {
    pub fn open(info: &DeviceInfo) -> io::Result<HidDevice> {
        let io = open_path(&info.path, GENERIC_READ | GENERIC_WRITE, true)?;
        let ctl = open_path(&info.path, GENERIC_READ | GENERIC_WRITE, false)?;
        unsafe {
            // A short queue keeps latency down if a reader stalls.
            HidD_SetNumInputBuffers(io.raw(), 8);
        }
        Ok(HidDevice {
            io,
            ctl,
            info: info.clone(),
        })
    }

    pub fn reader(&self) -> io::Result<Reader<'_>> {
        Reader::new(self)
    }

    pub fn writer(&self) -> io::Result<Writer<'_>> {
        Writer::new(self)
    }

    /// Read a feature report. `id` goes in byte 0.
    pub fn get_feature(&self, id: u8, len: usize) -> io::Result<Vec<u8>> {
        let n = len.max(self.info.feature_len).max(2);
        let mut buf = vec![0u8; n];
        buf[0] = id;
        let ok =
            unsafe { HidD_GetFeature(self.ctl.raw(), buf.as_mut_ptr() as *mut c_void, n as u32) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf)
    }
}

struct Event(Handle);

impl Event {
    fn new() -> io::Result<Event> {
        let h = unsafe { CreateEventW(null(), 1, 0, null()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Event(Handle(h)))
    }
}

/// Errors that mean the device node is gone, not just quiet.
pub fn is_fatal(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error().map(|c| c as u32),
        Some(ERROR_DEVICE_NOT_CONNECTED)
            | Some(ERROR_INVALID_HANDLE)
            | Some(ERROR_BAD_COMMAND)
            | Some(ERROR_FILE_NOT_FOUND)
            | Some(ERROR_DEV_NOT_EXIST)
            | Some(ERROR_ACCESS_DENIED)
    )
}

pub struct Reader<'a> {
    dev: &'a HidDevice,
    ov: Box<OVERLAPPED>,
    ev: Event,
    buf: Box<[u8]>,
    pending: bool,
}

unsafe impl Send for Reader<'_> {}

impl<'a> Reader<'a> {
    fn new(dev: &'a HidDevice) -> io::Result<Self> {
        let ev = Event::new()?;
        let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        ov.hEvent = ev.0.raw();
        Ok(Reader {
            dev,
            ov,
            ev,
            buf: vec![0u8; dev.info.input_len.max(64)].into_boxed_slice(),
            pending: false,
        })
    }

    /// Wait up to `timeout_ms` for one report. `Ok(None)` is a timeout; the
    /// request stays queued for the next call.
    pub fn read(&mut self, timeout_ms: u32) -> io::Result<Option<&[u8]>> {
        unsafe {
            if !self.pending {
                ResetEvent(self.ev.0.raw());
                let mut got = 0u32;
                let ok = ReadFile(
                    self.dev.io.raw(),
                    self.buf.as_mut_ptr(),
                    self.buf.len() as u32,
                    &mut got,
                    &mut *self.ov,
                );
                if ok == 0 {
                    let err = GetLastError();
                    if err != ERROR_IO_PENDING {
                        return Err(io::Error::from_raw_os_error(err as i32));
                    }
                    self.pending = true;
                } else {
                    return Ok(Some(&self.buf[..got as usize]));
                }
            }
            let w = WaitForSingleObject(self.ev.0.raw(), timeout_ms);
            if w == WAIT_TIMEOUT {
                return Ok(None);
            }
            if w != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
            let mut got = 0u32;
            let ok = GetOverlappedResult(self.dev.io.raw(), &*self.ov, &mut got, 0);
            self.pending = false;
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(&self.buf[..got as usize]))
        }
    }
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        if self.pending {
            unsafe {
                CancelIoEx(self.dev.io.raw(), &*self.ov);
                let mut got = 0u32;
                GetOverlappedResult(self.dev.io.raw(), &*self.ov, &mut got, 1);
            }
        }
    }
}

pub struct Writer<'a> {
    dev: &'a HidDevice,
    ov: Box<OVERLAPPED>,
    ev: Event,
    buf: Box<[u8]>,
}

unsafe impl Send for Writer<'_> {}

impl<'a> Writer<'a> {
    fn new(dev: &'a HidDevice) -> io::Result<Self> {
        let ev = Event::new()?;
        let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        ov.hEvent = ev.0.raw();
        let len = dev.info.output_len.max(ds_proto::stream::MAX_REPORT);
        Ok(Writer {
            dev,
            ov,
            ev,
            buf: vec![0u8; len].into_boxed_slice(),
        })
    }

    /// Write one report, padded to the output report length.
    pub fn write(&mut self, report: &[u8], timeout_ms: u32) -> io::Result<()> {
        let n = self.dev.info.output_len.max(report.len());
        let n = n.min(self.buf.len());
        self.buf[..report.len()].copy_from_slice(report);
        self.buf[report.len()..n].fill(0);
        unsafe {
            ResetEvent(self.ev.0.raw());
            let mut put = 0u32;
            let ok = WriteFile(
                self.dev.io.raw(),
                self.buf.as_ptr(),
                n as u32,
                &mut put,
                &mut *self.ov,
            );
            if ok != 0 {
                return Ok(());
            }
            let err = GetLastError();
            if err != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            let w = WaitForSingleObject(self.ev.0.raw(), timeout_ms);
            if w == WAIT_OBJECT_0 {
                let ok = GetOverlappedResult(self.dev.io.raw(), &*self.ov, &mut put, 0);
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            // Timed out: cancel and wait for the cancel to land so the buffer
            // and OVERLAPPED are free again.
            CancelIoEx(self.dev.io.raw(), &*self.ov);
            GetOverlappedResult(self.dev.io.raw(), &*self.ov, &mut put, 1);
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HID write timed out",
            ))
        }
    }
}
