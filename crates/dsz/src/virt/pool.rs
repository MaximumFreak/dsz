//! Virtual pads that outlive the controller driving them.
//!
//! A pad belongs to the pool, not to a controller connection. A controller
//! claims its pad when it connects (the one it had before, or the pad made
//! at startup) and lets go when it disconnects. While nobody drives a pad it
//! stays plugged in with a centered, idle report, and the physical
//! controller stays hidden, so a running game keeps the same pad and XInput
//! slot through a controller sleeping or dropping out. At startup the pool
//! plugs in one pad for the expected profile before any controller shows up,
//! so it takes the first free slot.
//!
//! Pads go away when their profile stops asking for that kind, when
//! persistence is turned off, or when the app exits.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use ds_proto::virtual_pad as vp;
use parking_lot::Mutex;

use super::{hidhide, usbip, Target};
use crate::device::Device;
use crate::engine::Engine;
use crate::profile::VirtualKind;

/// Hider key that keeps an absent controller hidden while its idle pad is
/// replaced. The companion pad uses `u64::MAX`; pad ids start at 1 << 40.
const SWAP_HIDE_IDLE: u64 = u64::MAX - 1;
/// Added to a pad's id for the Hider key that hides the pad itself while it
/// is parked (its own id hides the physical controller).
const PARK_HIDE: u64 = 1 << 62;
/// How often a parked pad's hiding is refreshed: closely while Windows may
/// still be adding nodes below a pad plugged in parked, then for Game Bar.
const PARK_WATCH: Duration = Duration::from_secs(15);
const PARK_EVERY_EARLY: Duration = Duration::from_millis(20);
const PARK_EVERY_LATE: Duration = Duration::from_secs(5);
/// How often an Xbox pad in use is checked for raw input, and the least
/// time between two replugs of one pad.
const RAW_CHECK_EVERY: Duration = Duration::from_secs(5);
const REPLUG_AGAIN_AFTER: Duration = Duration::from_secs(60);
/// A pad this new may not be in raw input's list yet.
const RAW_SETTLE: Duration = Duration::from_secs(3);
/// A game started this long before a pad was hidden may still have been
/// starting up (looking for controllers) by then.
const GAME_START_MARGIN: Duration = Duration::from_secs(30);
/// The pad plugged in at startup sends idle again this long after Windows
/// lit its ring LED, and at these times after it was plugged in.
const STARTUP_RELEASE_AFTER_LED: Duration = Duration::from_millis(600);
const STARTUP_RELEASES: [Duration; 3] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// When an Xbox pad's raw input check is next due, and its last replug.
struct RawCheck {
    next: Instant,
    replugged: Option<Instant>,
}

impl RawCheck {
    fn new() -> Self {
        RawCheck {
            next: Instant::now() + RAW_SETTLE,
            replugged: None,
        }
    }
}

/// Where a pad's game feedback goes: the controller driving it, if any.
#[derive(Default)]
pub struct Route {
    owner: Mutex<Weak<Device>>,
    /// Calibration answered to the game; follows the driving controller.
    mirror: Arc<Mutex<super::mirror::Mirror>>,
}

impl Route {
    fn device(&self) -> Option<Arc<Device>> {
        self.owner.lock().upgrade()
    }
}

pub struct Pad {
    /// HidHide key; never collides with a device id.
    pub id: u64,
    pub target: Arc<Target>,
    route: Arc<Route>,
    /// The controller this pad belongs to; empty until one claims it.
    mac: Mutex<String>,
    owner: Mutex<Option<u64>>,
    /// HidHide hides these nodes on this pad's behalf.
    hidden: Mutex<Vec<String>>,
    /// A parked Xbox pad: plugged in and idle, holding its XInput slot,
    /// while its controller's profile uses another kind or none. It only
    /// presses Xbox buttons.
    parked: Mutex<Option<Parked>>,
    /// See [`Pool::heal`].
    raw_check: Mutex<RawCheck>,
}

/// How a parked pad is hidden.
struct Parked {
    /// Its own nodes, hidden from all but explorer.exe and Game Bar.
    ids: Vec<String>,
    apps: Vec<std::path::PathBuf>,
    since: Instant,
    checked: Instant,
    /// Hidden before Windows first saw it: nothing running then found it.
    born_hidden: bool,
    /// When it was hidden. A game started since (or just before: the game
    /// that switched to DualSense starts up while it is hidden) looked for
    /// controllers without it.
    hidden_at: SystemTime,
}

impl Parked {
    fn visible() -> Parked {
        Parked {
            ids: Vec::new(),
            apps: Vec::new(),
            since: Instant::now(),
            checked: Instant::now(),
            born_hidden: false,
            hidden_at: SystemTime::now(),
        }
    }
}

/// Why a parked pad, now shown, has to be plugged in again for games to
/// find it, or `None`. Games look for controllers when they start and when
/// one arrives, and taking a pad off HidHide's list is no arrival. A pad
/// that was visible when it arrived and when every game still running
/// started needs nothing: switching back after a DualSense game closes
/// stays a replug-free swap.
fn missed_by(engine: &Engine, p: &Parked) -> Option<String> {
    if p.born_hidden {
        return Some("it was hidden when it was plugged in".into());
    }
    let since = p.hidden_at - GAME_START_MARGIN;
    crate::platform::process_list()
        .into_iter()
        .filter(|(pid, _)| crate::platform::process_started(*pid).is_some_and(|t| t >= since))
        .find(|(pid, _)| {
            crate::platform::process_path(*pid).is_some_and(|path| {
                crate::games::is_likely_game(&path.to_string_lossy())
                    || engine.dsdetect.check(&path).is_some()
            })
        })
        .map(|(_, name)| format!("{name} started while it was hidden"))
}

impl Pad {
    pub fn kind(&self) -> VirtualKind {
        self.target.kind()
    }

    pub fn is_hidden(&self) -> bool {
        !self.hidden.lock().is_empty()
    }

    pub fn hidden_ids(&self) -> Vec<String> {
        self.hidden.lock().clone()
    }

    pub fn is_parked(&self) -> bool {
        self.parked.lock().is_some()
    }

    /// Whether games can't see the parked pad.
    pub fn park_hidden(&self) -> bool {
        self.parked.lock().as_ref().is_some_and(|p| !p.ids.is_empty())
    }

    fn alive(&self) -> bool {
        self.target.alive()
    }

    /// Centered sticks, nothing pressed.
    fn submit_idle(&self) {
        if let Err(e) = self.target.submit_idle() {
            log::debug!("idle report to the virtual controller: {e}");
        }
    }
}

pub struct Pool {
    pads: Mutex<Vec<Arc<Pad>>>,
    next_id: AtomicU64,
    started: AtomicBool,
}

impl Default for Pool {
    fn default() -> Self {
        Pool {
            pads: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1 << 40),
            started: AtomicBool::new(false),
        }
    }
}

/// Plug in a pad of `kind` for the controller with address `mac` (a
/// DualShock 4 takes its identity). Game feedback follows `route`.
fn plug(kind: VirtualKind, route: &Arc<Route>, mac: &str) -> Result<Target, String> {
    plug_with(kind, route, mac, &|_| {})
}

/// [`plug`], calling `before_attach` with the pad's serial before Windows
/// sees it.
fn plug_with(
    kind: VirtualKind,
    route: &Arc<Route>,
    mac: &str,
    before_attach: &dyn Fn(&str),
) -> Result<Target, String> {
    use usbip::Model;
    let model = match kind {
        VirtualKind::Off => return Err("off".into()),
        VirtualKind::Xbox360 => Model::Xbox360,
        VirtualKind::DualSense => Model::DualSense,
        VirtualKind::DualShock4 => Model::DualShock4,
    };
    if let Some(why) = usbip::blocked() {
        return Err(why);
    }
    if !usbip::available() {
        return Err(format!(
            "A virtual {} controller needs the usbip-win2 driver (free, open source). Install it from the Virtual controller page; until then games see the real controller.",
            kind.label()
        ));
    }
    let r = route.clone();
    let on_output: Box<dyn Fn(&[u8]) + Send + Sync> = match model {
        Model::DualSense => Box::new(move |rep| {
            if let Some(d) = r.device() {
                d.apply_game_output(rep);
            }
        }),
        Model::Xbox360 => Box::new(move |rep| {
            if let (Some(d), Some(vp::X360Output::Rumble(large, small))) =
                (r.device(), vp::parse_x360_output(rep))
            {
                d.set_game_rumble(large, small);
            }
        }),
        Model::DualShock4 => Box::new(move |rep| {
            if let (Some(d), Some(o)) = (r.device(), vp::parse_ds4_output(rep)) {
                if let Some((large, small)) = o.rumble {
                    d.set_game_rumble(large, small);
                }
                if let Some(rgb) = o.rgb {
                    d.set_game_lightbar(rgb);
                }
            }
        }),
        Model::Microphone => Box::new(|_| {}),
    };
    let sink = usbip::DsSink {
        on_output,
        mirror: route.mirror.clone(),
    };
    usbip::UsbPad::create_for(model, sink, mac, before_attach).map_err(|e| {
        log::warn!("usbip-win2: {e}");
        format!(
            "usbip-win2 couldn't attach the virtual {} controller ({e}). Games see the real controller instead.",
            kind.label()
        )
    })
}

/// Send the pad plugged in at startup's idle report again once Windows has
/// it (ring LED lit), then at [`STARTUP_RELEASES`], while nobody drives it.
/// An idle Xbox 360 pad otherwise sends one report in its life, and after
/// some boots Explorer acted on a held Guide while XInput read the pad idle
/// (research/STUCK-GUIDE-2026-10-14.md). A driven pad sends its own.
fn startup_releases(pad: &Pad) {
    let start = Instant::now();
    while pad.target.xinput_slot().is_none() && start.elapsed() < STARTUP_RELEASES[0] {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(STARTUP_RELEASE_AFTER_LED);
    let mut sent = 0;
    for at in [start.elapsed()].into_iter().chain(STARTUP_RELEASES) {
        std::thread::sleep(at.saturating_sub(start.elapsed()));
        if !pad.alive() || pad.owner.lock().is_some() || pad.is_parked() {
            break;
        }
        pad.submit_idle();
        sent += 1;
    }
    log::info!("startup pad: sent idle {sent} more time(s)");
}

impl Pool {
    pub fn list(&self) -> Vec<Arc<Pad>> {
        self.pads.lock().clone()
    }

    /// Plug in a new pad for `device` (or for nobody yet, at startup).
    pub fn create(
        &self,
        kind: VirtualKind,
        mac: &str,
        device: Option<&Arc<Device>>,
    ) -> Result<Arc<Pad>, String> {
        self.create_as(kind, mac, mac, device)
    }

    /// [`Pool::create`], with the identity of the controller with address
    /// `like` (the one expected, for the pad made at startup).
    fn create_as(
        &self,
        kind: VirtualKind,
        mac: &str,
        like: &str,
        device: Option<&Arc<Device>>,
    ) -> Result<Arc<Pad>, String> {
        let route = Arc::new(Route::default());
        if let Some(d) = device {
            *route.owner.lock() = Arc::downgrade(d);
            *route.mirror.lock() = d.mirror.lock().clone();
        }
        let target = Arc::new(plug(kind, &route, like)?);
        let pad = Arc::new(Pad {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            target,
            route,
            mac: Mutex::new(mac.to_string()),
            owner: Mutex::new(device.map(|d| d.id)),
            hidden: Mutex::new(Vec::new()),
            parked: Mutex::new(None),
            raw_check: Mutex::new(RawCheck::new()),
        });
        self.pads.lock().push(pad.clone());
        Ok(pad)
    }

    /// The pad a newly connected controller takes over: the one it had
    /// before, else one that belongs to nobody yet. A DualShock 4 made at
    /// startup for another controller is unplugged instead, so the
    /// controller gets a pad with its own identity.
    pub fn claim(&self, engine: &Engine, device: &Arc<Device>) -> Option<Arc<Pad>> {
        let mac = device.mac.lock().clone();
        let pads = self.pads.lock();
        let free = |p: &&Arc<Pad>| p.owner.lock().is_none() && p.alive() && !p.is_parked();
        let pick = pads
            .iter()
            .filter(free)
            .find(|p| !mac.is_empty() && *p.mac.lock() == mac)
            .or_else(|| pads.iter().filter(free).find(|p| p.mac.lock().is_empty()))
            .cloned()?;
        if pick.kind() == VirtualKind::DualShock4
            && pick.mac.lock().is_empty()
            && usbip::ds4_tag(&mac).is_some_and(|t| pick.target.serial() != usbip::ds4_serial(t))
        {
            drop(pads);
            log::info!("virtual DualShock 4 made for another controller; replacing it");
            self.destroy(engine, &pick);
            return None;
        }
        *pick.owner.lock() = Some(device.id);
        if !mac.is_empty() {
            *pick.mac.lock() = mac;
        }
        *pick.route.owner.lock() = Arc::downgrade(device);
        *pick.route.mirror.lock() = device.mirror.lock().clone();
        Some(pick)
    }

    /// The controller driving `pad` went away. With `keep` the pad stays
    /// plugged in, idle and with the controller still hidden; otherwise it
    /// is unplugged.
    pub fn release(&self, engine: &Engine, pad: &Arc<Pad>, keep: bool) {
        *pad.owner.lock() = None;
        *pad.route.owner.lock() = Weak::new();
        if keep && pad.alive() {
            pad.submit_idle();
            if !pad.is_parked() {
                log::info!(
                    "keeping the virtual {} controller connected while its controller is away",
                    pad.kind().label()
                );
            }
        } else {
            self.destroy(engine, pad);
        }
    }

    /// The parked Xbox pad a newly connected controller takes back: the
    /// one it had, else one that belongs to nobody yet.
    pub fn claim_parked(&self, device: &Arc<Device>) -> Option<Arc<Pad>> {
        let mac = device.mac.lock().clone();
        let pads = self.pads.lock();
        let free = |p: &&Arc<Pad>| p.owner.lock().is_none() && p.alive() && p.is_parked();
        let pick = pads
            .iter()
            .filter(free)
            .find(|p| !mac.is_empty() && *p.mac.lock() == mac)
            .or_else(|| pads.iter().filter(free).find(|p| p.mac.lock().is_empty()))
            .cloned()?;
        *pick.owner.lock() = Some(device.id);
        if !mac.is_empty() {
            *pick.mac.lock() = mac;
        }
        Some(pick)
    }

    /// The parked Xbox pad left by the controller with `mac` while it is
    /// away, if any.
    fn parked_for(&self, mac: &str) -> Option<Arc<Pad>> {
        self.pads
            .lock()
            .iter()
            .find(|p| p.owner.lock().is_none() && p.is_parked() && *p.mac.lock() == mac)
            .cloned()
    }

    /// Whether an idle, unparked pad other than `except` belongs to `mac`.
    fn has_idle_pad(&self, mac: &str, except: &Arc<Pad>) -> bool {
        self.pads.lock().iter().any(|p| {
            !Arc::ptr_eq(p, except)
                && p.owner.lock().is_none()
                && !p.is_parked()
                && *p.mac.lock() == mac
        })
    }

    /// Park an Xbox pad: idle, its game feedback cut off, and hidden from
    /// all but explorer.exe and Game Bar when `hide`. Call again to follow
    /// a change of `hide` and to hide nodes Windows adds below it or let a
    /// newly started Game Bar through; cheap when nothing is due.
    pub fn park(&self, engine: &Engine, pad: &Pad, hide: bool) {
        let mut parked = pad.parked.lock();
        let fresh = parked.is_none();
        if fresh {
            *pad.route.owner.lock() = Weak::new();
            pad.submit_idle();
        }
        let hidden_now = parked.as_ref().is_some_and(|p| !p.ids.is_empty());
        let due = parked.as_ref().is_none_or(|p| {
            let every = if p.since.elapsed() < PARK_WATCH {
                PARK_EVERY_EARLY
            } else {
                PARK_EVERY_LATE
            };
            p.checked.elapsed() >= every
        });
        if !fresh && hidden_now == hide && !(hide && due) {
            return;
        }
        let key = pad.id + PARK_HIDE;
        let since = parked.as_ref().map(|p| p.since).unwrap_or_else(Instant::now);
        if !hide {
            if hidden_now {
                log::info!("parked Xbox 360 controller no longer hidden from games (HidHide off or gone)");
                if let Err(e) = engine.hider.set(key, Vec::new()) {
                    log::warn!("HidHide: {e}");
                }
            }
            *parked = Some(Parked::visible());
            return;
        }
        let serial = pad.target.serial();
        let usb = super::companion::usb_id(&serial);
        let kids = super::win::descendant_ids(&usb);
        let mut ids = parked.as_ref().map(|p| p.ids.clone()).unwrap_or_default();
        for id in std::iter::once(usb).chain(kids.iter().cloned()) {
            if !ids.iter().any(|i| i.eq_ignore_ascii_case(&id)) {
                ids.push(id);
            }
        }
        let apps = super::companion::readers();
        let same = parked.as_ref().is_some_and(|p| p.ids == ids && p.apps == apps);
        if !same {
            if let Err(e) = engine.hider.set_with_apps(key, ids.clone(), apps.clone()) {
                log::warn!("HidHide, parked Xbox controller: {e}");
                *parked = Some(Parked::visible());
                return;
            }
            // Remembered per serial, so the next pad plugged in parked is
            // hidden whole before Windows sees it.
            if !kids.is_empty()
                && engine.settings.read().helper_nodes.get(&serial) != Some(&kids)
            {
                engine.settings.write().helper_nodes.insert(serial, kids);
                engine.settings_save_later();
            }
        }
        let (born_hidden, hidden_at) = match parked.as_ref() {
            Some(p) if hidden_now => (p.born_hidden, p.hidden_at),
            _ => (false, SystemTime::now()),
        };
        *parked = Some(Parked {
            ids,
            apps,
            since,
            checked: Instant::now(),
            born_hidden,
            hidden_at,
        });
    }

    /// Plug in an Xbox pad parked for `device`, hidden from games before
    /// Windows sees it (with the nodes it had last time under that serial).
    pub fn create_parked(
        &self,
        engine: &Engine,
        mac: &str,
        device: &Arc<Device>,
    ) -> Result<Arc<Pad>, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let route = Arc::new(Route::default());
        let apps = super::companion::readers();
        let hidden: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let r = plug_with(VirtualKind::Xbox360, &route, mac, &|serial| {
            // Not the HID child: Windows' raw input opens it only as it
            // arrives, and one hidden then stays out of raw input for good
            // (SDL games would never see the pad once it is back in use).
            // `park` below hides it as soon as it exists.
            let mut ids = vec![super::companion::usb_id(serial)];
            ids.extend(
                engine
                    .settings
                    .read()
                    .helper_nodes
                    .get(serial)
                    .into_iter()
                    .flatten()
                    .filter(|n| !n.to_ascii_uppercase().starts_with(r"HID\"))
                    .cloned(),
            );
            match engine.hider.set_with_apps(id + PARK_HIDE, ids.clone(), apps.clone()) {
                Ok(true) => *hidden.lock() = ids,
                Ok(false) => {}
                Err(e) => log::warn!("HidHide, parked Xbox controller: {e}"),
            }
        });
        let hidden = hidden.into_inner();
        let pre_hidden = !hidden.is_empty();
        let target = match r {
            Ok(t) => Arc::new(t),
            Err(e) => {
                if !hidden.is_empty() {
                    let _ = engine.hider.set(id + PARK_HIDE, Vec::new());
                }
                return Err(e);
            }
        };
        let _ = target.submit_idle();
        let pad = Arc::new(Pad {
            id,
            target,
            route,
            mac: Mutex::new(mac.to_string()),
            owner: Mutex::new(Some(device.id)),
            hidden: Mutex::new(Vec::new()),
            raw_check: Mutex::new(RawCheck::new()),
            parked: Mutex::new(Some(Parked {
                apps: if hidden.is_empty() { Vec::new() } else { apps },
                ids: hidden,
                since: Instant::now(),
                checked: Instant::now() - PARK_EVERY_LATE,
                born_hidden: pre_hidden,
                hidden_at: SystemTime::now(),
            })),
        });
        self.pads.lock().push(pad.clone());
        // Hide the HID child the moment Windows has made it.
        let t = Instant::now();
        while pre_hidden
            && pad.target.hid_instance().is_none()
            && t.elapsed() < Duration::from_millis(1500)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Picks up the nodes Windows adds below it.
        self.park(engine, &pad, true);
        Ok(pad)
    }

    /// Put a parked pad back in service for `device` (or for nobody yet).
    ///
    /// A pad that was hidden while parked is plugged in again once it is
    /// shown, when something may have looked for controllers while it was
    /// hidden (see [`missed_by`]). Taking it off HidHide's list tells
    /// nobody, so without a fresh arrival such a game (an SDL game started
    /// in DualSense mode, then switched to Xbox) never finds it, though it
    /// is in Device Manager and Settings.
    pub fn unpark(&self, engine: &Engine, pad: &Pad, device: Option<&Arc<Device>>) {
        let was = pad.parked.lock().take().filter(|p| !p.ids.is_empty());
        let mut why = None;
        if let Some(p) = &was {
            match engine.hider.set(pad.id + PARK_HIDE, Vec::new()) {
                Ok(_) => why = missed_by(engine, p),
                Err(e) => log::warn!("HidHide, unparking the Xbox controller: {e}"),
            }
        }
        if let Some(d) = device {
            *pad.route.owner.lock() = Arc::downgrade(d);
            *pad.route.mirror.lock() = d.mirror.lock().clone();
        }
        if let (Some(why), true) = (why, pad.alive()) {
            log::info!("virtual Xbox 360 controller shown again; {why}: plugging it in again so games see it arrive");
            match pad.target.replug() {
                Ok(()) => {
                    let mut rc = pad.raw_check.lock();
                    rc.replugged = Some(Instant::now());
                    rc.next = Instant::now() + RAW_SETTLE;
                }
                Err(e) => {
                    log::warn!("virtual Xbox 360 controller replug after unparking: {e}");
                    pad.raw_check.lock().next = Instant::now();
                }
            }
        } else {
            pad.raw_check.lock().next = Instant::now();
        }
    }

    /// Unplug a pad and undo its hiding.
    pub fn destroy(&self, engine: &Engine, pad: &Arc<Pad>) {
        self.pads.lock().retain(|p| !Arc::ptr_eq(p, pad));
        // Unplugged first, so a parked pad is never visible.
        let parked = pad.parked.lock().take();
        if parked.is_some_and(|p| !p.ids.is_empty()) {
            pad.target.detach_now();
            if let Err(e) = engine.hider.set(pad.id + PARK_HIDE, Vec::new()) {
                log::warn!("HidHide: {e}");
            }
        }
        self.unhide(engine, pad);
    }

    /// Hide `ids` (the driving controller's nodes) for this pad.
    pub fn hide(&self, engine: &Engine, pad: &Pad, ids: &[String]) -> Result<bool, String> {
        if *pad.hidden.lock() == ids {
            return Ok(true);
        }
        let r = engine.hider.set(pad.id, ids.to_vec());
        if let Ok(true) = r {
            *pad.hidden.lock() = ids.to_vec();
            let mac = pad.mac.lock().clone();
            if !mac.is_empty()
                && engine
                    .settings
                    .read()
                    .hide_nodes
                    .get(&mac)
                    .map(|v| v.as_slice())
                    != Some(ids)
            {
                engine.settings.write().hide_nodes.insert(mac, ids.to_vec());
                engine.settings_save_later();
            }
        }
        r
    }

    pub fn unhide(&self, engine: &Engine, pad: &Pad) {
        if pad.hidden.lock().is_empty() {
            return;
        }
        if let Err(e) = engine.hider.set(pad.id, Vec::new()) {
            log::warn!("HidHide: {e}");
        }
        pad.hidden.lock().clear();
    }

    /// Unplug everything (app exit).
    pub fn clear(&self, engine: &Engine) {
        for p in self.list() {
            self.destroy(engine, &p);
        }
    }

    /// Plug in the pad the first controller will use, before any controller
    /// connects, so it gets the first free XInput slot. Runs once.
    pub fn startup(&self, engine: &Engine) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let (on, last) = {
            let s = engine.settings.read();
            (s.persistent_virtual, s.last_controller.clone())
        };
        if !on || !engine.device_list().is_empty() || !self.pads.lock().is_empty() {
            return;
        }
        let profile = engine.profile_for_mac(&last);
        let kind = crate::settings::effective_virtual_kind(
            profile.virtual_out.kind,
            engine.game_override.read().is_some(),
            engine.fallback_exe.read().is_some(),
            engine.settings.read().fallback_pad,
        );
        if kind == VirtualKind::Off {
            return;
        }
        match self.create_as(kind, "", &last, None) {
            Ok(p) => {
                p.submit_idle();
                log::info!(
                    "virtual {} controller plugged in at startup ({})",
                    kind.label(),
                    p.target.backend()
                );
                // Hide the controller now, before it connects, so nothing
                // (Steam, a game) grabs the real one in the meantime.
                let ids = engine.settings.read().hide_nodes.get(&last).cloned();
                let allowed = profile.virtual_out.hide_physical
                    && engine.settings.read().hidhide_control
                    && hidhide::available();
                if let (Some(ids), true) = (ids, allowed) {
                    match self.hide(engine, &p, &ids) {
                        Ok(_) => log::info!("HidHide: hid the last controller before it connected"),
                        Err(e) => log::warn!("HidHide at startup: {e}"),
                    }
                }
                if kind == VirtualKind::Xbox360 {
                    std::thread::spawn(move || startup_releases(&p));
                }
            }
            Err(e) => log::warn!("virtual {} at startup: {e}", kind.label()),
        }
    }

    /// Keep pads nobody drives in line with settings and profiles: unplug
    /// them when persistence is off or their profile no longer wants that
    /// kind (a game rule, an edit), replug when it wants the other kind.
    /// Plug an Xbox pad in use in again when Windows' raw input has lost
    /// it. Raw input opens a device only as it arrives, so one that was
    /// hidden then (or knocked off some other way) stays invisible to games
    /// that read controllers through raw input, SDL's default, even though
    /// XInput still sees it. A replug is a fresh arrival.
    fn heal(&self) {
        for p in self.list() {
            if p.kind() != VirtualKind::Xbox360 || !p.alive() || p.is_parked() {
                continue;
            }
            let mut rc = p.raw_check.lock();
            let now = Instant::now();
            if now < rc.next {
                continue;
            }
            rc.next = now + RAW_CHECK_EVERY;
            if p.target.attached_for() < RAW_SETTLE {
                continue;
            }
            let Some(hid) = p.target.hid_instance() else {
                continue;
            };
            if super::win::raw_input_lists(&hid)
                || rc.replugged.is_some_and(|t| t.elapsed() < REPLUG_AGAIN_AFTER)
            {
                continue;
            }
            rc.replugged = Some(now);
            drop(rc);
            log::info!("virtual Xbox 360 controller missing from raw input (games using SDL can't see it): plugging it in again");
            if let Err(e) = p.target.replug() {
                log::warn!("virtual Xbox 360 controller replug: {e}");
            }
        }
    }

    pub fn maintain(&self, engine: &Engine) {
        self.heal();
        let (on, hide_ok, last, fallback_pad, keep_xbox) = {
            let s = engine.settings.read();
            (
                s.persistent_virtual,
                s.hidhide_control,
                s.last_controller.clone(),
                s.fallback_pad,
                s.keep_xbox_pad,
            )
        };
        let park_hide = hide_ok && hidhide::available();
        let forced = engine.game_override.read().is_some();
        let falling_back = engine.fallback_exe.read().is_some();
        let idle: Vec<Arc<Pad>> = self
            .list()
            .into_iter()
            .filter(|p| p.owner.lock().is_none())
            .collect();
        for p in idle {
            let mac = p.mac.lock().clone();
            let profile = engine.profile_for_mac(if mac.is_empty() { &last } else { &mac });
            let want = crate::settings::effective_virtual_kind(
                profile.virtual_out.kind,
                forced,
                falling_back,
                fallback_pad,
            );
            if p.is_parked() {
                if !on || !p.alive() {
                    self.destroy(engine, &p);
                } else if want == VirtualKind::Xbox360 {
                    // Its profile wants the Xbox pad again. With another
                    // idle pad standing in, that pad's swap brings it back.
                    if !self.has_idle_pad(&mac, &p) {
                        self.unpark(engine, &p, None);
                        p.submit_idle();
                        let ids = engine.settings.read().hide_nodes.get(&mac).cloned();
                        if let (Some(ids), true) = (ids, park_hide && profile.virtual_out.hide_physical) {
                            let _ = self.hide(engine, &p, &ids);
                        }
                        log::info!("virtual Xbox 360 controller back in use while its controller is away");
                    }
                } else if super::park_allowed(keep_xbox, want, park_hide) {
                    self.park(engine, &p, park_hide);
                } else {
                    self.destroy(engine, &p);
                }
                continue;
            }
            if p.kind() == VirtualKind::Xbox360
                && on
                && p.alive()
                && want == VirtualKind::Off
                && super::park_allowed(keep_xbox, want, park_hide)
                && self.parked_for(&mac).is_none()
            {
                self.park(engine, &p, park_hide);
                self.unhide(engine, &p);
                log::info!("virtual Xbox 360 controller parked while its controller is away");
                continue;
            }
            if !on || !p.alive() || want == VirtualKind::Off {
                log::info!("virtual {} controller unplugged", p.kind().label());
                self.destroy(engine, &p);
                continue;
            }
            if !hide_ok || !profile.virtual_out.hide_physical || !hidhide::available() {
                self.unhide(engine, &p);
            }
            if want != p.kind() {
                let hidden = p.hidden.lock().clone();
                // Keep the controller hidden while the pads swap, as the
                // controller's own service does (`virt::run`).
                let bridged = !hidden.is_empty()
                    && engine.hider.set(SWAP_HIDE_IDLE, hidden.clone()).unwrap_or(false);
                let back = (want == VirtualKind::Xbox360)
                    .then(|| self.parked_for(&mac))
                    .flatten();
                if let Some(x) = back {
                    // The parked Xbox pad takes over again: no new pad.
                    self.destroy(engine, &p);
                    self.unpark(engine, &x, None);
                    x.submit_idle();
                    if !hidden.is_empty() {
                        let _ = self.hide(engine, &x, &hidden);
                    }
                    log::info!("virtual Xbox 360 controller back in use while its controller is away");
                    if bridged {
                        let _ = engine.hider.set(SWAP_HIDE_IDLE, Vec::new());
                    }
                    continue;
                }
                if p.kind() == VirtualKind::Xbox360
                    && super::park_allowed(keep_xbox, want, park_hide)
                    && self.parked_for(&mac).is_none()
                {
                    self.park(engine, &p, park_hide);
                    self.unhide(engine, &p);
                    log::info!("virtual Xbox 360 controller parked while its controller is away");
                } else {
                    self.destroy(engine, &p);
                }
                if want == VirtualKind::Xbox360 {
                    engine.companion.make_way();
                }
                match self.create(want, &mac, None) {
                    Ok(n) => {
                        n.submit_idle();
                        if !hidden.is_empty() {
                            let _ = self.hide(engine, &n, &hidden);
                        }
                        log::info!(
                            "virtual controller switched to {} while its controller is away",
                            want.label()
                        );
                    }
                    Err(e) => log::warn!("virtual {}: {e}", want.label()),
                }
                if bridged {
                    if let Err(e) = engine.hider.set(SWAP_HIDE_IDLE, Vec::new()) {
                        log::warn!("HidHide: {e}");
                    }
                }
            }
        }
    }
}
