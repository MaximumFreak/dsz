//! Motion server for emulators: the cemuhook ("DSU") protocol.
//!
//! Dolphin (Wii Remote motion), Cemu (Wii U), and Ryujinx and the
//! yuzu-family Switch emulators read a controller's gyro and accelerometer
//! from a DSU server on UDP 127.0.0.1:26760. A client asks which of four
//! slots hold a controller, then registers for data (all slots, one slot,
//! or one MAC) and repeats that every second or so; each controller report
//! is sent to every client registered for it in the last five seconds.
//!
//! Protocol: <https://v1993.github.io/cemuhook-protocol/> (version 1001).
//! Motion axes follow what DSU clients expect from a DualShock 4, as
//! DS4Windows and evdevhook2 send them: accelerometer in g as (-x, -y, -z)
//! of the controller's axes, gyro in deg/s as pitch +x, yaw -y, roll -z.
//! DualSense and DualShock 4 share those axes. The gyro is DSZ's
//! bias-corrected one; the sticks and buttons are the controller's own,
//! before any profile mapping.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ds_proto::input::{BatteryStatus, Button};
use ds_proto::{Connection, InputState};
use parking_lot::Mutex;

use crate::device::Device;
use crate::engine::Engine;

pub const PORT: u16 = 26760;
const VERSION: u16 = 1001;
const MSG_VERSION: u32 = 0x10_0000;
const MSG_INFO: u32 = 0x10_0001;
const MSG_DATA: u32 = 0x10_0002;
const SLOTS: usize = 4;
/// A client that stops asking is dropped after this.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
const ACC_PER_G: f32 = 8192.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Off,
    Listening { clients: usize },
    Failed(String),
}

#[derive(Clone, Copy)]
struct Client {
    addr: SocketAddr,
    all: Option<Instant>,
    slot: [Option<Instant>; SLOTS],
    mac: Option<([u8; 6], Instant)>,
    counter: u32,
}

impl Client {
    fn wants(&self, slot: usize, mac: [u8; 6], now: Instant) -> bool {
        let fresh = |t: Option<Instant>| t.is_some_and(|t| now.duration_since(t) < CLIENT_TIMEOUT);
        fresh(self.all) || fresh(self.slot[slot]) || self.mac.is_some_and(|(m, t)| m == mac && fresh(Some(t)))
    }

    fn last(&self) -> Option<Instant> {
        let mut t = self.all;
        for s in self.slot.iter().chain(std::iter::once(&self.mac.map(|m| m.1))) {
            t = t.max(*s);
        }
        t
    }
}

/// A controller in a slot, with its motion clock.
struct Pad {
    id: u64,
    /// Sensor clock in 1/3 µs, unwrapped.
    ticks: u64,
    last_ts: Option<u32>,
}

pub struct Server {
    socket: Mutex<Option<Arc<UdpSocket>>>,
    clients: Mutex<Vec<Client>>,
    /// Quick check on the report path: anyone listening?
    any_client: AtomicBool,
    pads: Mutex<[Option<Pad>; SLOTS]>,
    status: Mutex<Status>,
    id: u32,
}

impl Default for Server {
    fn default() -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Server {
            socket: Mutex::new(None),
            clients: Mutex::new(Vec::new()),
            any_client: AtomicBool::new(false),
            pads: Mutex::new(Default::default()),
            status: Mutex::new(Status::Off),
            id: t.subsec_nanos() ^ std::process::id(),
        }
    }
}

impl Server {
    pub fn status(&self) -> Status {
        self.status.lock().clone()
    }

    /// The slot of controller `id`, given one if it has none.
    fn slot_of(&self, id: u64) -> Option<usize> {
        let mut pads = self.pads.lock();
        if let Some(i) = pads.iter().position(|p| p.as_ref().is_some_and(|p| p.id == id)) {
            return Some(i);
        }
        let i = pads.iter().position(|p| p.is_none())?;
        pads[i] = Some(Pad { id, ticks: 0, last_ts: None });
        Some(i)
    }

    /// Send one controller report to the clients that want it. Called for
    /// every input report; returns at once when no one is listening.
    pub fn publish(&self, device: &Device, s: &InputState, gyro_dps: [f32; 3]) {
        if !self.any_client.load(Ordering::Acquire) {
            return;
        }
        let Some(sock) = self.socket.lock().clone() else { return };
        let Some(slot) = self.slot_of(device.id) else { return };
        let micros = {
            let mut pads = self.pads.lock();
            let Some(p) = pads[slot].as_mut() else { return };
            if s.motion_valid {
                if let Some(t) = p.last_ts {
                    p.ticks += s.sensor_ts.wrapping_sub(t) as u64;
                }
                p.last_ts = Some(s.sensor_ts);
            }
            p.ticks / 3
        };
        let mac = mac_bytes(&device.mac.lock());
        let now = Instant::now();
        let mut clients = self.clients.lock();
        for c in clients.iter_mut().filter(|c| c.wants(slot, mac, now)) {
            let p = data_packet(self.id, slot, device, mac, s, gyro_dps, micros, c.counter);
            c.counter = c.counter.wrapping_add(1);
            let _ = sock.send_to(&p, c.addr);
        }
    }

    fn handle(&self, engine: &Engine, sock: &UdpSocket, msg: &[u8], from: SocketAddr) {
        let Some((ty, body)) = parse(msg) else { return };
        match ty {
            MSG_VERSION => {
                let _ = sock.send_to(&packet(self.id, MSG_VERSION, &VERSION.to_le_bytes()), from);
            }
            MSG_INFO => {
                let Some(n) = body.get(..4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])) else {
                    return;
                };
                for &slot in body[4..].iter().take(n.clamp(0, SLOTS as i32) as usize) {
                    let slot = slot as usize;
                    if slot >= SLOTS {
                        continue;
                    }
                    let mut p = [0u8; 12];
                    p[0] = slot as u8;
                    if let Some(d) = self.device_in(engine, slot) {
                        p[..11].copy_from_slice(&shared(slot, &d, mac_bytes(&d.mac.lock()), &d.live.lock().input));
                    }
                    let _ = sock.send_to(&packet(self.id, MSG_INFO, &p), from);
                }
            }
            MSG_DATA => {
                let (Some(&flags), Some(&slot)) = (body.first(), body.get(1)) else { return };
                let now = Instant::now();
                let mut clients = self.clients.lock();
                let i = match clients.iter().position(|c| c.addr == from) {
                    Some(i) => i,
                    None => {
                        log::info!("DSU client {from} connected");
                        clients.push(Client { addr: from, all: None, slot: [None; SLOTS], mac: None, counter: 0 });
                        clients.len() - 1
                    }
                };
                let c = &mut clients[i];
                if flags == 0 {
                    c.all = Some(now);
                }
                if flags & 1 != 0 && (slot as usize) < SLOTS {
                    c.slot[slot as usize] = Some(now);
                }
                if flags & 2 != 0 {
                    if let Some(m) = body.get(2..8) {
                        c.mac = Some((m.try_into().unwrap(), now));
                    }
                }
                self.any_client.store(true, Ordering::Release);
            }
            _ => {}
        }
    }

    /// The connected controller in `slot`.
    fn device_in(&self, engine: &Engine, slot: usize) -> Option<Arc<Device>> {
        let id = self.pads.lock()[slot].as_ref()?.id;
        engine.device_list().into_iter().find(|d| d.id == id)
    }

    /// Give slots to new controllers and free those of gone ones; drop
    /// clients that stopped asking.
    fn tidy(&self, engine: &Engine) {
        let devices = engine.device_list();
        {
            let mut pads = self.pads.lock();
            for p in pads.iter_mut() {
                if p.as_ref().is_some_and(|p| !devices.iter().any(|d| d.id == p.id)) {
                    *p = None;
                }
            }
        }
        for d in devices.iter().filter(|d| d.has_motion) {
            self.slot_of(d.id);
        }
        let now = Instant::now();
        let mut clients = self.clients.lock();
        clients.retain(|c| {
            let keep = c.last().is_some_and(|t| now.duration_since(t) < CLIENT_TIMEOUT);
            if !keep {
                log::info!("DSU client {} left", c.addr);
            }
            keep
        });
        self.any_client.store(!clients.is_empty(), Ordering::Release);
        *self.status.lock() = Status::Listening { clients: clients.len() };
    }

    fn stop(&self) {
        *self.socket.lock() = None;
        self.clients.lock().clear();
        self.any_client.store(false, Ordering::Release);
    }
}

/// The server thread: listens while the setting is on.
pub fn run(engine: Arc<Engine>) {
    let server = &engine.dsu;
    let mut buf = [0u8; 1024];
    let mut tidied = Instant::now();
    while !engine.shutdown.load(Ordering::Acquire) {
        if !engine.settings.read().dsu_enabled {
            if server.socket.lock().take().is_some() {
                server.stop();
                log::info!("DSU motion server stopped");
            }
            *server.status.lock() = Status::Off;
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        let sock = server.socket.lock().clone();
        let sock = match sock {
            Some(s) => s,
            None => match UdpSocket::bind(("127.0.0.1", PORT)) {
                Ok(s) => {
                    let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
                    log::info!("DSU motion server listening on 127.0.0.1:{PORT}");
                    let s = Arc::new(s);
                    *server.socket.lock() = Some(s.clone());
                    *server.status.lock() = Status::Listening { clients: 0 };
                    s
                }
                Err(e) => {
                    let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                        format!("Port {PORT} is in use (is DS4Windows or another motion server running?)")
                    } else {
                        e.to_string()
                    };
                    let mut st = server.status.lock();
                    if *st != Status::Failed(msg.clone()) {
                        log::warn!("DSU motion server: {msg}");
                    }
                    *st = Status::Failed(msg);
                    drop(st);
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            },
        };
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => server.handle(&engine, &sock, &buf[..n], from),
            // Windows reports a client's closed port on the next receive.
            Err(_) => {}
        }
        if tidied.elapsed() >= Duration::from_secs(1) {
            server.tidy(&engine);
            tidied = Instant::now();
        }
    }
    server.stop();
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// A server packet: header, message type, payload, CRC.
fn packet(id: u32, ty: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(20 + payload.len());
    p.extend_from_slice(b"DSUS");
    p.extend_from_slice(&VERSION.to_le_bytes());
    p.extend_from_slice(&((4 + payload.len()) as u16).to_le_bytes());
    p.extend_from_slice(&[0; 4]);
    p.extend_from_slice(&id.to_le_bytes());
    p.extend_from_slice(&ty.to_le_bytes());
    p.extend_from_slice(payload);
    let crc = crc32(&p);
    p[8..12].copy_from_slice(&crc.to_le_bytes());
    p
}

/// A client packet's message type and payload, if it is well formed.
fn parse(msg: &[u8]) -> Option<(u32, &[u8])> {
    if msg.len() < 20 || &msg[..4] != b"DSUC" {
        return None;
    }
    if u16::from_le_bytes([msg[4], msg[5]]) > VERSION {
        return None;
    }
    let len = u16::from_le_bytes([msg[6], msg[7]]) as usize;
    let msg = msg.get(..16 + len)?;
    let crc = u32::from_le_bytes(msg[8..12].try_into().unwrap());
    let mut copy = msg.to_vec();
    copy[8..12].fill(0);
    if crc32(&copy) != crc {
        return None;
    }
    let ty = u32::from_le_bytes(msg[16..20].try_into().unwrap());
    Some((ty, &msg[20..]))
}

/// A colon-separated MAC (six hex pairs) as bytes; zeros if it isn't one.
fn mac_bytes(s: &str) -> [u8; 6] {
    let mut out = [0u8; 6];
    let parts: Vec<&str> = s.split([':', '-']).collect();
    if parts.len() == 6 {
        for (o, p) in out.iter_mut().zip(parts) {
            *o = u8::from_str_radix(p, 16).unwrap_or(0);
        }
    }
    out
}

/// The 11 bytes that start info and data answers.
fn shared(slot: usize, d: &Device, mac: [u8; 6], s: &InputState) -> [u8; 11] {
    let mut b = [0u8; 11];
    b[0] = slot as u8;
    b[1] = 2; // connected
    b[2] = if d.has_motion { 2 } else { 1 };
    b[3] = match d.conn {
        Connection::Usb => 1,
        Connection::Bluetooth => 2,
    };
    b[4..10].copy_from_slice(&mac);
    b[10] = match s.battery_status {
        BatteryStatus::Charging => 0xEE,
        BatteryStatus::Full => 0xEF,
        BatteryStatus::Error => 0,
        BatteryStatus::Discharging => match s.battery_percent {
            0..=10 => 1,
            11..=30 => 2,
            31..=60 => 3,
            61..=90 => 4,
            _ => 5,
        },
    };
    b
}

#[allow(clippy::too_many_arguments)]
fn data_packet(
    id: u32,
    slot: usize,
    d: &Device,
    mac: [u8; 6],
    s: &InputState,
    gyro_dps: [f32; 3],
    micros: u64,
    counter: u32,
) -> Vec<u8> {
    let mut p = [0u8; 80];
    p[..11].copy_from_slice(&shared(slot, d, mac, s));
    p[11] = 1;
    p[12..16].copy_from_slice(&counter.to_le_bytes());
    let has = |b: Button| s.buttons.has(b);
    let bits = |list: &[(Button, u8)]| list.iter().filter(|(b, _)| has(*b)).fold(0u8, |a, (_, m)| a | m);
    p[16] = bits(&[
        (Button::DpadLeft, 0x80),
        (Button::DpadDown, 0x40),
        (Button::DpadRight, 0x20),
        (Button::DpadUp, 0x10),
        (Button::Options, 0x08),
        (Button::R3, 0x04),
        (Button::L3, 0x02),
        (Button::Create, 0x01),
    ]);
    p[17] = bits(&[
        (Button::Triangle, 0x80),
        (Button::Circle, 0x40),
        (Button::Cross, 0x20),
        (Button::Square, 0x10),
        (Button::R1, 0x08),
        (Button::L1, 0x04),
        (Button::R2, 0x02),
        (Button::L2, 0x01),
    ]);
    p[18] = has(Button::Ps) as u8;
    p[19] = has(Button::Touchpad) as u8;
    // DSU sticks grow upward.
    p[20] = s.lx;
    p[21] = 255 - s.ly;
    p[22] = s.rx;
    p[23] = 255 - s.ry;
    let analog = |b: Button| if has(b) { 255 } else { 0 };
    p[24] = analog(Button::DpadLeft);
    p[25] = analog(Button::DpadDown);
    p[26] = analog(Button::DpadRight);
    p[27] = analog(Button::DpadUp);
    p[28] = analog(Button::Triangle);
    p[29] = analog(Button::Circle);
    p[30] = analog(Button::Cross);
    p[31] = analog(Button::Square);
    p[32] = analog(Button::R1);
    p[33] = analog(Button::L1);
    p[34] = s.r2;
    p[35] = s.l2;
    for (k, t) in s.touch.iter().enumerate() {
        let o = 36 + 6 * k;
        p[o] = t.active as u8;
        p[o + 1] = t.id;
        p[o + 2..o + 4].copy_from_slice(&t.x.to_le_bytes());
        p[o + 4..o + 6].copy_from_slice(&t.y.to_le_bytes());
    }
    if s.motion_valid {
        let (accel, gyro) = motion(s, gyro_dps);
        p[48..56].copy_from_slice(&micros.to_le_bytes());
        for (k, v) in accel.iter().chain(gyro.iter()).enumerate() {
            p[56 + 4 * k..60 + 4 * k].copy_from_slice(&v.to_le_bytes());
        }
    }
    packet(id, MSG_DATA, &p)
}

/// DSU accelerometer (g) and gyro (pitch, yaw, roll in deg/s).
fn motion(s: &InputState, gyro_dps: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let a = |k: usize| -(s.accel[k] as f32) / ACC_PER_G;
    ([a(0), a(1), a(2)], [gyro_dps[0], -gyro_dps[1], -gyro_dps[2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_is_the_standard_one() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn packets_round_trip() {
        let mut p = packet(7, MSG_INFO, &[1, 0, 0, 0, 0]);
        assert_eq!(p.len(), 25);
        assert_eq!(&p[..4], b"DSUS");
        assert_eq!(u16::from_le_bytes([p[6], p[7]]), 9, "length counts the type, not the header");
        // As a client would send it.
        p[..4].copy_from_slice(b"DSUC");
        p[8..12].fill(0);
        let crc = crc32(&p);
        p[8..12].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(parse(&p), Some((MSG_INFO, &[1u8, 0, 0, 0, 0][..])));
        p[20] ^= 1;
        assert_eq!(parse(&p), None, "a bad CRC is dropped");
    }

    #[test]
    fn data_packet_is_100_bytes() {
        let d = Device::for_test(Connection::Bluetooth, "");
        let s = InputState { motion_valid: true, ..Default::default() };
        assert_eq!(data_packet(1, 0, &d, [0; 6], &s, [0.0; 3], 0, 0).len(), 100);
    }

    #[test]
    fn flat_controller_reads_minus_one_g_up() {
        // Lying flat, the accelerometer's y reads +1 g (8192).
        let s = InputState { accel: [0, 8192, 0], ..Default::default() };
        let (a, g) = motion(&s, [10.0, 20.0, 30.0]);
        assert_eq!(a, [-0.0, -1.0, -0.0]);
        assert_eq!(g, [10.0, -20.0, -30.0]);
    }

    /// A client on a real localhost socket: asks for slot 0, registers for
    /// all controllers, and gets a report.
    #[test]
    fn serves_a_client() {
        let dir = std::env::temp_dir().join(format!("dsz-dsu-test-{}", std::process::id()));
        let engine = Engine::new(dir.clone());
        let device = Device::for_test(Connection::Bluetooth, "");
        engine.devices.write().push(device.clone());
        let server = Server::default();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        *server.socket.lock() = Some(Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap()));
        let srv = server.socket.lock().clone().unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let from = client.local_addr().unwrap();
        let ask = |ty: u32, body: &[u8]| {
            let mut p = packet(9, ty, body);
            p[..4].copy_from_slice(b"DSUC");
            p[8..12].fill(0);
            let crc = crc32(&p);
            p[8..12].copy_from_slice(&crc.to_le_bytes());
            p
        };
        server.tidy(&engine);
        let mut buf = [0u8; 256];

        server.handle(&engine, &srv, &ask(MSG_INFO, &[1, 0, 0, 0, 0]), from);
        let n = client.recv(&mut buf).unwrap();
        assert_eq!(n, 32);
        assert_eq!((buf[20], buf[21], buf[22], buf[23]), (0, 2, 2, 2), "slot 0: connected, full gyro, Bluetooth");

        assert!(!server.any_client.load(Ordering::Acquire));
        server.handle(&engine, &srv, &ask(MSG_DATA, &[0, 0, 0, 0, 0, 0, 0, 0]), from);
        let s = InputState {
            motion_valid: true,
            full: true,
            accel: [0, 8192, 0],
            ly: 0,
            ..Default::default()
        };
        server.publish(&device, &s, [1.0, 2.0, 3.0]);
        let n = client.recv(&mut buf).unwrap();
        assert_eq!(n, 100);
        let mut b = buf[..n].to_vec();
        b[8..12].fill(0);
        assert_eq!(crc32(&b).to_le_bytes(), buf[8..12], "CRC checks out");
        assert_eq!(buf[16..20], MSG_DATA.to_le_bytes());
        assert_eq!(buf[20 + 21], 255, "stick pushed up reads high");
        let f = |o: usize| f32::from_le_bytes(buf[20 + o..24 + o].try_into().unwrap());
        assert_eq!((f(60), f(68), f(72), f(76)), (-1.0, 1.0, -2.0, -3.0));
        drop(sock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_macs() {
        let mac = ["A0", "5A", "5C", "01", "02", "FF"].join(":");
        assert_eq!(mac_bytes(&mac), [0xA0, 0x5A, 0x5C, 1, 2, 0xFF]);
        assert_eq!(mac_bytes("usb"), [0; 6]);
    }
}
