//! UDP mod API (default `127.0.0.1:6969`) for game mods that drive the
//! adaptive triggers and lights.
//!
//! The protocol is the one existing DualSense game mods speak, built from
//! what those mods send and read (the mods reviewed are listed in the
//! README): instruction types, trigger modes and their parameters, and the
//! status reply.
//!
//! Packet: `{"instructions":[{"type":1,"parameters":[0,2,21,2,6]}]}`
//! (keys are case-insensitive; `type` may be the number or the name).

use std::net::UdpSocket;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ds_proto::output::player_pattern;
use ds_proto::TriggerEffect;
use serde_json::{json, Value};

use crate::device::Device;
use crate::engine::Engine;

fn get_ci<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

fn num(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::Bool(b) => Some(*b as i64),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64).or(
            match s.trim().to_ascii_lowercase().as_str() {
                "true" => Some(1),
                "false" => Some(0),
                _ => None,
            },
        ),
        _ => None,
    }
}

/// The mods' instruction types 0..=7, then this app's extensions at 100+.
const TYPES: [&str; 8] = [
    "GetDSXStatus",
    "TriggerUpdate",
    "RGBUpdate",
    "PlayerLED",
    "TriggerThreshold",
    "MicLED",
    "PlayerLEDNewRevision",
    "ResetToUserSettings",
];

const EXT: [(&str, usize); 2] = [("HapticPulse", 100), ("Rumble", 101)];

fn kind(v: &Value) -> Option<usize> {
    match v {
        Value::String(s) => TYPES
            .iter()
            .position(|t| t.eq_ignore_ascii_case(s.trim()))
            .or_else(|| {
                EXT.iter()
                    .find(|e| e.0.eq_ignore_ascii_case(s.trim()))
                    .map(|e| e.1)
            })
            .or_else(|| s.trim().parse().ok()),
        other => num(other).map(|n| n as usize),
    }
}

fn targets(engine: &Engine, index: i64) -> Vec<Arc<Device>> {
    let list = engine.device_list();
    if index < 0 {
        list
    } else {
        list.into_iter().nth(index as usize).into_iter().collect()
    }
}

/// What one instruction asks of a controller.
#[derive(Clone, Debug, PartialEq)]
enum Change {
    Trigger { left: bool, effect: TriggerEffect },
    Rgb([u8; 3]),
    PlayerLeds(u8),
    /// The output report's mute LED value: 0 off, 1 on, 2 pulse.
    MicLed(u8),
    Reset,
    Pulse(crate::device::HapticPulse),
    Rumble { heavy: u8, light: u8, ms: u64 },
    /// Understood, but nothing to do on a physical controller.
    Nothing,
}

/// Instruction type and parameters (after the controller index) to a change,
/// or `None` for one this app doesn't know.
fn change(ty: usize, p: &[i64]) -> Option<Change> {
    let at = |i: usize| p.get(i).copied();
    Some(match ty {
        0 => Change::Nothing, // status request: the reply is sent for every packet
        // [side 1 left / 2 right, mode, mode parameters...]
        1 => {
            let left = match at(0)? {
                1 => true,
                2 => false,
                _ => return None,
            };
            let params: Vec<u8> = p.iter().skip(2).map(|&x| x.clamp(0, 255) as u8).collect();
            let effect = TriggerEffect::from_mod_legacy(at(1)?, &params)?;
            Change::Trigger { left, effect }
        }
        // [r, g, b, brightness (optional)]
        2 => {
            let a = at(3).unwrap_or(255).clamp(0, 255) as u32;
            let c = |i: usize| ((at(i).unwrap_or(0).clamp(0, 255) as u32 * a) / 255) as u8;
            Change::Rgb([c(0), c(1), c(2)])
        }
        // [five lights on or off]
        3 => {
            let mut mask = 0u8;
            for i in 0..5 {
                if at(i).unwrap_or(0) != 0 {
                    mask |= 1 << i;
                }
            }
            Change::PlayerLeds(mask)
        }
        // [side, threshold]: only a virtual pad has a trigger threshold.
        4 => Change::Nothing,
        // [0 on, 1 pulse, 2 off]
        5 => Change::MicLed(match at(0).unwrap_or(2) {
            0 => 1,
            1 => 2,
            _ => 0,
        }),
        // [0..=4 player 1..5, 5 all off]
        6 => {
            let n = at(0).unwrap_or(5);
            Change::PlayerLeds(if (0..5).contains(&n) {
                player_pattern(n as u8 + 1)
            } else {
                0
            })
        }
        7 => Change::Reset,
        // Extension: [left 0-100, right 0-100, frequency Hz, ms]
        100 => Change::Pulse(crate::device::HapticPulse {
            left: at(0).unwrap_or(80).clamp(0, 100) as f32 / 100.0,
            right: at(1).unwrap_or(80).clamp(0, 100) as f32 / 100.0,
            freq: at(2).unwrap_or(160).clamp(20, 1200) as f32,
            ms: at(3).unwrap_or(120).clamp(5, 5000) as f32,
        }),
        // Extension: [heavy 0-255, light 0-255, ms]
        101 => Change::Rumble {
            heavy: at(0).unwrap_or(0).clamp(0, 255) as u8,
            light: at(1).unwrap_or(0).clamp(0, 255) as u8,
            ms: at(2).unwrap_or(250).clamp(10, 10_000) as u64,
        },
        _ => return None,
    })
}

fn apply(engine: &Engine, ty: usize, p: &[i64]) {
    let idx = p.first().copied().unwrap_or(-1);
    let rest = p.get(1..).unwrap_or(&[]);
    let Some(change) = change(ty, rest) else {
        return;
    };
    if change == Change::Nothing {
        return;
    }
    for d in targets(engine, idx) {
        let mut o = d.overrides.lock();
        o.last_update = Some(Instant::now());
        match &change {
            Change::Trigger { left, effect } => {
                let bytes = Some(effect.encode());
                if *left {
                    o.left_trigger = bytes;
                } else {
                    o.right_trigger = bytes;
                }
            }
            Change::Rgb(c) => o.rgb = Some(*c),
            Change::PlayerLeds(m) => o.player_leds = Some(*m),
            Change::MicLed(m) => o.mic_led = Some(*m),
            Change::Reset => *o = Default::default(),
            Change::Pulse(p) => d.pulse(*p),
            Change::Rumble { heavy, light, ms } => d.test_rumble(*heavy, *light, *ms),
            Change::Nothing => {}
        }
        drop(o);
        d.kick();
    }
}

fn response(engine: &Engine, first_index: i64) -> Vec<u8> {
    let list = engine.device_list();
    let pick = if first_index >= 0 {
        list.get(first_index as usize).cloned()
    } else {
        list.first().cloned()
    };
    let battery = |d: &Device| d.live.lock().input.battery_percent as i64;
    let devices: Vec<Value> = list
        .iter()
        .enumerate()
        .map(|(i, d)| {
            json!({
                "Index": i,
                "MacAddress": d.mac.lock().clone(),
                "DeviceType": if d.model.is_edge() { 1 } else { 0 },
                "ConnectionType": if d.is_bluetooth() { 1 } else { 0 },
                "BatteryLevel": battery(d),
                "IsSupportAT": true,
                "IsSupportLightBar": true,
                "IsSupportPlayerLED": true,
                "IsSupportMicLED": true,
                "InputHz": d.stats.input_hz.load(Ordering::Relaxed),
                "StreamPackets": d.stats.stream_packets.load(Ordering::Relaxed),
                "StreamHz": d.stats.stream_hz.load(Ordering::Relaxed),
                "ControlWrites": d.stats.writes.load(Ordering::Relaxed),
                "WriteErrors": d.stats.write_errors.load(Ordering::Relaxed),
                "CrcErrors": d.stats.crc_errors.load(Ordering::Relaxed),
                "HapticsActive": d.pcm_mode.load(Ordering::Relaxed),
            })
        })
        .collect();
    let v = json!({
        "Status": "OK",
        "TimeReceived": humantime_now(),
        "isControllerConnected": pick.is_some(),
        "BatteryLevel": pick.as_ref().map(|d| battery(d)).unwrap_or(0),
        "Devices": devices,
    });
    serde_json::to_vec(&v).unwrap_or_default()
}

/// A packet's instructions as `(type, parameters)`, or `None` if it isn't one.
fn parse(data: &[u8]) -> Option<Vec<(usize, Vec<i64>)>> {
    let text = String::from_utf8_lossy(data);
    let v = serde_json::from_str::<Value>(text.trim_matches(char::from(0)).trim()).ok()?;
    let list = get_ci(&v, "instructions")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|ins| {
                let ty = get_ci(ins, "type").and_then(kind)?;
                let params = get_ci(ins, "parameters")
                    .and_then(|p| p.as_array())
                    .map(|a| a.iter().map(|x| num(x).unwrap_or(0)).collect())
                    .unwrap_or_default();
                Some((ty, params))
            })
            .collect(),
    )
}

/// Applies a packet; returns the controller index its first instruction
/// named (for the reply), or -1.
fn handle(engine: &Engine, data: &[u8]) -> i64 {
    let Some(list) = parse(data) else {
        return -1;
    };
    let mut first = -1;
    for (n, (ty, params)) in list.iter().enumerate() {
        if n == 0 && *ty != 0 {
            first = params.first().copied().unwrap_or(-1);
        }
        apply(engine, *ty, params);
    }
    first
}

pub fn run(engine: Arc<Engine>) {
    let mut bound: Option<(u16, UdpSocket)> = None;
    let mut buf = vec![0u8; 65_536];
    while !engine.shutdown.load(Ordering::Acquire) {
        let (enabled, port) = {
            let s = engine.settings.read();
            (s.udp_enabled, s.udp_port)
        };
        if !enabled {
            if bound.take().is_some() {
                log::info!("UDP mod server stopped");
            }
            let mut st = engine.udp.lock();
            st.listening = false;
            st.error = None;
            drop(st);
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        if bound.as_ref().map(|b| b.0) != Some(port) {
            bound = None;
            match UdpSocket::bind(("127.0.0.1", port)) {
                Ok(s) => {
                    let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
                    log::info!("UDP mod server listening on 127.0.0.1:{port}");
                    let mut st = engine.udp.lock();
                    st.listening = true;
                    st.error = None;
                    bound = Some((port, s));
                }
                Err(e) => {
                    let msg = if e.kind() == std::io::ErrorKind::AddrInUse {
                        format!("Port {port} is in use (is another controller app running?)")
                    } else {
                        e.to_string()
                    };
                    let mut st = engine.udp.lock();
                    if st.error.as_deref() != Some(msg.as_str()) {
                        log::warn!("UDP mod server: {msg}");
                    }
                    st.listening = false;
                    st.error = Some(msg);
                    drop(st);
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }
        let Some((_, sock)) = bound.as_ref() else {
            continue;
        };
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                let first = handle(&engine, &buf[..n]);
                let resp = response(&engine, first);
                let _ = sock.send_to(&resp, from);
                let mut st = engine.udp.lock();
                st.packets += 1;
                st.last_packet = Some(Instant::now());
                st.last_text = String::from_utf8_lossy(&buf[..n.min(300)]).into_owned();
                let _ = from;
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) if e.raw_os_error() == Some(10054) => {} // client went away (ICMP reset)
            Err(e) => {
                log::debug!("UDP recv: {e}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_numbers() {
        assert_eq!(kind(&json!("TriggerUpdate")), Some(1));
        assert_eq!(kind(&json!(2)), Some(2));
        assert_eq!(num(&json!("12")), Some(12));
        assert_eq!(num(&json!(true)), Some(1));
        let v: Value =
            serde_json::from_str(r#"{"Instructions":[{"Type":1,"Parameters":[0,2,22,2,6,8]}]}"#)
                .unwrap();
        assert!(get_ci(&v, "instructions").is_some());
    }

    /// The packet's instructions as changes, with the controller index
    /// checked to be 0 (every reviewed mod drives controller 0).
    fn changes(packet: &str) -> Vec<Option<Change>> {
        parse(packet.as_bytes())
            .expect("packet parses")
            .iter()
            .map(|(ty, p)| {
                if *ty != 0 {
                    assert_eq!(p.first(), Some(&0), "controller index in {packet}");
                }
                change(*ty, p.get(1..).unwrap_or(&[]))
            })
            .collect()
    }

    fn trigger(left: bool, effect: TriggerEffect) -> Option<Change> {
        Some(Change::Trigger { left, effect })
    }

    /// Packets as the reviewed game mods build them (see the README's list),
    /// one test per mod, so a change that breaks one shows which.
    #[test]
    fn mod_tarkov_dsx() {
        // dvize/TarkovDSX, Newtonsoft.Json: Instruction.Resistance,
        // PlayerLEDNewRevision(Three), TriggerThreshold.
        let c = changes(
            r#"{"instructions":[{"type":1,"parameters":[0,2,13,0,8]},{"type":6,"parameters":[0,2]},{"type":4,"parameters":[0,2,0]}]}"#,
        );
        assert_eq!(c[0], trigger(false, TriggerEffect::Feedback { position: 0, strength: 8 }));
        assert_eq!(c[1], Some(Change::PlayerLeds(player_pattern(3))));
        assert_eq!(c[2], Some(Change::Nothing));
    }

    #[test]
    fn mod_dsx_fallout4() {
        // dvize/DSXFallout4, nlohmann::json: every parameter sent as a string.
        // CustomTriggerValue / VibrateResistance (no public definition, so
        // ignored), then AddRGB with brightness.
        let c = changes(
            r#"{"instructions":[{"type":1,"parameters":["0","1","12","9","40","200","0","0","0","0","0"]},{"type":2,"parameters":["0","255","64","0","128"]}]}"#,
        );
        assert_eq!(c[0], None);
        assert_eq!(c[1], Some(Change::Rgb([128, 32, 0])));
    }

    #[test]
    fn mod_dsx_skyrim_ng() {
        // dvize/DSXSkyrim-NG, strings: TriggerMode::Normal with {0,0,0,0}, MicLED Pulse.
        let c = changes(
            r#"{"instructions":[{"type":1,"parameters":["0","1","0","0","0","0","0"]},{"type":5,"parameters":["0","1"]}]}"#,
        );
        assert_eq!(c[0], trigger(true, TriggerEffect::Off));
        assert_eq!(c[1], Some(Change::MicLed(2)));
    }

    #[test]
    fn mod_dualsense4rockstar() {
        // tommargar/DualSense4Rockstar (GTA V, RDR 2): Bow, then TriggerThreshold.
        let c = changes(
            r#"{"instructions":[{"type":1,"parameters":[0,2,14,0,8,2,5]},{"type":4,"parameters":[0,2,120]}]}"#,
        );
        assert_eq!(
            c[0],
            trigger(false, TriggerEffect::Bow { start: 0, end: 8, strength: 2, snap: 5 })
        );
        assert_eq!(c[1], Some(Change::Nothing));
    }

    #[test]
    fn mod_wf2_dsx() {
        // SYSTEMATI0N/WF2_DSX (Wreckfest 2), System.Text.Json: AutomaticGun on
        // the brake, RGB without brightness, PlayerLED with booleans.
        let c = changes(
            r#"{"instructions":[{"type":1,"parameters":[0,1,17,0,6,12]},{"type":2,"parameters":[0,0,30,70]},{"type":3,"parameters":[0,true,false,true,false,true]}]}"#,
        );
        assert_eq!(
            c[0],
            trigger(true, TriggerEffect::Vibration { position: 0, amplitude: 6, frequency: 12 })
        );
        assert_eq!(c[1], Some(Change::Rgb([0, 30, 70])));
        assert_eq!(c[2], Some(Change::PlayerLeds(0b10101)));
    }

    #[test]
    fn mod_forza_dsx_legacy() {
        // cosmii02/ForzaDSXlegacy, Newtonsoft.Json: its test sequence of modes.
        let c = changes(
            r#"{"instructions":[
                {"type":1,"parameters":[0,1,8,10]},
                {"type":1,"parameters":[0,1,12,8,0,101,255,255,0,0,0]},
                {"type":1,"parameters":[0,1,15,0,9,2,4,10]},
                {"type":1,"parameters":[0,1,16,2,7,8]},
                {"type":1,"parameters":[0,1,18,0,9,7,7,10,0]}]}"#,
        );
        assert_eq!(
            c[0],
            trigger(true, TriggerEffect::Vibration { position: 0, amplitude: 8, frequency: 10 })
        );
        assert_eq!(
            c[1],
            trigger(true, TriggerEffect::Custom { mode: 0x26, forces: [0, 101, 255, 255, 0, 0, 0] })
        );
        assert_eq!(
            c[2],
            trigger(
                true,
                TriggerEffect::Galloping { start: 0, end: 9, first_foot: 2, second_foot: 4, frequency: 10 }
            )
        );
        assert_eq!(c[3], trigger(true, TriggerEffect::Weapon { start: 2, end: 7, strength: 8 }));
        assert_eq!(
            c[4],
            trigger(
                true,
                TriggerEffect::Machine { start: 0, end: 9, amp_a: 7, amp_b: 7, frequency: 10, period: 0 }
            )
        );
    }

    #[test]
    fn mod_rbr_adaptive_trigger() {
        // aaronfang/RBR_Adaptive_Trigger (Python json.dumps): AutomaticGun,
        // then ResetToUserSettings when it stops.
        let c = changes(
            r#"{"instructions": [{"type": 1, "parameters": [0, 2, 17, 0, 5, 30]}, {"type": 7, "parameters": [0]}]}"#,
        );
        assert_eq!(
            c[0],
            trigger(false, TriggerEffect::Vibration { position: 0, amplitude: 5, frequency: 30 })
        );
        assert_eq!(c[1], Some(Change::Reset));
    }

    #[test]
    fn mod_race_element() {
        // RiddleTime/Race-Element, System.Text.Json: GetDSXStatus, FEEDBACK
        // and VIBRATION (the newer modes), CustomTriggerValue / VibrateResistanceB
        // (ignored, like every custom mode past PulseAB), then RigidA.
        let c = changes(
            r#"{"instructions":[{"type":0,"parameters":[]},{"type":1,"parameters":[0,1,21,1,5]},{"type":1,"parameters":[0,2,23,0,6,40]},{"type":1,"parameters":[0,1,12,11,60,180,0,0,0,0,0]},{"type":1,"parameters":[0,1,12,2,60,180,0,0,0,0,0]}]}"#,
        );
        assert_eq!(c[0], Some(Change::Nothing));
        assert_eq!(c[1], trigger(true, TriggerEffect::Feedback { position: 1, strength: 5 }));
        assert_eq!(
            c[2],
            trigger(false, TriggerEffect::Vibration { position: 0, amplitude: 6, frequency: 40 })
        );
        assert_eq!(c[3], None);
        assert_eq!(
            c[4],
            trigger(true, TriggerEffect::Custom { mode: 0x21, forces: [60, 180, 0, 0, 0, 0, 0] })
        );
    }

    #[test]
    fn mod_dsxpp() {
        // tpetsas/DSXpp (C++ client library): integer parameters, Resistance
        // then the named presets.
        let c = changes(
            r#"{"instructions":[{"type":1,"parameters":[0,2,13,3,6]},{"type":1,"parameters":[0,1,7]},{"type":1,"parameters":[0,1,1]}]}"#,
        );
        assert_eq!(c[0], trigger(false, TriggerEffect::Feedback { position: 3, strength: 6 }));
        assert_eq!(c[1], trigger(true, TriggerEffect::Preset { preset: ds_proto::TriggerPreset::Rigid }));
        assert_eq!(c[2], trigger(true, TriggerEffect::Preset { preset: ds_proto::TriggerPreset::GameCube }));
    }

    #[test]
    fn mod_godot_dualsensex_support() {
        // Tio-Henry/DualSenseX-Support (Godot): GetDSXStatus with no parameters,
        // lightbar_led with brightness, player LEDs all off, mic LED off.
        let c = changes(
            r#"{"instructions":[{"type":0}]}"#,
        );
        assert_eq!(c[0], Some(Change::Nothing));
        let c = changes(
            r#"{"instructions":[{"type":2,"parameters":[0,255,255,255,255]},{"type":6,"parameters":[0,5]},{"type":5,"parameters":[0,2]}]}"#,
        );
        assert_eq!(c[0], Some(Change::Rgb([255, 255, 255])));
        assert_eq!(c[1], Some(Change::PlayerLeds(0)));
        assert_eq!(c[2], Some(Change::MicLed(0)));
    }

    #[test]
    fn every_mod_trigger_mode_is_understood() {
        // TriggerMode 0..=26 as the mods define them; nothing past 26.
        for mode in 0..=26 {
            assert!(
                TriggerEffect::from_mod_legacy(mode, &[0, 9, 5, 5, 5, 5, 5, 5, 5, 5, 5]).is_some(),
                "mode {mode}"
            );
        }
        assert!(TriggerEffect::from_mod_legacy(27, &[]).is_none());
        // Per-zone vibration: frequency first, then ten amplitudes.
        assert_eq!(
            TriggerEffect::from_mod_legacy(26, &[30, 1, 2, 3, 4, 5, 6, 7, 8, 0, 0]),
            Some(TriggerEffect::MultiVibration { amplitudes: [1, 2, 3, 4, 5, 6, 7, 8, 0, 0], frequency: 30 })
        );
    }
}

/// ISO-8601 UTC timestamp without a time crate.
pub fn humantime_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
