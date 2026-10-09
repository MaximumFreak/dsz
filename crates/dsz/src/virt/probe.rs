//! `--probe-virtual`: command-line checks of each driver interface.

use super::{hidhide, usbip};
use crate::haptics::game::PcmSource;

/// `--probe-virtual <what>`: exercise one backend from the command line and
/// print what happened. For diagnostics; run from a console.
pub fn probe(what: &str) {
    use std::time::{Duration, Instant};
    println!("HidHide present: {}", hidhide::available());
    match what {
        "hidhide" => match hidhide::snapshot() {
            Ok(s) => {
                println!("cloak active: {} inverse: {}", s.active, s.inverse);
                for a in s.apps {
                    println!("  app: {a}");
                }
                for d in s.devices {
                    println!("  dev: {d}");
                }
            }
            Err(e) => println!("hidhide error: {e}"),
        },
        "install-check" => {
            // `install-check <file>`: what the installer check sees;
            // `install-check <https url>`: download it to temp first.
            let arg = std::env::args().skip_while(|a| a != "install-check").nth(1).unwrap_or_default();
            let path = if arg.starts_with("https://") {
                let to = std::env::temp_dir().join("dsz-download-check.bin");
                match crate::install::download(&arg, &to) {
                    Ok(()) => to,
                    Err(e) => {
                        println!("download: {e}");
                        return;
                    }
                }
            } else {
                std::path::PathBuf::from(&arg)
            };
            let data = std::fs::read(&path).unwrap_or_default();
            let hash = crate::install::sha256(&data).map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>());
            println!("{}: {} bytes, sha256 {:?}", path.display(), data.len(), hash);
            println!("signer: {:?}", crate::install::signer(&path));
        }
        "usbip-x360" => {
            use windows_sys::Win32::UI::Input::XboxController::{
                XInputGetState, XInputSetState, XINPUT_STATE, XINPUT_VIBRATION,
            };
            let state = |i: u32| {
                let mut s: XINPUT_STATE = unsafe { std::mem::zeroed() };
                (unsafe { XInputGetState(i, &mut s) } == 0).then_some(s)
            };
            let before: Vec<bool> = (0..4).map(|i| state(i).is_some()).collect();
            println!("XInput slots in use before: {before:?}");
            let sink = usbip::DsSink {
                on_output: Box::new(|r| {
                    println!("driver output {:02X?} -> {:?}", r, ds_proto::virtual_pad::parse_x360_output(r))
                }),
                mirror: Default::default(),
            };
            let t_create = Instant::now();
            match usbip::UsbPad::create(usbip::Model::Xbox360, sink) {
                Ok(pad) => {
                    println!("attached in {} ms", t_create.elapsed().as_millis());
                    let t = Instant::now();
                    let mut slot = None;
                    while slot.is_none() && t.elapsed() < Duration::from_secs(5) {
                        slot = (0..4u32).find(|&i| !before[i as usize] && state(i).is_some());
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    println!(
                        "XInput slot {slot:?} after {} ms; LED slot {:?}",
                        t.elapsed().as_millis(),
                        pad.xinput_slot()
                    );
                    if let Some(i) = slot {
                        let mut p = ds_proto::virtual_pad::PadState::default();
                        p.buttons.set(ds_proto::input::Button::Cross, true);
                        p.l2 = 200;
                        p.lx = 255;
                        let rep = ds_proto::virtual_pad::xusb_report(&p);
                        let _ = pad.submit(&ds_proto::virtual_pad::x360_usb_input(&rep));
                        std::thread::sleep(Duration::from_millis(50));
                        if let Some(s) = state(i) {
                            let g = s.Gamepad;
                            println!(
                                "XInput reads buttons {:04X} LT {} LX {} (want 1000 200 32767)",
                                g.wButtons, g.bLeftTrigger, g.sThumbLX
                            );
                        }
                        let v = XINPUT_VIBRATION { wLeftMotorSpeed: 0xC000, wRightMotorSpeed: 0x4000 };
                        println!("XInputSetState rc {}", unsafe { XInputSetState(i, &v) });
                        std::thread::sleep(Duration::from_millis(300));
                        let v = XINPUT_VIBRATION { wLeftMotorSpeed: 0, wRightMotorSpeed: 0 };
                        unsafe { XInputSetState(i, &v) };
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    println!("URBs served: {}", pad.urbs());
                }
                Err(e) => println!("virtual Xbox 360 failed: {e}"),
            }
        }
        "usbip-ds4" => {
            let sink = usbip::DsSink {
                on_output: Box::new(|r| {
                    println!("game output {:02X?} -> {:?}", &r[..r.len().min(11)], ds_proto::virtual_pad::parse_ds4_output(r))
                }),
                mirror: Default::default(),
            };
            let t_create = Instant::now();
            match usbip::UsbPad::create(usbip::Model::DualShock4, sink) {
                Ok(pad) => {
                    println!("attached in {} ms", t_create.elapsed().as_millis());
                    let mut p = ds_proto::virtual_pad::PadState::default();
                    p.buttons.set(ds_proto::input::Button::Cross, true);
                    let rep = ds_proto::virtual_pad::ds4_usb_input(&p, None, false, 1);
                    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let feeder = {
                        let stop = stop.clone();
                        let pad = &pad;
                        std::thread::scope(|sc| {
                            let feeding = stop.clone();
                            sc.spawn(move || {
                                while !feeding.load(std::sync::atomic::Ordering::Relaxed) {
                                    let _ = pad.submit(&rep);
                                    std::thread::sleep(Duration::from_millis(4));
                                }
                            });
                            // Read the pad back the way a game would.
                            std::thread::sleep(Duration::from_millis(1500));
                            let mut guid = unsafe { std::mem::zeroed() };
                            unsafe {
                                windows_sys::Win32::Devices::HumanInterfaceDevice::HidD_GetHidGuid(&mut guid)
                            };
                            let mut seen = 0;
                            for path in super::win::interface_paths(&guid) {
                                if !path.to_ascii_lowercase().contains("pid_05c4") {
                                    continue;
                                }
                                seen += 1;
                                let info = crate::hid::DeviceInfo {
                                    path: path.clone(),
                                    vid: 0x054C,
                                    pid: 0x05C4,
                                    input_len: 64,
                                    output_len: 32,
                                    feature_len: 64,
                                    usage_page: 1,
                                    usage: 5,
                                    serial: String::new(),
                                    product: String::new(),
                                };
                                match crate::hid::HidDevice::open(&info) {
                                    Ok(dev) => {
                                        let mut rd = dev.reader().expect("reader");
                                        match rd.read(1000) {
                                            Ok(Some(b)) => println!(
                                                "HID read back {} bytes: {:02X?} (Cross {})",
                                                b.len(),
                                                &b[..10.min(b.len())],
                                                b.get(5).map(|v| v & 0x20 != 0).unwrap_or(false)
                                            ),
                                            other => println!("HID read: {other:?}"),
                                        }
                                    }
                                    Err(e) => println!("open {path}: {e}"),
                                }
                            }
                            println!("DualShock 4 HID nodes: {seen}");
                            stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        })
                    };
                    let _ = feeder;
                    println!("URBs served: {}", pad.urbs());
                }
                Err(e) => println!("virtual DualShock 4 failed: {e}"),
            }
        }
        "usbip-ds" => {
            println!(
                "usbip-win2 present: {} driver version {:?} blocked: {:?}",
                usbip::available(),
                usbip::driver_version(),
                usbip::blocked()
            );
            let sink = usbip::DsSink {
                on_output: Box::new(|r| {
                    if let Some(o) = ds_proto::virtual_pad::parse_ds_usb_output(r) {
                        println!("game output ({} B): {o:?}", r.len());
                    }
                }),
                mirror: Default::default(),
            };
            let t_create = Instant::now();
            match usbip::UsbPad::create(usbip::Model::DualSense, sink) {
                Ok(ds) => {
                    println!("attached in {} ms", t_create.elapsed().as_millis());
                    // Read the virtual pad back the way a game would.
                    let reader = std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_secs(2));
                        let found = crate::hid::enumerate_with(true);
                        println!("virtual HID nodes seen by Windows: {}", found.len());
                        println!(
                            "physical controllers the app would open: {}",
                            crate::hid::enumerate().len()
                        );
                        let Some(info) = found.first() else { return };
                        println!(
                            "  {} input {} output {} feature {}",
                            info.product, info.input_len, info.output_len, info.feature_len
                        );
                        let Ok(dev) = crate::hid::HidDevice::open(info) else {
                            println!("  open failed");
                            return;
                        };
                        let Ok(mut r) = dev.reader() else { return };
                        let (mut n, mut cross, mut t) = (0u32, 0u32, Instant::now());
                        let mut prev = false;
                        while t.elapsed() < Duration::from_secs(3) {
                            if let Ok(Some(b)) = r.read(100) {
                                n += 1;
                                let down = b.len() > 8 && b[8] & 0x20 != 0;
                                if down != prev {
                                    cross += 1;
                                    prev = down;
                                }
                            }
                        }
                        println!(
                            "  read {n} reports in 3 s ({} Hz), Cross changed {cross} times",
                            n / 3
                        );
                        t = Instant::now();
                        let _ = t;
                        match dev.get_feature(0x20, 64) {
                            Ok(f) => println!(
                                "  firmware feature 0x20: {} bytes, starts {:02X?}",
                                f.len(),
                                &f[..8.min(f.len())]
                            ),
                            Err(e) => println!("  feature 0x20 failed: {e}"),
                        }
                    });
                    let secs = secs_arg(14);
                    let audio = ds.audio();
                    let tone = play_tone(5, secs.saturating_sub(7).max(3));
                    let t0 = Instant::now();
                    let mut pcm = vec![0i16; 4 * 1024];
                    let (mut frames, mut peak) = (0usize, [0i32; 4]);
                    let mut first: Option<(Instant, u64)> = None;
                    let mut last = (Instant::now(), 0u64);
                    let mut seq = 0u8;
                    while t0.elapsed() < Duration::from_secs(secs) {
                        let mut pad = ds_proto::virtual_pad::PadState::default();
                        if (t0.elapsed().as_millis() / 500).is_multiple_of(2) {
                            pad.buttons.set(ds_proto::input::Button::Cross, true);
                        }
                        let rep = ds_proto::virtual_pad::ds_usb_input(&pad, None, false, seq);
                        seq = seq.wrapping_add(1);
                        if let Err(e) = ds.submit(&rep) {
                            println!("submit error: {e}");
                            break;
                        }
                        let n = audio.read(&mut pcm);
                        if n > 0 {
                            let total = audio.frames_in.load(std::sync::atomic::Ordering::Relaxed);
                            if first.is_none() {
                                first = Some((Instant::now(), total));
                            }
                            last = (Instant::now(), total);
                        }
                        frames += n;
                        for f in pcm[..n * 4].as_chunks::<4>().0 {
                            for c in 0..4 {
                                peak[c] = peak[c].max((f[c] as i32).abs());
                            }
                        }
                        std::thread::sleep(Duration::from_millis(4));
                    }
                    let _ = tone.join();
                    let _ = reader.join();
                    println!(
                        "alive {} streaming {} urbs {}",
                        ds.alive(),
                        ds.streaming(),
                        ds.urbs()
                    );
                    println!("audio frames read: {frames}, channel peaks {peak:?}");
                    if let Some((t, f)) = first {
                        let dt = last.0.duration_since(t).as_secs_f64();
                        if dt > 0.5 {
                            println!(
                                "game audio rate: {:.0} frames/s (48000 expected)",
                                (last.1 - f) as f64 / dt
                            );
                        }
                    }
                }
                Err(e) => println!("usbip-win2 DualSense failed: {e}"),
            }
        }
        "game-output" => {
            // Write output reports to the app's virtual DualSense like a game.
            let Some(info) = crate::hid::enumerate_with(true).into_iter().next() else {
                println!(
                    "no virtual DualSense is attached (start the app with a DualSense profile)"
                );
                return;
            };
            let Ok(dev) = crate::hid::HidDevice::open(&info) else {
                println!("open failed");
                return;
            };
            let Ok(mut w) = dev.writer() else { return };
            let mut send = |r: [u8; 48], what: &str| match w.write(&r, 500) {
                Ok(()) => println!("{what}"),
                Err(e) => println!("{what}: write failed: {e}"),
            };
            let mut r = [0u8; 48];
            r[0] = 0x02;
            r[1] = 0x01 | 0x02; // rumble
            r[3] = 160;
            r[4] = 255;
            send(r, "rumble: heavy 255, light 160 for 1.5 s");
            std::thread::sleep(Duration::from_millis(1500));
            r[3] = 0;
            r[4] = 0;
            send(r, "rumble off");
            std::thread::sleep(Duration::from_millis(500));
            let mut t = [0u8; 48];
            t[0] = 0x02;
            t[1] = 0x04; // right trigger effect
            t[11] = 0x01; // continuous resistance
            t[12] = 0x20; // from 12% of travel
            t[13] = 0xFF; // full force
            send(t, "right trigger: full resistance for 5 s (press R2)");
            std::thread::sleep(Duration::from_secs(5));
            t[11] = 0x05; // off
            t[12] = 0;
            t[13] = 0;
            send(t, "right trigger off");
        }
        "dsdetect" => {
            // Scan every running program (no cache) and list DualSense games.
            let mut seen = std::collections::HashSet::new();
            let t0 = Instant::now();
            let mut n = 0;
            for (pid, name) in crate::platform::process_list() {
                let Some(path) = crate::platform::process_path(pid) else {
                    continue;
                };
                if !seen.insert(path.clone()) {
                    continue;
                }
                n += 1;
                if let Some(why) = crate::dsdetect::scan(&path) {
                    println!("DUALSENSE  {name}  ({why})  {}", path.display());
                }
            }
            if let Some(p) = std::env::args().skip_while(|a| a != "--path").nth(1) {
                println!("{p}: {:?}", crate::dsdetect::scan(std::path::Path::new(&p)));
            }
            println!("scanned {n} programs in {} ms", t0.elapsed().as_millis());
        }
        "tone" => {
            let _ = play_tone(0, secs_arg(6)).join();
        }
        "default-audio" => {
            use cpal::traits::{DeviceTrait, HostTrait};
            let host = cpal::default_host();
            let name = |d: Option<cpal::Device>| d.and_then(|d| d.name().ok()).unwrap_or_default();
            println!("default output: {}", name(host.default_output_device()));
            println!("default input: {}", name(host.default_input_device()));
            super::audio_default::describe();
        }
        _ => println!(
            "probe what? hidhide | usbip-x360 | usbip-ds4 | usbip-ds | tone | default-audio"
        ),
    }
}

fn secs_arg(default: u64) -> u64 {
    std::env::args()
        .skip_while(|a| a != "--secs")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// After `delay` seconds, play a 150 Hz tone into the controller's audio
/// endpoint the way a game would: 4 channels, each at a different level.
fn play_tone(delay: u64, secs: u64) -> std::thread::JoinHandle<Option<()>> {
    use std::time::Duration;
    std::thread::spawn(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        std::thread::sleep(Duration::from_secs(delay));
        let host = cpal::default_host();
        let dev = host.output_devices().ok()?.find(|d| {
            let n = d.name().unwrap_or_default();
            println!("output device: {n}");
            n.contains("Wireless Controller") || n.contains("DualSense")
        })?;
        let cfg = cpal::StreamConfig {
            channels: 4,
            sample_rate: cpal::SampleRate(48_000),
            buffer_size: cpal::BufferSize::Default,
        };
        let mut t = 0f32;
        let s = dev
            .build_output_stream(
                &cfg,
                move |d: &mut [f32], _| {
                    for f in d.chunks_mut(4) {
                        let v = (t * 2.0 * std::f32::consts::PI * 150.0 / 48_000.0).sin();
                        f[0] = v * 0.1;
                        f[1] = v * 0.2;
                        f[2] = v * 0.5;
                        f[3] = v * 0.9;
                        t += 1.0;
                    }
                },
                |e| println!("tone stream error: {e}"),
                None,
            )
            .map_err(|e| println!("tone stream failed: {e}"))
            .ok()?;
        s.play().ok()?;
        println!("playing test tone on {}", dev.name().unwrap_or_default());
        std::thread::sleep(Duration::from_secs(secs));
        Some(())
    })
}
