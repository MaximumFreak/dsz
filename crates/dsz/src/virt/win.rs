//! Win32 plumbing shared by the virtual-device backends: device-interface
//! lookup, handles, and overlapped `DeviceIoControl` with a timeout.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

pub use crate::hid::Handle;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// `CTL_CODE` from winioctl.h.
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

/// Paths of present device interfaces of one class.
pub fn interface_paths(guid: &GUID) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let set = SetupDiGetClassDevsW(
            guid,
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
            if SetupDiEnumDeviceInterfaces(set, null(), guid, index, &mut ifd) == 0 {
                break;
            }
            index += 1;
            let mut required = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut required, null_mut());
            if required == 0 {
                continue;
            }
            let mut buf = vec![0u64; (required as usize).div_ceil(8) + 1];
            let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, detail, required, null_mut(), null_mut())
                == 0
            {
                continue;
            }
            let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
            let max = (required as usize).saturating_sub(4) / 2;
            out.push(from_wide(std::slice::from_raw_parts(p, max)));
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// Open a device for overlapped I/O.
pub fn open_overlapped(path: &str) -> io::Result<Handle> {
    let w = wide(path);
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL
                | FILE_FLAG_NO_BUFFERING
                | FILE_FLAG_WRITE_THROUGH
                | FILE_FLAG_OVERLAPPED,
            null_mut(),
        )
    };
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle::from_raw(h))
}

/// Manual-reset event plus an OVERLAPPED that points at it. Boxed so the
/// kernel's pointer stays valid while a request is pending.
pub struct Overlapped {
    pub ov: Box<OVERLAPPED>,
    ev: Handle,
}

unsafe impl Send for Overlapped {}

impl Overlapped {
    pub fn new() -> io::Result<Overlapped> {
        let h = unsafe { CreateEventW(null(), 1, 0, null()) };
        if h.is_null() {
            return Err(io::Error::last_os_error());
        }
        let ev = Handle::from_raw(h);
        let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        ov.hEvent = ev.raw();
        Ok(Overlapped { ov, ev })
    }

    fn reset(&mut self) {
        let ev = self.ev.raw();
        *self.ov = unsafe { std::mem::zeroed() };
        self.ov.hEvent = ev;
        unsafe { ResetEvent(ev) };
    }

    /// Start an IOCTL. `Ok(Some(n))` finished at once, `Ok(None)` is pending.
    pub fn start(
        &mut self,
        h: HANDLE,
        code: u32,
        input: &[u8],
        output: &mut [u8],
    ) -> io::Result<Option<u32>> {
        self.reset();
        let mut got = 0u32;
        let ok = unsafe {
            DeviceIoControl(
                h,
                code,
                if input.is_empty() {
                    null()
                } else {
                    input.as_ptr() as *const c_void
                },
                input.len() as u32,
                if output.is_empty() {
                    null_mut()
                } else {
                    output.as_mut_ptr() as *mut c_void
                },
                output.len() as u32,
                &mut got,
                &mut *self.ov,
            )
        };
        if ok != 0 {
            return Ok(Some(got));
        }
        let err = unsafe { GetLastError() };
        if err == ERROR_IO_PENDING {
            Ok(None)
        } else {
            Err(io::Error::from_raw_os_error(err as i32))
        }
    }

    /// Wait for a pending request. `Ok(None)` is a timeout; the request stays queued.
    pub fn wait(&mut self, h: HANDLE, timeout_ms: u32) -> io::Result<Option<u32>> {
        let w = unsafe { WaitForSingleObject(self.ev.raw(), timeout_ms) };
        if w == WAIT_TIMEOUT {
            return Ok(None);
        }
        let mut got = 0u32;
        let ok = unsafe { GetOverlappedResult(h, &*self.ov, &mut got, 0) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(got))
    }

    /// Cancel a pending request and wait for the cancel to land.
    pub fn cancel(&mut self, h: HANDLE) {
        unsafe {
            CancelIoEx(h, &*self.ov);
            let mut got = 0u32;
            GetOverlappedResult(h, &*self.ov, &mut got, 1);
        }
    }

    /// Synchronous IOCTL on an overlapped handle, with a timeout.
    pub fn call(
        &mut self,
        h: HANDLE,
        code: u32,
        input: &[u8],
        output: &mut [u8],
        timeout_ms: u32,
    ) -> io::Result<u32> {
        if let Some(n) = self.start(h, code, input, output)? {
            return Ok(n);
        }
        match self.wait(h, timeout_ms)? {
            Some(n) => Ok(n),
            None => {
                self.cancel(h);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "driver request timed out",
                ))
            }
        }
    }
}

/// Synchronous IOCTL on a non-overlapped handle.
pub fn ioctl(h: HANDLE, code: u32, input: &[u8], output: &mut [u8]) -> io::Result<u32> {
    let mut got = 0u32;
    let ok = unsafe {
        DeviceIoControl(
            h,
            code,
            if input.is_empty() {
                null()
            } else {
                input.as_ptr() as *const c_void
            },
            input.len() as u32,
            if output.is_empty() {
                null_mut()
            } else {
                output.as_mut_ptr() as *mut c_void
            },
            output.len() as u32,
            &mut got,
            null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(got)
}

/// Device instance id (`HID\...`) behind a device-interface path.
pub fn instance_id_for_interface(path: &str) -> Option<String> {
    unsafe {
        let set = SetupDiCreateDeviceInfoList(null(), null_mut());
        if set as isize == -1 || set as isize == 0 {
            return None;
        }
        let w = wide(path);
        let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
        ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        let mut out = None;
        if SetupDiOpenDeviceInterfaceW(set, w.as_ptr(), 0, &mut ifd) != 0 {
            let mut dev: SP_DEVINFO_DATA = std::mem::zeroed();
            dev.cbSize = std::mem::size_of::<SP_DEVINFO_DATA>() as u32;
            let mut required = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &ifd, null_mut(), 0, &mut required, &mut dev);
            if dev.DevInst != 0 {
                out = device_id(dev.DevInst);
            }
        }
        SetupDiDestroyDeviceInfoList(set);
        out
    }
}

fn device_id(devinst: u32) -> Option<String> {
    let mut buf = [0u16; 512];
    let r = unsafe { CM_Get_Device_IDW(devinst, buf.as_mut_ptr(), buf.len() as u32, 0) };
    (r == CR_SUCCESS).then(|| from_wide(&buf))
}

/// Instance id of the parent node (the Bluetooth HID service for a BT pad).
pub fn parent_instance_id(instance_id: &str) -> Option<String> {
    unsafe {
        let w = wide(instance_id);
        let mut inst = 0u32;
        if CM_Locate_DevNodeW(&mut inst, w.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) != CR_SUCCESS {
            return None;
        }
        let mut parent = 0u32;
        if CM_Get_Parent(&mut parent, inst, 0) != CR_SUCCESS {
            return None;
        }
        device_id(parent)
    }
}

/// File version (`a.b.c.d`) from a PE version resource.
pub fn file_version(path: &str) -> Option<[u16; 4]> {
    let w = wide(path);
    unsafe {
        let n = GetFileVersionInfoSizeW(w.as_ptr(), null_mut());
        if n == 0 {
            return None;
        }
        let mut buf = vec![0u8; n as usize];
        if GetFileVersionInfoW(w.as_ptr(), 0, n, buf.as_mut_ptr() as *mut c_void) == 0 {
            return None;
        }
        let root = wide("\\");
        let mut p: *mut c_void = null_mut();
        let mut len = 0u32;
        if VerQueryValueW(
            buf.as_ptr() as *const c_void,
            root.as_ptr(),
            &mut p,
            &mut len,
        ) == 0
            || p.is_null()
        {
            return None;
        }
        let fi = &*(p as *const VS_FIXEDFILEINFO);
        Some([
            (fi.dwFileVersionMS >> 16) as u16,
            (fi.dwFileVersionMS & 0xFFFF) as u16,
            (fi.dwFileVersionLS >> 16) as u16,
            (fi.dwFileVersionLS & 0xFFFF) as u16,
        ])
    }
}

/// `C:\dir\app.exe` → `\Device\HarddiskVolume3\dir\app.exe`.
pub fn dos_device_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() < 3 || bytes[1] != b':' {
        return None;
    }
    let drive = &path[..2];
    let mut buf = [0u16; 512];
    let w = wide(drive);
    let n = unsafe { QueryDosDeviceW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return None;
    }
    Some(format!("{}{}", from_wide(&buf), &path[2..]))
}
