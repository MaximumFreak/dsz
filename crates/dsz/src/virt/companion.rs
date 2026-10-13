//! The helper Xbox pad: a virtual Xbox 360 controller that presses Xbox
//! buttons for a profile with no virtual Xbox pad of its own (a DualSense
//! profile, or none). Windows reacts to the Xbox button only from an Xbox
//! controller: a hold opens Task View. PS on a virtual DualSense or
//! DualShock 4 does nothing (tested).
//!
//! With HidHide, the pad is hidden before Windows sees it, from everything
//! but explorer.exe and Game Bar. explorer.exe handles Guide (tested: Task
//! View opens, and an app reading XInput or HID doesn't see the pad). The
//! nodes Windows makes below the pad (its XInput HID device) get ids only
//! once they appear; they are hidden then and remembered per serial, so the
//! next plug-in hides them in advance. A hidden pad stays plugged in while a
//! connected controller's profile has an Xbox-button mapping, so presses are
//! instant.
//!
//! Without HidHide games would see the pad, so it is plugged in on the
//! first press and unplugged [`LINGER`] after the last one. Windows takes
//! about a second to start reading a new pad (measured: Guide went unseen
//! 0.6 s after attach, seen at 1.05 s), so a press made before then is held
//! back and replayed for as long as it was held.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ds_proto::virtual_pad as vp;
use ds_proto::Buttons;
use parking_lot::{Condvar, Mutex};

use super::{hidhide, usbip, win};
use crate::engine::Engine;
use crate::profile::{Action, VirtualKind};

/// Unplug this long after the last press (visible), or after no controller
/// wants the pad any more (hidden).
const LINGER: Duration = Duration::from_secs(30);
/// Earliest press Windows sees: after attach, and after the ring LED
/// settles on a player.
const READY_AFTER_ATTACH: Duration = Duration::from_millis(1200);
const READY_AFTER_LED: Duration = Duration::from_millis(600);
/// A controller that stops refreshing its buttons (it went away) lets go.
const STALE: Duration = Duration::from_millis(300);
/// After a failed plug-in, don't retry for this long.
const RETRY_AFTER: Duration = Duration::from_secs(10);
/// Longest replay of a press made before the pad was ready.
const MAX_REPLAY: Duration = Duration::from_secs(3);
const MIN_REPLAY: Duration = Duration::from_millis(80);
/// The helper's key in the [`super::Hider`]; never a device or pad id.
const HIDER_KEY: u64 = u64::MAX;
/// How often to recheck whether any controller wants the helper.
const WANT_EVERY: Duration = Duration::from_millis(500);
/// Look for new nodes below the pad this often, closely at first.
const CHILD_WATCH: Duration = Duration::from_secs(15);
const CHILD_EVERY_EARLY: Duration = Duration::from_millis(20);
const CHILD_EVERY_LATE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Companion {
    /// Buttons each controller holds, in DualSense layout, and when it
    /// last said so.
    held: Mutex<HashMap<u64, (u32, Instant)>>,
    wake: Condvar,
    /// The thread has unplugged the pad and returned.
    done: AtomicBool,
    /// HidHide can hide the pad (installed, and the app may use it).
    can_hide: AtomicBool,
}

impl Companion {
    /// What controller `device` presses on the helper pad right now. Call
    /// every input report while anything is held, and once with 0 after.
    pub fn set(&self, device: u64, buttons: u32) {
        let mut h = self.held.lock();
        if buttons == 0 {
            h.remove(&device);
        } else {
            let was = h.insert(device, (buttons, Instant::now()));
            if was.is_none() {
                self.wake.notify_one();
            }
        }
    }

    /// At exit: wake the thread and give it a moment to unplug the pad.
    pub fn stop(&self) {
        self.wake.notify_all();
        let t0 = Instant::now();
        while !self.done.load(Ordering::Acquire) && t0.elapsed() < Duration::from_millis(500) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether the pad can be hidden from games (for the mapping page).
    pub fn can_hide(&self) -> bool {
        self.can_hide.load(Ordering::Relaxed)
    }

    fn current(&self) -> u32 {
        let mut h = self.held.lock();
        h.retain(|_, (_, at)| at.elapsed() < STALE);
        h.values().fold(0, |m, (b, _)| m | b)
    }

    fn sleep(&self, d: Duration) {
        let mut h = self.held.lock();
        if h.is_empty() {
            self.wake.wait_for(&mut h, d);
        } else {
            drop(h);
            std::thread::sleep(Duration::from_millis(4).min(d));
        }
    }
}

/// HidHide state of a hidden helper pad.
struct Hidden {
    serial: String,
    /// Every node hidden so far: the pad and what Windows made below it.
    ids: Vec<String>,
    apps: Vec<PathBuf>,
    checked: Instant,
}

struct Live {
    pad: usbip::UsbPad,
    attached: Instant,
    led_at: Option<Instant>,
    ready: bool,
    sent: Option<u32>,
    hidden: Option<Hidden>,
}

fn report(buttons: u32) -> [u8; vp::X360_USB_INPUT_LEN] {
    let b = Buttons(buttons);
    let p = vp::PadState {
        buttons: b,
        l2: if b.has(ds_proto::input::Button::L2) { 255 } else { 0 },
        r2: if b.has(ds_proto::input::Button::R2) { 255 } else { 0 },
        ..Default::default()
    };
    vp::x360_usb_input(&vp::xusb_report(&p))
}

fn usb_id(serial: &str) -> String {
    format!(r"USB\VID_045E&PID_028E\{serial}")
}

/// Apps that may see the hidden pad: explorer.exe, which opens Task View
/// on a Guide hold, and Game Bar while it runs (its folder changes with
/// each update).
fn readers() -> Vec<PathBuf> {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let mut v = vec![Path::new(&windir).join("explorer.exe")];
    v.extend(
        crate::platform::process_list()
            .into_iter()
            .filter(|(_, n)| n == "gamebar.exe")
            .find_map(|(pid, _)| crate::platform::process_path(pid)),
    );
    v
}

/// Whether a connected controller's profile presses Xbox buttons on the
/// helper: it has an Xbox mapping and no virtual Xbox pad of its own.
fn wanted(engine: &Engine) -> bool {
    engine.device_list().iter().any(|d| {
        d.virt.active() != Some(VirtualKind::Xbox360)
            && engine
                .profiles
                .get_or_first(&engine.profile_name_for(d))
                .mappings
                .iter()
                .any(|m| m.enabled && matches!(m.action, Action::Xbox { .. }))
    })
}

fn hide_ok(engine: &Engine) -> bool {
    engine.settings.read().hidhide_control && hidhide::available()
}

/// Plug in the pad, hidden first when HidHide is there.
fn plug(engine: &Engine) -> Result<Live, String> {
    if let Some(why) = usbip::blocked() {
        return Err(why);
    }
    if !usbip::available() {
        return Err("The Xbox button needs the usbip-win2 driver, to plug in a virtual Xbox controller for the press. Install it from the Virtual controller page.".to_string());
    }
    let sink = usbip::DsSink {
        on_output: Box::new(|_| {}),
        mirror: Default::default(),
    };
    let hide = hide_ok(engine);
    let apps = if hide { readers() } else { Vec::new() };
    let hidden: Mutex<Option<Hidden>> = Mutex::new(None);
    let r = usbip::UsbPad::create_with(usbip::Model::Xbox360, sink, &|serial| {
        if !hide {
            return;
        }
        let mut ids = vec![usb_id(serial)];
        ids.extend(engine.settings.read().helper_nodes.get(serial).cloned().unwrap_or_default());
        match engine.hider.set_with_apps(HIDER_KEY, ids.clone(), apps.clone()) {
            Ok(true) => {
                *hidden.lock() = Some(Hidden {
                    serial: serial.to_string(),
                    ids,
                    apps: apps.clone(),
                    checked: Instant::now(),
                })
            }
            Ok(false) => {}
            Err(e) => log::warn!("HidHide, helper Xbox controller: {e}"),
        }
    });
    let hidden = hidden.into_inner();
    match r {
        Ok(pad) => {
            let _ = pad.submit_idle();
            log::info!(
                "helper Xbox controller plugged in ({})",
                if hidden.is_some() { "hidden from games" } else { "visible, for this press" }
            );
            Ok(Live {
                pad,
                attached: Instant::now(),
                led_at: None,
                ready: false,
                sent: None,
                hidden,
            })
        }
        Err(e) => {
            if hidden.is_some() {
                let _ = engine.hider.set(HIDER_KEY, Vec::new());
            }
            Err(format!("usbip-win2 couldn't attach the helper Xbox controller ({e})."))
        }
    }
}

/// Hide the nodes Windows makes below the pad as they appear, and let Game
/// Bar through once it runs.
fn hide_children(engine: &Engine, l: &mut Live) {
    let Some(h) = l.hidden.as_mut() else { return };
    let early = l.attached.elapsed() < CHILD_WATCH;
    if h.checked.elapsed() < if early { CHILD_EVERY_EARLY } else { CHILD_EVERY_LATE } {
        return;
    }
    h.checked = Instant::now();
    let kids = win::descendant_ids(&usb_id(&h.serial));
    let new: Vec<String> = kids
        .iter()
        .filter(|k| !h.ids.iter().any(|i| i.eq_ignore_ascii_case(k)))
        .cloned()
        .collect();
    let apps = if early { h.apps.clone() } else { readers() };
    if new.is_empty() && apps == h.apps {
        return;
    }
    let mut ids = h.ids.clone();
    ids.extend(new.iter().cloned());
    match engine.hider.set_with_apps(HIDER_KEY, ids.clone(), apps.clone()) {
        Ok(_) => {
            h.ids = ids;
            h.apps = apps;
        }
        Err(e) => {
            log::warn!("HidHide, helper Xbox controller: {e}");
            return;
        }
    }
    if !new.is_empty() {
        log::info!("helper Xbox controller: hid {} new node(s) below it", new.len());
        engine.settings.write().helper_nodes.insert(h.serial.clone(), kids);
        engine.settings_save_later();
    }
}

/// Unplug the pad, then stop hiding it (in that order, so it is never
/// visible).
fn unplug(engine: &Engine, live: &mut Option<Live>) {
    let hidden = live.as_ref().is_some_and(|l| l.hidden.is_some());
    *live = None;
    if hidden {
        if let Err(e) = engine.hider.set(HIDER_KEY, Vec::new()) {
            log::warn!("HidHide, helper Xbox controller: {e}");
        }
    }
}

/// The helper pad's thread; runs until the app exits.
pub fn run(engine: Arc<Engine>) {
    let c = &engine.companion;
    let mut live: Option<Live> = None;
    let mut failed_at: Option<Instant> = None;
    let mut last_use = Instant::now();
    // A press made before the pad was ready: its buttons, when it started,
    // and how long it lasted once released.
    let mut early: Option<(u32, Instant, Option<Duration>)> = None;
    let mut replay: Option<(u32, Instant)> = None;
    let mut want = false;
    let mut want_checked: Option<Instant> = None;
    let mut last_want = Instant::now();

    while !engine.shutdown.load(Ordering::Acquire) {
        let now = Instant::now();
        let held = c.current();
        if held != 0 {
            last_use = now;
        }
        if want_checked.is_none_or(|t| t.elapsed() >= WANT_EVERY) {
            want_checked = Some(now);
            // Plugged in ahead of a press only when it can be hidden.
            let can = hide_ok(&engine);
            c.can_hide.store(can, Ordering::Relaxed);
            want = can && wanted(&engine);
            if want {
                last_want = now;
            }
        }

        if (held != 0 || want) && live.is_none() && failed_at.is_none_or(|t| t.elapsed() >= RETRY_AFTER) {
            match plug(&engine) {
                Ok(l) => {
                    live = Some(l);
                    failed_at = None;
                }
                Err(e) => {
                    log::warn!("helper Xbox controller: {e}");
                    engine.warn_toast(e);
                    failed_at = Some(now);
                    early = None;
                }
            }
        }

        if let Some(l) = live.as_mut() {
            if !l.pad.alive() {
                log::warn!("helper Xbox controller was detached");
                unplug(&engine, &mut live);
                early = None;
                replay = None;
                c.sleep(Duration::from_millis(50));
                continue;
            }
            hide_children(&engine, l);
            if !l.ready {
                if l.led_at.is_none() && (6..=9).contains(&l.pad.led()) {
                    l.led_at = Some(now);
                }
                l.ready = l.led_at.is_some_and(|t| t.elapsed() >= READY_AFTER_LED)
                    && l.attached.elapsed() >= READY_AFTER_ATTACH;
                // Fall back on time alone if the LED never settles.
                l.ready |= l.attached.elapsed() >= READY_AFTER_ATTACH * 5;
                if !l.ready {
                    // Remember the press for the replay.
                    match early.as_mut() {
                        None if held != 0 => early = Some((held, now, None)),
                        Some((b, start, len)) => {
                            *b |= held;
                            if held == 0 && len.is_none() {
                                *len = Some(now - *start);
                            }
                        }
                        None => {}
                    }
                } else {
                    log::info!(
                        "helper Xbox controller ready after {} ms",
                        l.attached.elapsed().as_millis()
                    );
                    if let Some((b, start, len)) = early.take() {
                        let len = len.unwrap_or(now - start).clamp(MIN_REPLAY, MAX_REPLAY);
                        replay = Some((b, now + len));
                        log::info!("replaying a {} ms press made before that", len.as_millis());
                    }
                }
            }
            if l.ready {
                let mut out = held;
                if let Some((b, until)) = replay {
                    if now < until {
                        out |= b;
                        last_use = now;
                    } else {
                        replay = None;
                    }
                }
                if l.sent != Some(out) {
                    match l.pad.submit(&report(out)) {
                        Ok(()) => l.sent = Some(out),
                        Err(e) => log::debug!("helper Xbox controller report: {e}"),
                    }
                }
            }
            let idle = held == 0 && replay.is_none() && early.is_none();
            let unwanted = if l.hidden.is_some() {
                // Hidden: stays while wanted, and a while after (a
                // controller turning off and on again).
                !want && last_want.elapsed() >= LINGER && last_use.elapsed() >= LINGER
            } else {
                let own_xbox = engine
                    .device_list()
                    .iter()
                    .any(|d| d.virt.active() == Some(VirtualKind::Xbox360));
                last_use.elapsed() >= LINGER || own_xbox
            };
            if idle && unwanted {
                log::info!("helper Xbox controller unplugged");
                unplug(&engine, &mut live);
            }
        }

        c.sleep(if live.is_some() {
            Duration::from_millis(4)
        } else {
            WANT_EVERY
        });
    }
    unplug(&engine, &mut live);
    c.done.store(true, Ordering::Release);
}
