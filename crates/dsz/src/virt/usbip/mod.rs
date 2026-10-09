//! Virtual wired controllers served over USB/IP to the usbip-win2 driver:
//! a DualSense, an Xbox 360 controller, or a DualShock 4.
//!
//! The app is the USB device: it listens on a loopback port, asks the
//! driver to attach it (`PLUGIN_HARDWARE_ONCE`), and answers every URB
//! itself. Windows loads its own drivers on top: HID and USB audio for the
//! DualSense (games see the 4-channel speaker/actuator endpoint and write
//! their haptics audio to it), the in-box `xusb22` for the Xbox 360 pad
//! (XInput), and HID for the DualShock 4. No other driver is needed.
//!
//! ```text
//! game ─ HID / WASAPI ─► usbip-win2 (UDE) ─ TCP 127.0.0.1 ─► reader thread
//!   ◄── input reports ── pipeline: submit() ──────────────────┘   │ EP0, EP3 (output report)
//!   ◄── RET_SUBMIT at the end of each ISO window ── clock thread ◄┘ EP1 PCM → AudioRing
//! ```
//!
//! Timing follows a real host controller. Isochronous URBs are reserved
//! back to back on an absolute clock (one packet per millisecond) and each
//! completes when its window ends, so the audio engine runs at real time;
//! after a gap of a full packet the clock re-anchors instead of replaying.
//! PCM is published as soon as a URB arrives. Input completes a pending
//! interrupt URB the moment the physical report is processed.

pub mod descriptors;
pub mod wire;

use std::collections::{BTreeSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use windows_sys::core::GUID;

use self::descriptors as d;
use self::wire::{Command, Submit};
use super::win;
use crate::haptics::game::PcmSource;

/// Which controller a virtual pad presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    DualSense,
    Xbox360,
    DualShock4,
}

impl Model {
    pub fn name(self) -> &'static str {
        match self {
            Model::DualSense => "DualSense",
            Model::Xbox360 => "Xbox 360 controller",
            Model::DualShock4 => "DualShock 4",
        }
    }
    fn ids(self) -> (u16, u16) {
        match self {
            Model::DualSense => (d::VID, d::PID),
            Model::Xbox360 => (d::x360::VID, d::x360::PID),
            Model::DualShock4 => (d::ds4::VID, d::ds4::PID),
        }
    }
    fn interfaces(self) -> &'static [(u8, u8, u8)] {
        match self {
            Model::DualSense => &[(1, 1, 0), (1, 2, 0), (1, 2, 0), (3, 0, 0)],
            Model::Xbox360 => &[(0xFF, 0x5D, 0x01)],
            Model::DualShock4 => &[(3, 0, 0)],
        }
    }
    /// Interrupt endpoints: (input, output).
    fn endpoints(self) -> (u32, u32) {
        match self {
            Model::DualSense => (d::EP_INPUT, d::EP_OUTPUT),
            Model::Xbox360 => (d::x360::EP_INPUT, d::x360::EP_OUTPUT),
            Model::DualShock4 => (d::ds4::EP_INPUT, d::ds4::EP_OUTPUT),
        }
    }
    fn hid_interface(self) -> Option<u8> {
        match self {
            Model::DualSense => Some(d::IF_HID),
            Model::Xbox360 => None,
            Model::DualShock4 => Some(d::ds4::IF_HID),
        }
    }
    fn input_len(self) -> usize {
        match self {
            Model::Xbox360 => ds_proto::virtual_pad::X360_USB_INPUT_LEN,
            _ => 64,
        }
    }
    /// A real Xbox 360 pad reports only changes; the Sony pads stream.
    fn keepalive(self) -> Option<Duration> {
        match self {
            Model::Xbox360 => None,
            _ => Some(INPUT_KEEPALIVE),
        }
    }
    fn idle_input(self) -> [u8; 64] {
        let mut r = [0u8; 64];
        match self {
            Model::Xbox360 => {
                r[1] = 0x14;
            }
            Model::DualSense => {
                r[0] = 0x01;
                r[1..5].copy_from_slice(&[0x80; 4]);
                r[8] = 0x08;
            }
            Model::DualShock4 => {
                let pad = ds_proto::virtual_pad::PadState::default();
                r = ds_proto::virtual_pad::ds4_usb_input(&pad, None, false, 0);
            }
        }
        r
    }
}

/// Game-facing callbacks and canned answers for one virtual pad.
pub struct DsSink {
    /// Called with each output the game or driver writes: the DualSense's
    /// USB output report (`0x02`), the DualShock 4's (`0x05`), or an Xbox 360
    /// interrupt-OUT packet (rumble, ring LED).
    pub on_output: Box<dyn Fn(&[u8]) + Send + Sync>,
    /// Feature reports from the physical controller driving it. Shared, so
    /// a pad that outlives its controller answers with whichever controller
    /// drives it now.
    pub mirror: std::sync::Arc<parking_lot::Mutex<super::mirror::Mirror>>,
}

impl DsSink {
    /// Answer for a feature-report `GET_REPORT`; `None` declines it.
    pub fn feature(&self, model: Model, id: u8) -> Option<Vec<u8>> {
        let m = self.mirror.lock();
        if model == Model::DualShock4 {
            return d::ds4::feature(id, m.calibration.as_deref());
        }
        m.answer(id)
    }
}

const GUID_VHCI: GUID = GUID::from_u128(0xB4030C06_DC5F_4FCC_87EB_E5515A0935C0);
const IOCTL_PLUGOUT_HARDWARE: u32 = win::ctl_code(0x22, 0x801, 0, 3);
const IOCTL_PLUGIN_HARDWARE_ONCE: u32 = win::ctl_code(0x22, 0x806, 0, 3);
/// `vhci::ioctl::plugin_hardware` in 0.9.7.7: size, port, busid[32],
/// service[32], host[1025], padding. 0.9.7.8 appends serial[16]; we serve
/// our serial in the device descriptor instead, so it is never needed.
const PLUGIN_SIZE: usize = 1100;
const PLUGIN_SIZE_WITH_SERIAL: usize = 1116;

const MS: Duration = Duration::from_millis(1);
/// Resend the last input report when nothing new arrived for this long.
const INPUT_KEEPALIVE: Duration = Duration::from_millis(8);
/// About 200 ms of game PCM.
const RING_FRAMES: usize = 9600;

/// Serial prefix that marks our virtual controllers in device instance ids.
pub const SERIAL_PREFIX: &str = "DSZV";

/// Whether a usbip-win2 host controller we may use is present.
pub fn available() -> bool {
    blocked().is_none() && !win::interface_paths(&GUID_VHCI).is_empty()
}

/// Path of the active `usbip2_ude.sys`, from its service entry.
fn driver_path() -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::Registry::*;
    let key = win::wide(r"SYSTEM\CurrentControlSet\Services\usbip2_ude");
    let name = win::wide("ImagePath");
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            &mut len,
        )
    };
    if r != 0 {
        return None;
    }
    let raw = win::from_wide(&buf);
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let rest = raw.trim_start_matches(r"\??\");
    let path = if let Some(p) = rest.strip_prefix(r"\SystemRoot\") {
        format!(r"{windir}\{p}")
    } else if rest.len() > 1 && rest.as_bytes()[1] == b':' {
        rest.to_string()
    } else {
        format!(r"{windir}\{}", rest.trim_start_matches('\\'))
    };
    let path = std::path::PathBuf::from(path);
    path.exists().then_some(path)
}

/// Installed usbip-win2 release. The driver files carry no version
/// resource, so this is `usbip.exe`'s, which the installer puts down with
/// them.
pub fn driver_version() -> Option<[u16; 4]> {
    let pf = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
    win::file_version(&format!(r"{pf}\USBip\usbip.exe"))
}

/// `DriverVer` date (`MM/DD/YYYY`) from the active driver's INF.
fn driver_date() -> Option<String> {
    let inf = driver_path()?.with_file_name("usbip2_ude.inf");
    let text = std::fs::read_to_string(inf).ok()?;
    let line = text
        .lines()
        .find(|l| l.trim_start().to_ascii_lowercase().starts_with("driverver"))?;
    let v = line.split('=').nth(1)?.trim();
    Some(v.split(',').next()?.trim().to_string())
}

/// Why the installed usbip-win2 must not be used, if it mustn't. 0.9.7.8
/// corrupts kernel memory (its author's release note; it crashed the
/// reference PC twice, once with a game opening the virtual DualSense).
pub fn blocked() -> Option<String> {
    let bad_version = driver_version()
        .map(|v| v[..] == [0, 9, 7, 8])
        .unwrap_or(false);
    // 0.9.7.8's driver was built in early July 2026; catch it even when
    // usbip.exe is missing.
    let bad_date = driver_date()
        .map(|d| d.starts_with("07/0") && d.ends_with("/2026"))
        .unwrap_or(false);
    (bad_version || bad_date).then(|| {
        "usbip-win2 0.9.7.8 is installed. That release can corrupt memory and crash Windows (its author's warning), \
so DualSense mode stays off. Uninstall it, restart, and install 0.9.7.7."
            .to_string()
    })
}

/// Whether a HID device path belongs to one of our virtual controllers.
pub fn is_virtual_path(path: &str) -> bool {
    win::instance_id_for_interface(path)
        .map(|id| is_ours_instance(&id))
        .unwrap_or(false)
}

/// Whether a device instance, or one of its parents, is our virtual
/// controller (its USB serial starts with [`SERIAL_PREFIX`]).
pub fn is_ours_instance(id: &str) -> bool {
    let tag = format!("\\{SERIAL_PREFIX}");
    let mut id = id.to_string();
    for _ in 0..4 {
        if id.to_ascii_uppercase().contains(&tag) {
            return true;
        }
        match win::parent_instance_id(&id) {
            Some(p) => id = p,
            None => break,
        }
    }
    false
}

// ------------------------------------------------------------------ audio

/// The game's 4-channel render stream, as the haptics streamer reads it.
pub struct AudioRing {
    q: Mutex<VecDeque<i16>>,
    pub frames_in: AtomicU64,
}

impl AudioRing {
    fn new() -> AudioRing {
        AudioRing {
            q: Mutex::new(VecDeque::with_capacity(RING_FRAMES * d::RENDER_CHANNELS)),
            frames_in: AtomicU64::new(0),
        }
    }

    /// Append little-endian PCM, with `gain` on the speaker channels.
    fn push(&self, pcm: &[u8], gain: f32) {
        let mut q = self.q.lock();
        let frames = pcm.len() / (2 * d::RENDER_CHANNELS);
        for (i, c) in pcm
            .as_chunks::<2>()
            .0
            .iter()
            .take(frames * d::RENDER_CHANNELS)
            .enumerate()
        {
            let mut v = i16::from_le_bytes([c[0], c[1]]);
            if i % d::RENDER_CHANNELS < 2 && gain != 1.0 {
                v = (v as f32 * gain) as i16;
            }
            q.push_back(v);
        }
        let over = q.len().saturating_sub(RING_FRAMES * d::RENDER_CHANNELS);
        q.drain(..over);
        self.frames_in.fetch_add(frames as u64, Ordering::Relaxed);
    }

    fn clear(&self) {
        self.q.lock().clear();
    }
}

impl PcmSource for AudioRing {
    fn filled_frames(&self) -> usize {
        self.q.lock().len() / d::RENDER_CHANNELS
    }
    fn read(&self, out: &mut [i16]) -> usize {
        let mut q = self.q.lock();
        let frames = (out.len() / d::RENDER_CHANNELS).min(q.len() / d::RENDER_CHANNELS);
        for (o, v) in out.iter_mut().zip(q.drain(..frames * d::RENDER_CHANNELS)) {
            *o = v;
        }
        frames
    }
    fn drop_frames(&self, n: usize) {
        let mut q = self.q.lock();
        let n = (n * d::RENDER_CHANNELS).min(q.len());
        q.drain(..n);
    }
}

// ------------------------------------------------------------------ device state

/// Audio-class and HID control state the host sets.
struct Ctl {
    alt: [u8; 4],
    speaker_mute: bool,
    /// Q8.8 dB, -100..0.
    speaker_volume: i16,
    mic_mute: bool,
    mic_volume: i16,
    idle: u8,
}

impl Ctl {
    fn speaker_gain(&self) -> f32 {
        if self.speaker_mute {
            0.0
        } else {
            10f32.powf(self.speaker_volume as f32 / 256.0 / 20.0)
        }
    }
}

const SPEAKER_VOL: (i16, i16, i16) = (-100 * 256, 0, 256); // min, max, resolution
const MIC_VOL: (i16, i16, i16) = (0, 48 * 256, 0x7A);

struct IsoJob {
    seq: u32,
    lens: Vec<u32>,
    due: Instant,
    frame: u32,
}

#[derive(Default)]
struct IsoClock {
    q: VecDeque<IsoJob>,
    cursor: Option<Instant>,
}

impl IsoClock {
    /// Reserve the next window for `lens.len()` one-millisecond packets.
    fn admit(&mut self, seq: u32, lens: Vec<u32>, now: Instant, epoch: Instant) {
        let start = match self.cursor {
            Some(c) if c + MS >= now => c,
            _ => now,
        };
        let due = start + MS * lens.len().max(1) as u32;
        self.cursor = Some(due);
        let frame = start.saturating_duration_since(epoch).as_millis() as u32;
        self.q.push_back(IsoJob {
            seq,
            lens,
            due,
            frame,
        });
    }

    fn remove(&mut self, seq: u32) -> bool {
        match self.q.iter().position(|j| j.seq == seq) {
            Some(i) => {
                self.q.remove(i);
                if self.q.is_empty() {
                    self.cursor = None;
                }
                true
            }
            None => false,
        }
    }
}

/// The live connection: the socket writer plus every URB we hold.
struct Conn {
    w: TcpStream,
    buf: Vec<u8>,
    int_in: VecDeque<u32>,
    last_input: Instant,
    render: IsoClock,
    capture: IsoClock,
    broken: bool,
}

impl Conn {
    fn send(&mut self, bytes: &[u8]) {
        if self.broken {
            return;
        }
        if let Err(e) = self.w.write_all(bytes) {
            log::warn!("virtual DualSense: USB/IP write failed: {e}");
            self.broken = true;
            let _ = self.w.shutdown(Shutdown::Both);
        }
    }

    fn ret(
        &mut self,
        seq: u32,
        status: i32,
        actual: u32,
        frame: u32,
        iso: Option<&[(u32, u32, u32)]>,
        data: &[u8],
    ) {
        let mut buf = std::mem::take(&mut self.buf);
        wire::ret_submit(&mut buf, seq, status, actual, frame, iso, data);
        self.send(&buf);
        self.buf = buf;
    }

    fn complete_input(&mut self, seq: u32, report: &[u8]) {
        self.ret(seq, 0, report.len() as u32, 0, None, report);
        self.last_input = Instant::now();
    }
}

struct Shared {
    model: Model,
    stop: AtomicBool,
    sink: DsSink,
    conn: Mutex<Option<Conn>>,
    input: Mutex<[u8; 64]>,
    fresh: AtomicBool,
    audio: Arc<AudioRing>,
    ctl: Mutex<Ctl>,
    epoch: Instant,
    busid: String,
    devnum: u32,
    ended: AtomicBool,
    urbs: AtomicU64,
    /// Last Xbox 360 ring-LED pattern the driver set (the XInput slot).
    led: std::sync::atomic::AtomicU8,
}

impl Shared {
    fn input_now(&self) -> ([u8; 64], usize) {
        (*self.input.lock(), self.model.input_len())
    }
}

// ------------------------------------------------------------------ public handle

static SLOTS: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

pub struct UsbPad {
    shared: Arc<Shared>,
    addr: SocketAddr,
    port: AtomicI32,
    slot: u32,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl UsbPad {
    /// Serve a virtual pad and attach it. Blocks until Windows has the
    /// device (a few hundred milliseconds).
    pub fn create(model: Model, sink: DsSink) -> io::Result<UsbPad> {
        let paths = win::interface_paths(&GUID_VHCI);
        let Some(vhci_path) = paths.first() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "usbip-win2 is not installed",
            ));
        };
        let slot = {
            let mut s = SLOTS.lock();
            let n = (1..100).find(|n| !s.contains(n)).unwrap_or(99);
            s.insert(n);
            n
        };
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let idle = model.idle_input();
        let shared = Arc::new(Shared {
            model,
            stop: AtomicBool::new(false),
            sink,
            conn: Mutex::new(None),
            input: Mutex::new(idle),
            fresh: AtomicBool::new(false),
            audio: Arc::new(AudioRing::new()),
            ctl: Mutex::new(Ctl {
                alt: [0; 4],
                speaker_mute: false,
                speaker_volume: SPEAKER_VOL.1,
                mic_mute: false,
                mic_volume: MIC_VOL.1,
                idle: 0,
            }),
            epoch: Instant::now(),
            busid: format!("1-{slot}"),
            devnum: slot,
            ended: AtomicBool::new(false),
            urbs: AtomicU64::new(0),
            led: Default::default(),
        });
        // From here on Drop releases the slot, detaches, and joins.
        let dev = UsbPad {
            shared: shared.clone(),
            addr,
            port: AtomicI32::new(0),
            slot,
            threads: Mutex::new(Vec::new()),
        };
        {
            let s = shared.clone();
            let t = std::thread::Builder::new()
                .name(format!("usbip-ds{slot}"))
                .spawn(move || serve(&s, listener))?;
            dev.threads.lock().push(t);
            let s = shared.clone();
            let t = std::thread::Builder::new()
                .name(format!("usbip-ds{slot}-clock"))
                .spawn(move || clock(&s))?;
            dev.threads.lock().push(t);
        }

        // Ask the driver to connect to us.
        if let Some(why) = blocked() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, why));
        }
        let h = win::open_overlapped(vhci_path)?;
        let mut ov = win::Overlapped::new()?;
        let mut out = vec![0u8; PLUGIN_SIZE_WITH_SERIAL];
        let mut last = None;
        // Later releases may grow the struct; the driver rejects a size it
        // doesn't expect before doing anything, so try the known ones.
        for size in [PLUGIN_SIZE, PLUGIN_SIZE_WITH_SERIAL] {
            let mut req = vec![0u8; size];
            req[..4].copy_from_slice(&(size as u32).to_le_bytes());
            let put = |req: &mut [u8], at: usize, len: usize, s: &str| {
                let b = s.as_bytes();
                let n = b.len().min(len - 1);
                req[at..at + n].copy_from_slice(&b[..n]);
            };
            put(&mut req, 8, 32, &shared.busid);
            put(&mut req, 40, 32, &addr.port().to_string());
            put(&mut req, 72, 1025, "127.0.0.1");
            match ov.call(
                h.raw(),
                IOCTL_PLUGIN_HARDWARE_ONCE,
                &req,
                &mut out[..size],
                15_000,
            ) {
                Ok(_) => {
                    last = None;
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        if let Some(e) = last {
            return Err(io::Error::new(e.kind(), format!("usbip-win2 attach: {e}")));
        }
        let port = i32::from_le_bytes([out[4], out[5], out[6], out[7]]);
        if port <= 0 {
            return Err(io::Error::other("usbip-win2 attach returned no port"));
        }
        dev.port.store(port, Ordering::Release);
        log::info!(
            "virtual {} attached on usbip-win2 port {port} ({})",
            model.name(),
            shared.busid
        );
        Ok(dev)
    }

    pub fn model(&self) -> Model {
        self.shared.model
    }

    /// XInput user index of a virtual Xbox 360 pad, from the ring LED the
    /// driver lit.
    pub fn xinput_slot(&self) -> Option<u32> {
        ds_proto::virtual_pad::x360_player(self.shared.led.load(Ordering::Relaxed))
    }

    /// Present an input report: the model's USB interrupt-IN packet.
    pub fn submit(&self, report: &[u8]) -> io::Result<()> {
        if self.shared.ended.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "virtual controller was detached",
            ));
        }
        let n = report.len().min(64);
        self.shared.input.lock()[..n].copy_from_slice(&report[..n]);
        let mut g = self.shared.conn.lock();
        if let Some(c) = g.as_mut() {
            if let Some(seq) = c.int_in.pop_front() {
                c.complete_input(seq, &report[..n]);
                self.shared.fresh.store(false, Ordering::Release);
            } else {
                self.shared.fresh.store(true, Ordering::Release);
            }
        }
        Ok(())
    }

    /// Centered sticks, nothing pressed.
    pub fn submit_idle(&self) -> io::Result<()> {
        let m = self.shared.model;
        self.submit(&m.idle_input()[..m.input_len()])
    }

    pub fn audio(&self) -> Arc<AudioRing> {
        self.shared.audio.clone()
    }

    /// Still attached (false once Windows or the driver dropped it).
    pub fn alive(&self) -> bool {
        !self.shared.ended.load(Ordering::Acquire)
    }

    /// Whether a game has the render stream open.
    pub fn streaming(&self) -> bool {
        self.shared.ctl.lock().alt[d::IF_RENDER as usize] != 0
    }

    pub fn urbs(&self) -> u64 {
        self.shared.urbs.load(Ordering::Relaxed)
    }
}

impl Drop for UsbPad {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        let port = self.port.load(Ordering::Acquire);
        if port > 0 {
            if let Some(path) = win::interface_paths(&GUID_VHCI).first() {
                if let (Ok(h), Ok(mut ov)) = (win::open_overlapped(path), win::Overlapped::new()) {
                    let mut req = [0u8; 8];
                    req[..4].copy_from_slice(&8u32.to_le_bytes());
                    req[4..].copy_from_slice(&port.to_le_bytes());
                    if let Err(e) = ov.call(h.raw(), IOCTL_PLUGOUT_HARDWARE, &req, &mut [], 5_000) {
                        log::warn!("usbip-win2 detach: {e}");
                    }
                }
            }
        }
        if let Some(c) = self.shared.conn.lock().as_ref() {
            let _ = c.w.shutdown(Shutdown::Both);
        }
        // Wake a reader still waiting in accept().
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        for t in self.threads.lock().drain(..) {
            let _ = t.join();
        }
        SLOTS.lock().remove(&self.slot);
        log::info!("virtual {} removed", self.shared.model.name());
    }
}

// ------------------------------------------------------------------ server

fn device_info(s: &Shared) -> Vec<u8> {
    let (vid, pid) = s.model.ids();
    wire::usb_device(&wire::DeviceInfo {
        busid: &s.busid,
        busnum: 1,
        devnum: s.devnum,
        speed: if s.model == Model::DualSense {
            d::SPEED_HIGH
        } else {
            d::SPEED_FULL
        },
        vid,
        pid,
        bcd_device: if s.model == Model::Xbox360 { 0x0114 } else { 0x0100 },
        num_interfaces: s.model.interfaces().len() as u8,
    })
}

/// Accept the driver's connection, do the import handshake, then read
/// URBs until it goes away.
fn serve(s: &Arc<Shared>, listener: TcpListener) {
    let stream = loop {
        let (mut st, peer) = match listener.accept() {
            Ok(x) => x,
            Err(e) => {
                log::warn!("virtual {}: accept: {e}", s.model.name());
                s.ended.store(true, Ordering::Release);
                return;
            }
        };
        if s.stop.load(Ordering::Acquire) {
            s.ended.store(true, Ordering::Release);
            return;
        }
        if !peer.ip().is_loopback() {
            continue;
        }
        let _ = st.set_read_timeout(Some(Duration::from_secs(5)));
        let mut op = [0u8; 8];
        if st.read_exact(&mut op).is_err() {
            continue;
        }
        let code = u16::from_be_bytes([op[2], op[3]]);
        match code {
            wire::OP_REQ_DEVLIST => {
                let mut r = wire::op_common(wire::OP_REP_DEVLIST, 0).to_vec();
                r.extend_from_slice(&1u32.to_be_bytes());
                r.extend_from_slice(&device_info(s));
                for &(class, sub, proto) in s.model.interfaces() {
                    r.extend_from_slice(&[class, sub, proto, 0]);
                }
                let _ = st.write_all(&r);
            }
            wire::OP_REQ_IMPORT => {
                let mut busid = [0u8; 32];
                if st.read_exact(&mut busid).is_err() {
                    continue;
                }
                let want = String::from_utf8_lossy(&busid)
                    .trim_end_matches('\0')
                    .to_string();
                if want != s.busid {
                    let _ = st.write_all(&wire::op_common(wire::OP_REP_IMPORT, 4));
                    continue;
                }
                let mut r = wire::op_common(wire::OP_REP_IMPORT, 0).to_vec();
                r.extend_from_slice(&device_info(s));
                if st.write_all(&r).is_ok() {
                    break st;
                }
            }
            _ => {}
        }
    };
    drop(listener);
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_nodelay(true);
    let Ok(w) = stream.try_clone() else {
        s.ended.store(true, Ordering::Release);
        return;
    };
    *s.conn.lock() = Some(Conn {
        w,
        buf: Vec::with_capacity(4096),
        int_in: VecDeque::new(),
        last_input: Instant::now(),
        render: IsoClock::default(),
        capture: IsoClock::default(),
        broken: false,
    });

    let mut r = io::BufReader::with_capacity(64 * 1024, stream);
    let mut hdr = [0u8; wire::HEADER];
    loop {
        match wire::read_command(&mut r, &mut hdr) {
            Ok(Some(Command::Submit(sub))) => {
                s.urbs.fetch_add(1, Ordering::Relaxed);
                submit(s, sub);
            }
            Ok(Some(Command::Unlink { seq, victim })) => unlink(s, seq, victim),
            Ok(None) => break,
            Err(e) => {
                if !s.stop.load(Ordering::Acquire) {
                    log::warn!("virtual {}: USB/IP read: {e}", s.model.name());
                }
                break;
            }
        }
    }
    *s.conn.lock() = None;
    s.audio.clear();
    s.ended.store(true, Ordering::Release);
    if !s.stop.load(Ordering::Acquire) {
        log::warn!("virtual {}: the driver closed the connection", s.model.name());
    }
}

fn unlink(s: &Shared, seq: u32, victim: u32) {
    let mut g = s.conn.lock();
    let Some(c) = g.as_mut() else { return };
    let mut found = false;
    if let Some(i) = c.int_in.iter().position(|&q| q == victim) {
        c.int_in.remove(i);
        found = true;
    }
    found = found || c.render.remove(victim) || c.capture.remove(victim);
    c.send(&wire::ret_unlink(
        seq,
        if found { wire::ECONNRESET } else { 0 },
    ));
}

fn submit(s: &Shared, u: Submit) {
    match (u.ep, u.dir) {
        (0, _) => {
            let (status, data) = match control(s, &u) {
                Some(d) => (0, d),
                None => (wire::EPIPE, Vec::new()),
            };
            let mut g = s.conn.lock();
            if let Some(c) = g.as_mut() {
                if u.dir == wire::DIR_IN {
                    let n = data.len().min(u.length as usize);
                    c.ret(u.seq, status, n as u32, 0, None, &data[..n]);
                } else {
                    let n = if status == 0 { u.data.len() as u32 } else { 0 };
                    c.ret(u.seq, status, n, 0, None, &[]);
                }
            }
        }
        (ep, wire::DIR_IN) if ep == s.model.endpoints().0 => {
            let mut g = s.conn.lock();
            let Some(c) = g.as_mut() else { return };
            if s.fresh.swap(false, Ordering::AcqRel) {
                let (rep, n) = s.input_now();
                c.complete_input(u.seq, &rep[..n]);
            } else {
                c.int_in.push_back(u.seq);
            }
        }
        (ep, wire::DIR_OUT) if ep == s.model.endpoints().1 => {
            {
                let mut g = s.conn.lock();
                if let Some(c) = g.as_mut() {
                    c.ret(u.seq, 0, u.data.len() as u32, 0, None, &[]);
                }
            }
            if s.model == Model::Xbox360 {
                if let Some(ds_proto::virtual_pad::X360Output::Led(n)) =
                    ds_proto::virtual_pad::parse_x360_output(&u.data)
                {
                    s.led.store(n, Ordering::Relaxed);
                }
            }
            (s.sink.on_output)(&u.data);
        }
        (d::EP_RENDER, wire::DIR_OUT) if s.model == Model::DualSense && u.packets.is_some() => {
            let gain = s.ctl.lock().speaker_gain();
            s.audio.push(&u.data, gain);
            let mut g = s.conn.lock();
            if let Some(c) = g.as_mut() {
                c.render.admit(u.seq, u.iso_lens, Instant::now(), s.epoch);
            }
        }
        (d::EP_CAPTURE, wire::DIR_IN) if s.model == Model::DualSense && u.packets.is_some() => {
            let mut g = s.conn.lock();
            if let Some(c) = g.as_mut() {
                c.capture.admit(u.seq, u.iso_lens, Instant::now(), s.epoch);
            }
        }
        _ => {
            log::debug!(
                "virtual {}: unexpected URB ep {} dir {}",
                s.model.name(),
                u.ep,
                u.dir
            );
            let mut g = s.conn.lock();
            if let Some(c) = g.as_mut() {
                c.ret(u.seq, wire::EPIPE, 0, 0, None, &[]);
            }
        }
    }
}

/// Completes isochronous windows on time and keeps input flowing.
fn clock(s: &Arc<Shared>) {
    crate::platform::timer_resolution_1ms();
    crate::platform::raise_thread_priority(false);
    let timer = crate::platform::PreciseTimer::new();
    let mut descs: Vec<(u32, u32, u32)> = Vec::with_capacity(64);
    let mut zeros: Vec<u8> = Vec::new();
    while !s.stop.load(Ordering::Acquire) && !s.ended.load(Ordering::Acquire) {
        let now = Instant::now();
        let mut next = now + Duration::from_millis(4);
        {
            let mut g = s.conn.lock();
            if let Some(c) = g.as_mut() {
                while c.render.q.front().map(|j| j.due <= now).unwrap_or(false) {
                    let j = c.render.q.pop_front().unwrap();
                    descs.clear();
                    let mut off = 0;
                    for &l in &j.lens {
                        descs.push((off, l, l));
                        off += l;
                    }
                    c.ret(j.seq, 0, off, j.frame, Some(&descs), &[]);
                }
                while c.capture.q.front().map(|j| j.due <= now).unwrap_or(false) {
                    let j = c.capture.q.pop_front().unwrap();
                    descs.clear();
                    let mut off = 0;
                    let mut total = 0;
                    for &l in &j.lens {
                        let a = l.min(d::CAPTURE_PACKET as u32);
                        descs.push((off, l, a));
                        off += l;
                        total += a;
                    }
                    zeros.resize(total as usize, 0);
                    c.ret(j.seq, 0, total, j.frame, Some(&descs), &zeros);
                }
                if let Some(j) = c.render.q.front() {
                    next = next.min(j.due);
                }
                if let Some(j) = c.capture.q.front() {
                    next = next.min(j.due);
                }
                if let (false, Some(every)) = (c.int_in.is_empty(), s.model.keepalive()) {
                    let due = c.last_input + every;
                    if due <= now {
                        let seq = c.int_in.pop_front().unwrap();
                        let (rep, n) = s.input_now();
                        c.complete_input(seq, &rep[..n]);
                    } else {
                        next = next.min(due);
                    }
                }
            } else {
                next = now + Duration::from_millis(20);
            }
        }
        timer.sleep_until(next);
    }
}

// ------------------------------------------------------------------ control requests

/// Answer an EP0 request. `None` stalls.
fn control(s: &Shared, u: &Submit) -> Option<Vec<u8>> {
    let st = u.setup;
    let (rt, req) = (st[0], st[1]);
    let value = u16::from_le_bytes([st[2], st[3]]);
    let index = u16::from_le_bytes([st[4], st[5]]);
    let kind = (rt >> 5) & 3;
    let recipient = rt & 0x1F;
    let iface = (index & 0xFF) as u8;
    let model = s.model;
    let hid_if = model.hid_interface();
    let report_desc: &[u8] = match model {
        Model::DualShock4 => d::ds4::HID_REPORT_DESCRIPTOR,
        _ => &d::HID_REPORT_DESCRIPTOR,
    };
    match (kind, req) {
        // ---- standard
        (0, 0x06) => {
            let (ty, idx) = ((value >> 8) as u8, (value & 0xFF) as u8);
            match ty {
                1 => Some(match model {
                    Model::DualSense => d::DEVICE.to_vec(),
                    Model::Xbox360 => d::x360::DEVICE.to_vec(),
                    Model::DualShock4 => d::ds4::DEVICE.to_vec(),
                }),
                2 => Some(match model {
                    Model::DualSense => d::configuration(),
                    Model::Xbox360 => d::x360::configuration(),
                    Model::DualShock4 => d::ds4::configuration(),
                }),
                3 if idx == 3 => Some(d::utf16_string(&format!("{SERIAL_PREFIX}{}", s.devnum))),
                3 => match model {
                    Model::DualSense => d::string(idx),
                    Model::Xbox360 => d::x360::string(idx),
                    Model::DualShock4 => d::ds4::string(idx),
                },
                0x21 if recipient == 1 && Some(iface) == hid_if => {
                    Some(d::hid_descriptor(report_desc.len() as u16).to_vec())
                }
                0x22 if recipient == 1 && Some(iface) == hid_if => Some(report_desc.to_vec()),
                _ => None,
            }
        }
        (0, 0x00) => Some(vec![0, 0]),
        (0, 0x01) | (0, 0x03) => Some(Vec::new()),
        (0, 0x08) => Some(vec![1]),
        (0, 0x09) => {
            s.ctl.lock().alt = [0; 4];
            reset_streams(s, true, true);
            Some(Vec::new())
        }
        (0, 0x0A) => Some(vec![s
            .ctl
            .lock()
            .alt
            .get(iface as usize)
            .copied()
            .unwrap_or(0)]),
        (0, 0x0B) => {
            let alt = value as u8;
            if iface as usize >= 4 {
                return None;
            }
            let was = std::mem::replace(&mut s.ctl.lock().alt[iface as usize], alt);
            if was != alt {
                match iface {
                    d::IF_RENDER => {
                        log::info!(
                            "virtual DualSense: game audio {}",
                            if alt != 0 { "opened" } else { "closed" }
                        );
                        reset_streams(s, true, false);
                    }
                    d::IF_CAPTURE => reset_streams(s, false, true),
                    _ => {}
                }
            }
            Some(Vec::new())
        }
        // ---- HID class
        (1, _) if recipient == 1 && Some(iface) == hid_if => hid_request(s, req, value, &u.data),
        // ---- audio class, feature units
        (1, 0x01 | 0x81..=0x84)
            if model == Model::DualSense && recipient == 1 && iface == d::IF_AUDIO_CONTROL =>
        {
            audio_request(s, req, (index >> 8) as u8, (value >> 8) as u8, &u.data)
        }
        // ---- audio class, sampling frequency on an endpoint (fixed 48 kHz)
        (1, 0x01) if model == Model::DualSense && recipient == 2 => Some(Vec::new()),
        (1, 0x81..=0x84) if model == Model::DualSense && recipient == 2 => {
            Some(vec![0x80, 0xBB, 0x00])
        }
        // ---- Xbox 360 vendor requests: the driver asks for the
        // controller's capabilities and serial; a zeroed answer of the
        // requested length is what a pad without those features returns.
        (2, _) if model == Model::Xbox360 && rt & 0x80 != 0 => {
            log::debug!(
                "virtual Xbox 360: vendor request {rt:02X} {req:02X} {value:04X} {index:04X} len {}",
                u.length
            );
            Some(vec![0; u.length as usize])
        }
        (2, _) if model == Model::Xbox360 => Some(Vec::new()),
        _ => {
            log::debug!(
                "virtual {}: stalled control {rt:02X} {req:02X} {value:04X} {index:04X}",
                model.name()
            );
            None
        }
    }
}

fn reset_streams(s: &Shared, render: bool, capture: bool) {
    if render {
        s.audio.clear();
    }
    let mut g = s.conn.lock();
    if let Some(c) = g.as_mut() {
        if render {
            c.render.cursor = None;
        }
        if capture {
            c.capture.cursor = None;
        }
    }
}

fn hid_request(s: &Shared, req: u8, value: u16, data: &[u8]) -> Option<Vec<u8>> {
    let (ty, id) = ((value >> 8) as u8, (value & 0xFF) as u8);
    match req {
        // GET_REPORT
        0x01 => match ty {
            1 => {
                let (rep, n) = s.input_now();
                Some(rep[..n].to_vec())
            }
            3 => {
                let a = s.sink.feature(s.model, id);
                if a.is_none() {
                    log::debug!(
                        "virtual {}: unanswered feature report 0x{id:02X}",
                        s.model.name()
                    );
                }
                a
            }
            _ => None,
        },
        // SET_REPORT
        0x09 => match ty {
            2 => {
                (s.sink.on_output)(data);
                Some(Vec::new())
            }
            3 => Some(Vec::new()),
            _ => None,
        },
        0x02 => Some(vec![s.ctl.lock().idle]),
        0x0A => {
            s.ctl.lock().idle = (value >> 8) as u8;
            Some(Vec::new())
        }
        0x03 => Some(vec![1]),
        0x0B => Some(Vec::new()),
        _ => None,
    }
}

fn audio_request(s: &Shared, req: u8, unit: u8, selector: u8, data: &[u8]) -> Option<Vec<u8>> {
    let speaker = match unit {
        d::UNIT_SPEAKER => true,
        d::UNIT_MIC => false,
        _ => return None,
    };
    let (min, max, res) = if speaker { SPEAKER_VOL } else { MIC_VOL };
    let mut c = s.ctl.lock();
    match (selector, req) {
        (1, 0x01) => {
            let m = data.first().copied().unwrap_or(0) != 0;
            if speaker {
                c.speaker_mute = m
            } else {
                c.mic_mute = m
            }
            Some(Vec::new())
        }
        (1, 0x81) => Some(vec![if speaker { c.speaker_mute } else { c.mic_mute } as u8]),
        (2, 0x01) => {
            let v = data
                .get(..2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .unwrap_or(max)
                .clamp(min, max);
            if speaker {
                c.speaker_volume = v
            } else {
                c.mic_volume = v
            }
            Some(Vec::new())
        }
        (2, 0x81) => Some(
            (if speaker {
                c.speaker_volume
            } else {
                c.mic_volume
            })
            .to_le_bytes()
            .to_vec(),
        ),
        (2, 0x82) => Some(min.to_le_bytes().to_vec()),
        (2, 0x83) => Some(max.to_le_bytes().to_vec()),
        (2, 0x84) => Some(res.to_le_bytes().to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_windows_are_back_to_back_and_reanchor_after_a_gap() {
        let epoch = Instant::now();
        let mut c = IsoClock::default();
        let t0 = epoch + Duration::from_millis(5);
        c.admit(1, vec![384; 10], t0, epoch);
        c.admit(2, vec![384; 10], t0, epoch);
        assert_eq!(c.q[0].due, t0 + Duration::from_millis(10));
        assert_eq!(c.q[1].due, t0 + Duration::from_millis(20));
        assert_eq!(c.q[1].frame, 15);
        // Long after the cursor: start from now, not from the stale cursor.
        let late = t0 + Duration::from_millis(100);
        c.q.clear();
        c.admit(3, vec![384; 2], late, epoch);
        assert_eq!(c.q[0].due, late + Duration::from_millis(2));
    }

    #[test]
    fn ring_applies_speaker_gain_only() {
        let r = AudioRing::new();
        let frame: Vec<u8> = [1000i16, 1000, 1000, 1000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        r.push(&frame, 0.5);
        let mut out = [0i16; 4];
        assert_eq!(r.read(&mut out), 1);
        assert_eq!(out, [500, 500, 1000, 1000]);
    }
}
