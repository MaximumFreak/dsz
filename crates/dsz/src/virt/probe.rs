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
        "helper" => {
            // `helper <hold ms> [<hold ms> ...]`: press Guide on the helper
            // Xbox pad the way a mapping does, one press per argument, 3 s
            // apart, through the real companion thread.
            let holds: Vec<u64> = std::env::args()
                .skip_while(|a| a != "helper")
                .skip(1)
                .filter_map(|s| s.parse().ok())
                .collect();
            // Pads numbered from 10, so none shares a serial (and so a
            // HidHide entry) with the running app's.
            usbip::reserve_slots_below(10);
            let dir = std::env::temp_dir().join("dsz-helper-probe");
            let engine = crate::engine::Engine::new(dir);
            {
                let e = engine.clone();
                std::thread::spawn(move || super::companion::run(e));
            }
            let guide = ds_proto::input::Button::Ps.bit();
            let t0 = Instant::now();
            for hold in holds {
                println!("press {hold} ms at {} ms", t0.elapsed().as_millis());
                let t = Instant::now();
                while t.elapsed() < Duration::from_millis(hold) {
                    engine.companion.set(1, guide);
                    std::thread::sleep(Duration::from_millis(4));
                }
                engine.companion.set(1, 0);
                println!("release at {} ms", t0.elapsed().as_millis());
                std::thread::sleep(Duration::from_secs(3));
            }
            engine.shutdown.store(true, std::sync::atomic::Ordering::Release);
            engine.companion.stop();
            println!("stopped at {} ms", t0.elapsed().as_millis());
        }
        "ps-home" => {
            // `ps-home <ds|ds4> <hold ms> <wait ms>`: plug in a virtual
            // DualSense or DualShock 4, wait, hold PS: does Windows react
            // to a PlayStation home button the way it does to Guide?
            let args: Vec<String> = std::env::args().skip_while(|a| a != "ps-home").skip(1).collect();
            let model = if args.first().map(|s| s.as_str()) == Some("ds4") {
                usbip::Model::DualShock4
            } else {
                usbip::Model::DualSense
            };
            let num = |i: usize, d: u64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
            let (hold, wait) = (num(1, 1500), num(2, 2000));
            let mut guard = super::audio_default::Watcher::new([false; 6]);
            guard.tick();
            let sink = usbip::DsSink {
                on_output: Box::new(|_| {}),
                mirror: Default::default(),
            };
            let t0 = Instant::now();
            match usbip::UsbPad::create(model, sink) {
                Ok(pad) => {
                    println!("{} attached in {} ms", model.name(), t0.elapsed().as_millis());
                    let mut seq = 0u8;
                    let mut feed = |until: Duration, ps: bool| {
                        let mut p = ds_proto::virtual_pad::PadState::default();
                        p.buttons.set(ds_proto::input::Button::Ps, ps);
                        while t0.elapsed() < until {
                            seq = seq.wrapping_add(1);
                            let rep: Vec<u8> = match model {
                                usbip::Model::DualShock4 => ds_proto::virtual_pad::ds4_usb_input(&p, None, false, seq).to_vec(),
                                _ => ds_proto::virtual_pad::ds_usb_input(&p, None, false, seq).to_vec(),
                            };
                            let _ = pad.submit(&rep);
                            guard.tick();
                            std::thread::sleep(Duration::from_millis(4));
                        }
                    };
                    feed(Duration::from_millis(wait), false);
                    println!("PS down at {} ms", t0.elapsed().as_millis());
                    feed(Duration::from_millis(wait + hold), true);
                    println!("PS up at {} ms", t0.elapsed().as_millis());
                    feed(Duration::from_millis(wait + hold + 1500), false);
                }
                Err(e) => println!("virtual {} failed: {e}", model.name()),
            }
        }
        "x360-guide" => {
            // `x360-guide <hold ms> <wait ms>`: plug in an Xbox 360 pad,
            // wait `wait` ms after attach, hold Guide for `hold` ms.
            let arg = |n: usize, d: u64| {
                std::env::args()
                    .skip_while(|a| a != "x360-guide")
                    .nth(n)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(d)
            };
            let (hold, wait) = (arg(1, 1500), arg(2, 0));
            let slot: u32 = std::env::var("DSZ_PROBE_SLOT").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
            usbip::reserve_slots_below(slot);
            // DSZ_PROBE_HIDE=1: hide the pad with HidHide before it exists,
            // so only whitelisted apps (not this probe's caller) see it.
            // DSZ_PROBE_ALLOW=<exe>: let that app see the hidden pad for the
            // run (taken off the list again at the end).
            let allow = std::env::var("DSZ_PROBE_ALLOW").ok().map(std::path::PathBuf::from);
            if let Some(a) = &allow {
                println!("allow {}: {:?}", a.display(), hidhide::set_app(a, true));
            }
            let hidden = if std::env::var_os("DSZ_PROBE_HIDE").is_some() {
                let mut ids = vec![format!(r"USB\VID_045E&PID_028E\DSZV{slot}")];
                ids.extend(
                    std::env::var("DSZ_PROBE_HIDE_ALSO")
                        .unwrap_or_default()
                        .split(';')
                        .filter(|s| !s.is_empty())
                        .map(String::from),
                );
                let exe = std::env::current_exe().unwrap_or_default();
                match hidhide::hide(&ids, &exe) {
                    Ok(ch) => {
                        println!("hidden: {:?}", ch.devices_added);
                        Some(ch)
                    }
                    Err(e) => {
                        println!("hidhide: {e}");
                        None
                    }
                }
            } else {
                None
            };
            let sink = usbip::DsSink {
                on_output: Box::new(|r| println!("driver output {:02X?}", &r[..r.len().min(6)])),
                mirror: Default::default(),
            };
            let t0 = Instant::now();
            match usbip::UsbPad::create(usbip::Model::Xbox360, sink) {
                Ok(pad) => {
                    println!("attached in {} ms", t0.elapsed().as_millis());
                    let _ = pad.submit_idle();
                    while !(6..=9).contains(&pad.led()) && t0.elapsed() < Duration::from_secs(5) {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    println!("solid LED, slot {:?}, at {} ms", pad.xinput_slot(), t0.elapsed().as_millis());
                    std::thread::sleep(Duration::from_millis(wait));
                    let mut p = ds_proto::virtual_pad::PadState::default();
                    p.buttons.set(ds_proto::input::Button::Ps, true);
                    let down = ds_proto::virtual_pad::x360_usb_input(&ds_proto::virtual_pad::xusb_report(&p));
                    println!("guide down at {} ms, LED slot {:?}", t0.elapsed().as_millis(), pad.xinput_slot());
                    let _ = pad.submit(&down);
                    std::thread::sleep(Duration::from_millis(hold));
                    let _ = pad.submit_idle();
                    println!("guide up at {} ms, LED slot {:?}", t0.elapsed().as_millis(), pad.xinput_slot());
                    std::thread::sleep(Duration::from_millis(1500));
                }
                Err(e) => println!("virtual Xbox 360 failed: {e}"),
            }
            if let Some(ch) = hidden {
                println!("unhide: {:?}", hidhide::restore(&ch));
            }
            if let Some(a) = &allow {
                println!("disallow {}: {:?}", a.display(), hidhide::set_app(a, false));
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
        "usbip-mic" => {
            // The microphone device used beside an Xbox 360 or DualShock 4
            // pad, with a 440 Hz tone in place of the controller: record
            // from "Wireless Controller" (Sound Recorder, Discord's mic
            // test) to hear it. Default devices are kept as the app does.
            let mut guard = super::audio_default::Watcher::new([false; 6]);
            guard.tick();
            let sink = usbip::DsSink {
                on_output: Box::new(|_| {}),
                mirror: Default::default(),
            };
            let t0 = Instant::now();
            match usbip::UsbPad::create(usbip::Model::Microphone, sink) {
                Ok(mic) => {
                    println!("attached in {} ms, serial {}", t0.elapsed().as_millis(), mic.serial());
                    let ring = mic.microphone();
                    let secs = secs_arg(20);
                    let mut listed = false;
                    let (mut recording, mut phase, mut pushed) = (false, 0f32, 0usize);
                    let step = 2.0 * std::f32::consts::PI * 440.0 / 48_000.0;
                    let mut next = Instant::now();
                    while t0.elapsed() < Duration::from_secs(secs) && mic.alive() {
                        guard.tick();
                        if !listed && t0.elapsed() > Duration::from_secs(3) {
                            listed = true;
                            println!("our recording endpoints: {:?}", super::audio_default::probe::ours(1));
                            for (i, (id, ours)) in super::audio_default::probe::snapshot().iter().enumerate() {
                                println!("  default slot {i}: ours {ours} {id:?}");
                            }
                        }
                        if mic.recording() != recording {
                            recording = !recording;
                            println!("{:.1} s: an app {} recording", t0.elapsed().as_secs_f32(), if recording { "started" } else { "stopped" });
                        }
                        // 10 ms of stereo tone at a time, on the clock.
                        let mut pcm = [0i16; 960];
                        for f in pcm.chunks_exact_mut(2) {
                            let v = (phase.sin() * 8_000.0) as i16;
                            phase = (phase + step) % (2.0 * std::f32::consts::PI);
                            f[0] = v;
                            f[1] = v;
                        }
                        ring.push(&pcm);
                        if recording {
                            pushed += 480;
                        }
                        next += Duration::from_millis(10);
                        std::thread::sleep(next.saturating_duration_since(Instant::now()));
                    }
                    println!("alive {} urbs {} frames offered while recording {pushed}", mic.alive(), mic.urbs());
                }
                Err(e) => println!("usbip-win2 microphone failed: {e}"),
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
        "dsdetect-all" => {
            // Scan every program in the detection cache again (no cache).
            let file = std::env::var("APPDATA").map(std::path::PathBuf::from).unwrap_or_default()
                .join("DSZ").join("dualsense-detect.json");
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let map: std::collections::BTreeMap<String, serde_json::Value> =
                serde_json::from_str(&text).unwrap_or_default();
            // DSZ_SCAN_DIRS=<dir;dir>: scan every exe below these too.
            let mut map = map;
            for d in std::env::var("DSZ_SCAN_DIRS").unwrap_or_default().split(';').filter(|d| !d.is_empty()) {
                let mut stack = vec![(std::path::PathBuf::from(d), 0)];
                while let Some((dir, depth)) = stack.pop() {
                    let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            if depth < 5 { stack.push((p, depth + 1)); }
                        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")) {
                            map.entry(p.display().to_string().to_ascii_lowercase()).or_insert(serde_json::Value::Null);
                        }
                    }
                }
            }
            let t0 = Instant::now();
            let mut n = 0;
            for (path, old) in &map {
                let p = std::path::Path::new(path);
                if !p.exists() {
                    continue;
                }
                n += 1;
                let now = crate::dsdetect::scan(p);
                let before = old.get("found").and_then(|v| v.as_str()).map(String::from);
                if now.is_some() || before.is_some() {
                    let mark = if before.is_none() { "NEW " } else if now.is_none() { "LOST" } else { "    " };
                    println!("{mark} {:<45} {}", now.or(before).unwrap_or_default(), path);
                }
            }
            println!("scanned {n} programs in {} ms", t0.elapsed().as_millis());
        }
        "speaker" => {
            // `speaker [--secs N]`: speaker stream with no input injection;
            // prints what the controller sends back.
            let report = crate::haptics::probe::speaker(secs_arg(4));
            println!("{report}");
            if let Some(f) = std::env::var_os("DSZ_PROBE_OUT") {
                let _ = std::fs::write(f, &report);
            }
        }
        "speaker-ab" => {
            let only = std::env::args()
                .skip_while(|a| a != "--variant")
                .nth(1)
                .and_then(|s| s.parse().ok());
            let mic_off_at = std::env::args()
                .skip_while(|a| a != "--mic-off-at")
                .nth(1)
                .and_then(|s| s.parse::<f32>().ok())
                .map(std::time::Duration::from_secs_f32);
            let report = crate::haptics::probe::speaker_ab(only, secs_arg(10), mic_off_at);
            println!("{report}");
            if let Some(f) = std::env::var_os("DSZ_PROBE_OUT") {
                let _ = std::fs::write(f, &report);
            }
        }
        "mic-listen" => {
            // Listen-only: never writes to the controller.
            let report = crate::haptics::probe::mic_listen(secs_arg(20));
            println!("{report}");
            if let Some(f) = std::env::var_os("DSZ_PROBE_OUT") {
                let _ = std::fs::write(f, &report);
            }
        }
        "tone" => {
            let _ = play_tone(0, secs_arg(6)).join();
        }
        "default-watch" => {
            // How Windows moves the default audio devices when a virtual
            // DualSense comes and goes, and whether it puts back a default
            // the user picked. Restores the original defaults at the end.
            use super::audio_default::probe as ad;
            let slot: u32 = std::env::var("DSZ_PROBE_SLOT").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
            usbip::reserve_slots_below(slot);
            let orig = ad::snapshot();
            let short = |id: &Option<String>| {
                id.as_deref().map(|s| s.rsplit('.').next().unwrap_or(s).to_string()).unwrap_or_default()
            };
            for (i, (id, ours)) in orig.iter().enumerate() {
                println!("start slot {i}: {} ours {ours}", short(id));
            }
            let watch = |label: &str, secs: u64| {
                let t0 = Instant::now();
                let mut last = ad::snapshot();
                let mut seen = [false, false];
                while t0.elapsed() < Duration::from_secs(secs) {
                    std::thread::sleep(Duration::from_millis(25));
                    for f in 0..2 {
                        let now = !ad::ours(f).is_empty();
                        if now != seen[f as usize] {
                            seen[f as usize] = now;
                            println!("{label} +{:>5} ms flow {f}: our endpoint {}", t0.elapsed().as_millis(),
                                if now { "active" } else { "gone" });
                        }
                    }
                    let now = ad::snapshot();
                    for i in 0..6 {
                        if now[i] != last[i] {
                            println!("{label} +{:>5} ms slot {i}: {} -> {} ours {}", t0.elapsed().as_millis(),
                                short(&last[i].0), short(&now[i].0), now[i].1);
                        }
                    }
                    last = now;
                }
            };
            let sink = || usbip::DsSink { on_output: Box::new(|_| {}), mirror: Default::default() };
            let restore = |what: &str| {
                for (i, (id, _)) in orig.iter().enumerate() {
                    if let Some(id) = id {
                        ad::set(id, (i % 3) as i32);
                    }
                }
                println!("{what}: original defaults restored");
            };

            println!("--- 1: attach");
            let pad = match usbip::UsbPad::create(usbip::Model::DualSense, sink()) {
                Ok(p) => p,
                Err(e) => { println!("attach failed: {e}"); return; }
            };
            watch("attach1", 12);
            restore("after attach1");
            watch("settle1", 2);
            let render = ad::ours(0);
            println!("our render endpoints: {render:?}");
            println!("--- 2: pick ours as the default output, as a user would");
            if let Some(id) = render.first() {
                for role in 0..3 {
                    ad::set(id, role);
                }
            }
            watch("picked", 2);
            println!("--- 3: detach");
            drop(pad);
            watch("detach", 6);
            println!("--- 4: attach again");
            let pad = match usbip::UsbPad::create(usbip::Model::DualSense, sink()) {
                Ok(p) => p,
                Err(e) => { println!("attach failed: {e}"); restore("end"); return; }
            };
            watch("attach2", 12);
            restore("before detach2");
            drop(pad);
            watch("detach2", 4);
            restore("end");
            for (i, (id, ours)) in ad::snapshot().iter().enumerate() {
                println!("end slot {i}: {} ours {ours}", short(id));
            }
        }
        "default-guard" => {
            // The audio-default watcher against Windows: plug, pick, unplug,
            // replug, un-pick. Prints the defaults and the learned slots
            // after each step; restores the original defaults at the end.
            use super::audio_default::{probe as ad, Watcher};
            use std::sync::{Arc, Mutex};
            let slot: u32 = std::env::var("DSZ_PROBE_SLOT").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
            usbip::reserve_slots_below(slot);
            let orig = ad::snapshot();
            // DSZ_PROBE_PREFS=110000: start with those slots chosen.
            let mut prefs = [false; 6];
            for (i, c) in std::env::var("DSZ_PROBE_PREFS").unwrap_or_default().chars().take(6).enumerate() {
                prefs[i] = c == '1';
            }
            let w = Arc::new(Mutex::new(Watcher::new(prefs)));
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (w2, stop2) = (w.clone(), stop.clone());
            let t = std::thread::spawn(move || {
                while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                    let busy = {
                        let mut g = w2.lock().unwrap();
                        g.tick();
                        g.busy()
                    };
                    std::thread::sleep(Duration::from_millis(if busy { 100 } else { 500 }));
                }
            });
            let show = |step: &str| {
                let snap = ad::snapshot();
                let marks: String = snap
                    .iter()
                    .enumerate()
                    .map(|(i, (id, ours))| {
                        if *ours { 'V' } else if id == &orig[i].0 { 'o' } else { '?' }
                    })
                    .collect();
                println!("{step:<34} defaults {marks}  learned {:?}", w.lock().unwrap().prefs.map(|b| b as u8));
            };
            let sink = || usbip::DsSink { on_output: Box::new(|_| {}), mirror: Default::default() };
            let restore = || {
                for (i, (id, _)) in orig.iter().enumerate() {
                    if let Some(id) = id {
                        ad::set(id, (i % 3) as i32);
                    }
                }
            };
            println!("defaults: o = original, V = virtual DualSense; slots out x3, mic x3");
            std::thread::sleep(Duration::from_secs(1));
            show("start");
            let pad = match usbip::UsbPad::create(usbip::Model::DualSense, sink()) {
                Ok(p) => p,
                Err(e) => { println!("attach failed: {e}"); return; }
            };
            std::thread::sleep(Duration::from_secs(5));
            show("1 plugged (expect oooooo)");
            if let Some(id) = ad::ours(0).first() {
                ad::set(id, 0);
                ad::set(id, 1);
            }
            std::thread::sleep(Duration::from_secs(2));
            show("2 user picks output (VVo ooo)");
            drop(pad);
            std::thread::sleep(Duration::from_secs(3));
            show("3 unplugged (oooooo, kept 110)");
            let pad = match usbip::UsbPad::create(usbip::Model::DualSense, sink()) {
                Ok(p) => p,
                Err(e) => { println!("attach failed: {e}"); restore(); return; }
            };
            std::thread::sleep(Duration::from_secs(5));
            show("4 replugged (VVoooo)");
            if let Some(id) = &orig[0].0 {
                ad::set(id, 0);
                ad::set(id, 1);
            }
            std::thread::sleep(Duration::from_secs(2));
            show("5 user picks original (oooooo, 0)");
            drop(pad);
            std::thread::sleep(Duration::from_secs(3));
            let pad = match usbip::UsbPad::create(usbip::Model::DualSense, sink()) {
                Ok(p) => p,
                Err(e) => { println!("attach failed: {e}"); restore(); return; }
            };
            std::thread::sleep(Duration::from_secs(5));
            show("6 replugged (oooooo)");
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = t.join();
            drop(pad);
            std::thread::sleep(Duration::from_secs(2));
            restore();
            show("end, restored");
        }
        "hold-x360" => {
            // Plug a virtual Xbox 360 pad on a probe slot (never hidden) and
            // hold it, so other tools can look at it.
            let slot: u32 = std::env::var("DSZ_PROBE_SLOT").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
            usbip::reserve_slots_below(slot);
            let sink = usbip::DsSink { on_output: Box::new(|_| {}), mirror: Default::default() };
            match usbip::UsbPad::create(usbip::Model::Xbox360, sink) {
                Ok(pad) => {
                    println!("attached");
                    std::thread::sleep(Duration::from_secs(secs_arg(10)));
                    drop(pad);
                    println!("removed");
                }
                Err(e) => println!("attach failed: {e}"),
            }
        }
        "hide-cycle" => {
            // A probe Xbox 360 pad: plugged at 0 s, hidden with HidHide at
            // 4 s, shown again at 10 s, unplugged at 16 s. Look at raw input
            // from outside in between.
            let slot: u32 = std::env::var("DSZ_PROBE_SLOT").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
            usbip::reserve_slots_below(slot);
            let sink = usbip::DsSink { on_output: Box::new(|_| {}), mirror: Default::default() };
            let t0 = Instant::now();
            // DSZ_PROBE_PRE_HIDE=<id;id>: hide these (and the pad) before it
            // is plugged in, as a parked pad is, and show them at 6 s.
            let exe = std::env::current_exe().unwrap_or_default();
            let pre: Vec<String> = std::env::var("DSZ_PROBE_PRE_HIDE")
                .unwrap_or_default()
                .split(';')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            if !pre.is_empty() {
                let mut ids = vec![format!(r"USB\VID_045E&PID_028E\DSZV{slot}")];
                ids.extend(pre);
                let ch = match hidhide::hide(&ids, &exe) {
                    Ok(c) => c,
                    Err(e) => { println!("hide failed: {e}"); return; }
                };
                let pad = match usbip::UsbPad::create(usbip::Model::Xbox360, sink) {
                    Ok(p) => p,
                    Err(e) => { println!("attach failed: {e}"); let _ = hidhide::restore(&ch); return; }
                };
                println!("+{:>5} ms attached hidden {ids:?}", t0.elapsed().as_millis());
                // DSZ_PROBE_LATE_HIDE=1: hide the rest of the pad's nodes
                // (its HID child) once Windows has made them.
                let late = if std::env::var_os("DSZ_PROBE_LATE_HIDE").is_some() {
                    std::thread::sleep(Duration::from_millis(1500));
                    let root = format!(r"USB\VID_045E&PID_028E\DSZV{slot}");
                    let rest: Vec<String> = super::win::descendant_ids(&root)
                        .into_iter()
                        .filter(|d| !ids.iter().any(|i| i.eq_ignore_ascii_case(d)))
                        .collect();
                    println!("+{:>5} ms late hide {rest:?}", t0.elapsed().as_millis());
                    hidhide::hide(&rest, &exe).ok()
                } else {
                    None
                };
                std::thread::sleep(Duration::from_millis(4500));
                if let Some(l) = &late {
                    println!("late restore {:?}", hidhide::restore(l));
                }
                println!("+{:>5} ms showing: {:?}", t0.elapsed().as_millis(), hidhide::restore(&ch));
                // DSZ_PROBE_REPLUG=1: then heal it the way the pool does.
                if std::env::var_os("DSZ_PROBE_REPLUG").is_some() {
                    std::thread::sleep(Duration::from_secs(2));
                    let hid = pad.hid_instance();
                    let listed = hid.as_deref().map(super::win::raw_input_lists);
                    println!("+{:>5} ms hid {hid:?} in raw input: {listed:?}", t0.elapsed().as_millis());
                    let r = pad.replug();
                    println!("+{:>5} ms replug: {r:?}", t0.elapsed().as_millis());
                    std::thread::sleep(Duration::from_secs(2));
                    let hid = pad.hid_instance();
                    let listed = hid.as_deref().map(super::win::raw_input_lists);
                    println!("+{:>5} ms hid {hid:?} in raw input: {listed:?} alive {}", t0.elapsed().as_millis(), pad.alive());
                    let ok = pad.submit_idle();
                    println!("submit after replug: {ok:?}");
                }
                std::thread::sleep(Duration::from_secs(8));
                drop(pad);
                println!("+{:>5} ms removed", t0.elapsed().as_millis());
                return;
            }
            let pad = match usbip::UsbPad::create(usbip::Model::Xbox360, sink) {
                Ok(p) => p,
                Err(e) => { println!("attach failed: {e}"); return; }
            };
            println!("+{:>5} ms attached", t0.elapsed().as_millis());
            std::thread::sleep(Duration::from_secs(4));
            let root = format!(r"USB\VID_045E&PID_028E\DSZV{slot}");
            let mut ids = vec![root.clone()];
            ids.extend(super::win::descendant_ids(&root));
            println!("+{:>5} ms hiding {ids:?}", t0.elapsed().as_millis());
            let ch = match hidhide::hide(&ids, &exe) {
                Ok(c) => c,
                Err(e) => { println!("hide failed: {e}"); return; }
            };
            std::thread::sleep(Duration::from_secs(6));
            println!("+{:>5} ms showing again: {:?}", t0.elapsed().as_millis(), hidhide::restore(&ch));
            std::thread::sleep(Duration::from_secs(6));
            drop(pad);
            println!("+{:>5} ms removed", t0.elapsed().as_millis());
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
            "probe what? hidhide | usbip-x360 | usbip-ds4 | usbip-ds | usbip-mic | tone | default-audio"
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
