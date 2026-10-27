//! Virtual controllers: an Xbox 360 pad or a USB DualSense that games see
//! in place of the physical controller, plus hiding the physical one.
//!
//! Backends:
//! - Every virtual pad (DualSense with game haptics, Xbox 360, DualShock 4)
//!   is our own wired USB device, served over USB/IP to usbip-win2
//!   (`usbip`); Windows' own drivers take it from there.
//! - Hiding: HidHide when installed.
//!
//! Nothing here installs a driver. Whatever is already on the machine is used.
//!
//! ```text
//! reader thread ─ pipeline ─► PadState ─► Slot::submit ─► usbip-win2
//!                                                            │ rumble, triggers, LEDs, PCM
//! virt thread (per controller): create/destroy, HidHide ◄────┘ into Feedback, game_audio
//! ```

pub mod audio_default;
pub mod companion;
pub mod gamebar;
pub mod mirror;
pub mod hidhide;
pub mod pool;
pub mod probe;
pub mod shape;
pub mod usbip;
pub mod win;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ds_proto::virtual_pad::{self as vp, PadState, XusbReport};
use parking_lot::{Mutex, RwLock};

use crate::device::Device;
use crate::engine::{Engine, ProfileCache};
use crate::haptics::game::PcmSource;
use crate::profile::{Action, VirtualKind};

pub use pool::{Pad, Pool};
pub use probe::probe;

impl crate::haptics::capture::PcmSink for usbip::MicrophoneRing {
    fn push(&self, pcm: &[i16]) {
        usbip::MicrophoneRing::push(self, pcm);
    }

    fn clear(&self) {
        usbip::MicrophoneRing::clear(self);
    }
}

/// One live virtual controller.
pub type Target = usbip::UsbPad;

impl usbip::UsbPad {
    pub fn kind(&self) -> VirtualKind {
        match self.model() {
            usbip::Model::DualSense => VirtualKind::DualSense,
            usbip::Model::Xbox360 => VirtualKind::Xbox360,
            usbip::Model::DualShock4 => VirtualKind::DualShock4,
            // The microphone device is no game pad.
            usbip::Model::Microphone => VirtualKind::Off,
        }
    }

    pub fn backend(&self) -> &'static str {
        "usbip-win2"
    }

    /// The game's audio stream, when this pad carries one.
    pub fn game_audio(&self) -> Option<Arc<dyn PcmSource>> {
        (self.model() == usbip::Model::DualSense).then(|| self.audio() as Arc<dyn PcmSource>)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    /// What the profile asks for.
    pub want: Option<VirtualKind>,
    pub active: Option<VirtualKind>,
    pub backend: &'static str,
    /// XInput slot of the virtual Xbox pad.
    pub xinput_slot: Option<u32>,
    /// XInput slot of the parked Xbox pad, and whether games can't see it.
    pub parked: Option<(Option<u32>, bool)>,
    pub error: Option<String>,
    pub hidden: bool,
    pub hide_note: Option<String>,
    /// A game has the virtual DualSense's audio endpoint open.
    pub game_audio_open: bool,
    /// The microphone device couldn't be plugged in (retried later).
    pub microphone_failed: bool,
}

/// Per-controller handle the reader thread submits to.
#[derive(Default)]
pub struct Slot {
    target: RwLock<Option<Arc<Target>>>,
    /// The parked Xbox pad: Xbox buttons, and the whole pad while Game Bar
    /// is open.
    parked: RwLock<Option<Arc<Target>>>,
    /// The microphone device beside a pad that has no microphone.
    mic: RwLock<Option<Arc<Target>>>,
    parked_sent: Mutex<Option<XusbReport>>,
    last_xusb: Mutex<Option<XusbReport>>,
    seq: AtomicU8,
    pub reports: AtomicU64,
    pub errors: AtomicU64,
    pub status: Mutex<Status>,
}

impl Slot {
    pub fn active(&self) -> Option<VirtualKind> {
        self.target.read().as_ref().map(|t| t.kind())
    }

    /// An app is recording from this controller's virtual DualSense, or
    /// from the microphone device beside another pad.
    pub fn microphone_open(&self) -> bool {
        self.target
            .read()
            .as_ref()
            .is_some_and(|t| t.kind() == VirtualKind::DualSense && t.recording())
            || self.mic.read().as_ref().is_some_and(|m| m.recording())
    }

    /// Windows has a recording device for this controller's microphone.
    pub fn microphone_ready(&self) -> bool {
        self.active() == Some(VirtualKind::DualSense) || self.mic.read().is_some()
    }

    pub fn has_parked(&self) -> bool {
        self.parked.read().is_some()
    }

    fn set_parked(&self, t: Option<Arc<Target>>) {
        let mut sent = self.parked_sent.lock();
        let old = std::mem::replace(&mut *self.parked.write(), t);
        // Nothing stays held on a pad this controller lets go of.
        if let (Some(old), Some(rep)) = (old, *sent) {
            if rep != vp::xusb_report(&PadState::default()) {
                let _ = old.submit_idle();
            }
        }
        *sent = None;
    }

    /// Send `pad` to the parked Xbox pad: Xbox buttons only (see
    /// [`companion::pad`]), or the whole pad while Game Bar is open. False
    /// when there is none.
    pub fn submit_parked(&self, pad: &PadState) -> bool {
        // The same lock order as set_parked, so a pad let go of gets no
        // report after its idle one.
        let mut sent = self.parked_sent.lock();
        let t = self.parked.read().clone();
        let Some(t) = t else { return false };
        let rep = vp::xusb_report(pad);
        if *sent != Some(rep) {
            match t.submit(&vp::x360_usb_input(&rep)) {
                Ok(()) => *sent = Some(rep),
                Err(e) => log::debug!("parked Xbox controller report: {e}"),
            }
        }
        true
    }

    fn xusb_changed(&self, rep: XusbReport) -> bool {
        let mut last = self.last_xusb.lock();
        if *last == Some(rep) {
            return false;
        }
        *last = Some(rep);
        true
    }

    /// Send the processed pad to the virtual controller, if one is up.
    /// `raw` is the physical report in USB layout, for motion and touch.
    pub fn submit(&self, pad: &PadState, raw: Option<&[u8; vp::DS_USB_INPUT_LEN]>, touch: bool) {
        let t = self.target.read().clone();
        let Some(t) = t else { return };
        let r = match t.model() {
            usbip::Model::Xbox360 => {
                let rep = vp::xusb_report(pad);
                if !self.xusb_changed(rep) {
                    return;
                }
                t.submit(&vp::x360_usb_input(&rep))
            }
            usbip::Model::DualShock4 => {
                let seq = self.seq.fetch_add(1, Ordering::Relaxed);
                t.submit(&vp::ds4_usb_input(pad, raw, touch, seq))
            }
            usbip::Model::DualSense => {
                let seq = self.seq.fetch_add(1, Ordering::Relaxed);
                t.submit(&vp::ds_usb_input(pad, raw, touch, seq))
            }
            usbip::Model::Microphone => return,
        };
        match r {
            Ok(()) => {
                self.reports.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                if self
                    .errors
                    .fetch_add(1, Ordering::Relaxed)
                    .is_multiple_of(500)
                {
                    log::warn!("virtual controller report failed: {e}");
                }
            }
        }
    }

    fn set(&self, t: Option<Arc<Target>>) {
        *self.last_xusb.lock() = None;
        *self.target.write() = t;
    }
}

// ------------------------------------------------------------------ hiding

/// HidHide changes for every controller, applied as one set and recorded
/// on disk so a crash can be undone at the next start.
pub struct Hider {
    file: PathBuf,
    state: Mutex<HiderState>,
}

#[derive(Default)]
struct HiderState {
    /// Per requester: nodes to hide and other apps to let through.
    want: BTreeMap<u64, (Vec<String>, Vec<PathBuf>)>,
    applied_for: Vec<String>,
    applied_apps: Vec<PathBuf>,
    applied: hidhide::Changes,
}

impl Hider {
    pub fn new(data_dir: &std::path::Path) -> Hider {
        let file = data_dir.join("hidhide-changes.json");
        let mut state = HiderState::default();
        // Undo whatever a previous run left behind.
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Ok(ch) = serde_json::from_str::<hidhide::Changes>(&text) {
                match hidhide::restore(&ch) {
                    Ok(()) => {
                        log::info!(
                            "HidHide: undid leftovers from the last run ({} node(s))",
                            ch.devices_added.len()
                        );
                        let _ = std::fs::remove_file(&file);
                    }
                    Err(e) => {
                        // Keep the record: forgotten entries would stay on
                        // HidHide's list for good. They are undone with the
                        // next change, and at exit.
                        log::warn!("HidHide: could not undo leftovers from the last run, will retry: {e}");
                        state.applied = ch;
                    }
                }
            } else {
                let _ = std::fs::remove_file(&file);
            }
        }
        Hider {
            file,
            state: Mutex::new(state),
        }
    }

    /// At startup, before any virtual pad exists: take this app's virtual
    /// pads off HidHide's list. None of them should be there now, and one
    /// left behind (a crash before it was recorded, or a record lost by an
    /// older version) would hide the next pad with that serial from every
    /// game for good, since only what this app put on is ever taken off.
    /// `pad_nodes`: nodes Windows made below our pads on earlier runs
    /// (their ids don't carry the pad's serial).
    pub fn clear_stale_pads(&self, pad_nodes: &[String]) {
        if !hidhide::available() {
            return;
        }
        let st = self.state.lock();
        if !st.want.is_empty() {
            return;
        }
        match hidhide::blocklist() {
            Ok(list) => {
                let stale = stale_pad_entries(&list, pad_nodes, &st.applied.devices_added);
                if !stale.is_empty() {
                    let undo = hidhide::Changes {
                        devices_added: stale.clone(),
                        ..Default::default()
                    };
                    match hidhide::restore(&undo) {
                        Ok(()) => log::info!(
                            "HidHide: took {} stale virtual controller node(s) off the block list: {stale:?}",
                            stale.len()
                        ),
                        Err(e) => log::warn!("HidHide: could not take stale virtual controller nodes off the block list: {e}"),
                    }
                }
            }
            Err(e) => log::warn!("HidHide: could not read the block list: {e}"),
        }
    }

    /// Ask for `ids` to be hidden on behalf of controller `dev` (empty to
    /// release). Returns whether they are hidden now.
    pub fn set(&self, dev: u64, ids: Vec<String>) -> Result<bool, String> {
        self.set_with_apps(dev, ids, Vec::new())
    }

    /// [`Hider::set`], also letting `apps` see what is hidden while this
    /// request stands. Changes go in as a difference, so nodes that stay
    /// hidden are never shown in between.
    pub fn set_with_apps(&self, dev: u64, ids: Vec<String>, apps: Vec<PathBuf>) -> Result<bool, String> {
        let mut st = self.state.lock();
        if ids.is_empty() {
            st.want.remove(&dev);
        } else {
            st.want.insert(dev, (ids, apps));
        }
        let mut union: Vec<String> = st.want.values().flat_map(|w| w.0.iter()).cloned().collect();
        union.sort();
        union.dedup();
        let mut union_apps: Vec<PathBuf> = st.want.values().flat_map(|w| w.1.iter()).cloned().collect();
        union_apps.sort();
        union_apps.dedup();
        if union == st.applied_for && union_apps == st.applied_apps {
            return Ok(!union.is_empty());
        }
        if union.is_empty() {
            if !st.applied.is_empty() {
                hidhide::restore(&st.applied).map_err(|e| e.to_string())?;
            }
            st.applied = Default::default();
            st.applied_for.clear();
            st.applied_apps.clear();
            let _ = std::fs::remove_file(&self.file);
            log::info!("HidHide: physical controller visible again");
            return Ok(false);
        }
        // Apps first, so a reader is let through before its device hides.
        let new_apps: Vec<PathBuf> = union_apps.iter().filter(|a| !st.applied_apps.contains(a)).cloned().collect();
        for a in &new_apps {
            match hidhide::set_app(a, true) {
                Ok(Some(dos)) => st.applied.apps_added.push(dos),
                Ok(None) => {}
                Err(e) => log::warn!("HidHide: couldn't let {} through: {e}", a.display()),
            }
        }
        let gone_apps: Vec<PathBuf> = st.applied_apps.iter().filter(|a| !union_apps.contains(a)).cloned().collect();
        for a in gone_apps {
            if let Some(dos) = win::dos_device_path(&a.display().to_string()) {
                if let Some(i) = st.applied.apps_added.iter().position(|x| x.eq_ignore_ascii_case(&dos)) {
                    let _ = hidhide::set_app(&a, false);
                    st.applied.apps_added.remove(i);
                }
            }
        }
        let added: Vec<String> = union.iter().filter(|i| !st.applied_for.contains(i)).cloned().collect();
        if !added.is_empty() {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let ch = hidhide::hide(&added, &exe).map_err(|e| e.to_string())?;
            st.applied.devices_added.extend(ch.devices_added);
            if st.applied.app_added.is_none() {
                st.applied.app_added = ch.app_added;
            }
            st.applied.activated |= ch.activated;
        }
        let removed: Vec<String> = st
            .applied
            .devices_added
            .iter()
            .filter(|d| !union.iter().any(|u| u.eq_ignore_ascii_case(d)))
            .cloned()
            .collect();
        if !removed.is_empty() {
            let undo = hidhide::Changes {
                devices_added: removed.clone(),
                ..Default::default()
            };
            hidhide::restore(&undo).map_err(|e| e.to_string())?;
            st.applied.devices_added.retain(|d| !removed.contains(d));
        }
        if let Ok(t) = serde_json::to_string_pretty(&st.applied) {
            let _ = std::fs::write(&self.file, t);
        }
        if st.applied_for.is_empty() {
            log::info!("HidHide: hiding {} node(s) from other apps", union.len());
        }
        st.applied_for = union;
        st.applied_apps = union_apps;
        Ok(true)
    }

    pub fn release_all(&self) {
        let mut st = self.state.lock();
        st.want.clear();
        if !st.applied.is_empty() {
            match hidhide::restore(&st.applied) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&self.file);
                }
                // The record stays on disk, so the next start undoes it.
                Err(e) => log::warn!("HidHide: restore failed, will retry at the next start: {e}"),
            }
            st.applied = Default::default();
        }
        st.applied_for.clear();
        st.applied_apps.clear();
    }
}

/// Block-list entries that belong to this app's virtual pads (a serial of
/// ours, or a node recorded below one) and that `pending` doesn't already
/// cover. Only called while no virtual pad exists.
fn stale_pad_entries(list: &[String], pad_nodes: &[String], pending: &[String]) -> Vec<String> {
    list.iter()
        .filter(|d| {
            usbip::has_our_serial(d) || pad_nodes.iter().any(|n| n.eq_ignore_ascii_case(d))
        })
        .filter(|d| !pending.iter().any(|p| p.eq_ignore_ascii_case(d)))
        .cloned()
        .collect()
}

/// HidHide block-list ids for a controller: its HID node and the parent
/// (the Bluetooth HID service, or the USB composite).
fn hide_ids(device: &Device) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(id) = win::instance_id_for_interface(&device.info.path) {
        if let Some(p) = win::parent_instance_id(&id) {
            v.push(p);
        }
        v.push(id);
    }
    v
}

// ------------------------------------------------------------------ manager

const RETRY_AFTER: Duration = Duration::from_secs(30);
/// Hider key (plus the controller's id) that keeps a controller hidden
/// while one virtual pad replaces another. Pad ids start at 1 << 40.
const SWAP_HIDE: u64 = 1 << 39;
const NO_HIDHIDE: &str =
    "Install HidHide to hide the physical controller; until then games may see both.";

/// Where the controller's microphone goes: the virtual DualSense's own
/// capture endpoint, else the microphone device beside another pad.
fn microphone_sink(
    pad: Option<&Target>,
    mic: Option<&Target>,
) -> Option<Arc<dyn crate::haptics::capture::PcmSink>> {
    let ring = match pad.filter(|t| t.kind() == VirtualKind::DualSense) {
        Some(t) => Some(t.microphone()),
        None => mic.map(|m| m.microphone()),
    };
    ring.map(|r| r as Arc<dyn crate::haptics::capture::PcmSink>)
}

/// Point `device` at `pad`: input goes to it, its game audio comes back.
fn attach(device: &Device, pad: &Pad) {
    if let Some(a) = pad.target.game_audio() {
        *device.game_audio.lock() = Some(a);
    }
    let mic = microphone_sink(Some(&pad.target), device.virt.mic.read().as_deref());
    device.microphone.set_sink(mic);
    device.virt.set(Some(pad.target.clone()));
    device.kick();
}

fn detach(device: &Device) {
    device.microphone.set_sink(microphone_sink(None, device.virt.mic.read().as_deref()));
    device.microphone.clear();
    device.virt.set(None);
    *device.game_audio.lock() = None;
    device.feedback.lock().clear_game();
    device.kick();
}

/// Plug in or take away the microphone device. Capture follows it unless
/// the virtual DualSense has it.
fn set_microphone_device(device: &Device, mic: Option<Arc<Target>>) {
    *device.virt.mic.write() = mic;
    if device.virt.active() != Some(VirtualKind::DualSense) {
        device
            .microphone
            .set_sink(microphone_sink(None, device.virt.mic.read().as_deref()));
    }
    device.kick();
}

/// Whether the controller needs the microphone device: the profile turns
/// the microphone on, the controller sends one (a DualSense over
/// Bluetooth), and the virtual pad in use has none of its own. A virtual
/// DualSense carries the microphone itself, and with no virtual pad the
/// app adds no devices.
pub(crate) fn wants_microphone_device(
    enabled: bool,
    bluetooth_dualsense: bool,
    pad: Option<VirtualKind>,
) -> bool {
    enabled
        && bluetooth_dualsense
        && matches!(pad, Some(VirtualKind::Xbox360 | VirtualKind::DualShock4))
}

fn plug_microphone() -> Result<Target, String> {
    if let Some(why) = usbip::blocked() {
        return Err(why);
    }
    if !usbip::available() {
        return Err("usbip-win2 is not installed".into());
    }
    let sink = usbip::DsSink {
        on_output: Box::new(|_| {}),
        mirror: Default::default(),
    };
    usbip::UsbPad::create(usbip::Model::Microphone, sink).map_err(|e| e.to_string())
}

/// Per-controller service: keeps the virtual pad the profile asks for,
/// hides the physical one, and relays game feedback. Blocks until the
/// controller goes away; the pad itself may stay (see [`pool`]).
///
/// With "keep the Xbox pad" on (the default), a virtual Xbox pad is parked
/// rather than unplugged when the profile switches to another kind: it
/// stays plugged in, idle and hidden from games, so it keeps its XInput
/// slot and presses Xbox buttons in place of the helper pad.
pub fn run(engine: &Arc<Engine>, device: &Arc<Device>) {
    let mut cache = ProfileCache::default();
    let mut failed: Option<(VirtualKind, Instant, String)> = None;
    let mut park_failed: Option<Instant> = None;
    let mut mic: Option<Arc<Target>> = None;
    let mut mic_failed: Option<Instant> = None;
    let ids = hide_ids(device);

    // Take over the pads this controller had before it went away, or the
    // one plugged in at startup.
    let mut pad = engine.pads.claim(engine, device);
    if let Some(p) = &pad {
        attach(device, p);
        log::info!(
            "{} is driving the virtual {} controller",
            device.model.name(),
            p.kind().label()
        );
    }
    let mut parked = engine.pads.claim_parked(device);
    if let Some(p) = &parked {
        device.virt.set_parked(Some(p.target.clone()));
    }

    while !device.stop.load(Ordering::Acquire) {
        let profile = cache.get(engine, device);
        let vo = &profile.virtual_out;
        let (fallback_pad, keep_xbox, hide_ok) = {
            let s = engine.settings.read();
            (s.fallback_pad, s.keep_xbox_pad, s.hidhide_control && hidhide::available())
        };
        let want = crate::settings::effective_virtual_kind(
            vo.kind,
            engine.game_override.read().is_some(),
            engine.fallback_exe.read().is_some(),
            fallback_pad,
        );
        let park_ok = park_allowed(keep_xbox, want, hide_ok);
        let lost = pad.as_ref().filter(|p| !p.target.alive()).cloned();
        if let Some(p) = lost {
            let e = format!(
                "The virtual {} controller was detached by Windows or the usbip-win2 driver; trying again in 30 seconds.",
                p.kind().label()
            );
            log::warn!("{e}");
            detach(device);
            engine.pads.destroy(engine, &p);
            pad = None;
            engine.warn_toast(e.clone());
            failed = Some((p.kind(), Instant::now(), e));
        }
        let have = pad.as_ref().map(|p| p.kind());
        let want_some = (want != VirtualKind::Off).then_some(want);

        let mut swap_hidden = false;
        if have != want_some {
            if let Some(p) = pad.take() {
                // Keep the controller hidden until the new pad hides it.
                // Unplugging the old pad undoes its hiding, and a game
                // starting right then (the usual reason for a swap) can open
                // the physical controller in that gap.
                let ids = p.hidden_ids();
                if want_some.is_some() && !ids.is_empty() {
                    swap_hidden = engine.hider.set(SWAP_HIDE + device.id, ids).unwrap_or(false);
                }
                if p.kind() == VirtualKind::Xbox360 && park_ok && parked.is_none() {
                    // Parked before the controller lets go of it, so the
                    // helper pad never sees a moment without an Xbox pad.
                    device.virt.set_parked(Some(p.target.clone()));
                    detach(device);
                    engine.pads.park(engine, &p, hide_ok);
                    engine.pads.unhide(engine, &p);
                    log::info!(
                        "virtual Xbox 360 controller parked: plugged in and idle{}",
                        if p.park_hidden() { ", hidden from games" } else { "" }
                    );
                    parked = Some(p);
                } else {
                    detach(device);
                    engine.pads.destroy(engine, &p);
                    engine.toast(crate::i18n::tr("Virtual controller disconnected"));
                }
            }
            if let Some(want) = want_some {
                if want == VirtualKind::Xbox360 && parked.is_some() {
                    let p = parked.take().expect("checked");
                    device.virt.set_parked(None);
                    engine.pads.unpark(engine, &p, Some(device));
                    attach(device, &p);
                    log::info!("virtual Xbox 360 controller back in use");
                    pad = Some(p);
                    failed = None;
                } else {
                    let retry = match &failed {
                        Some((k, at, _)) => *k != want || at.elapsed() >= RETRY_AFTER,
                        None => true,
                    };
                    if retry {
                        if want == VirtualKind::Xbox360 {
                            // Player 1 goes to the controller, not the helper.
                            engine.companion.make_way();
                        }
                        let mac = device.mac.lock().clone();
                        match engine.pads.create(want, &mac, Some(device)) {
                            Ok(p) => {
                                attach(device, &p);
                                engine.toast(crate::localized_format!(
                                    "Virtual {} controller connected ({})", "Controle virtual {} conectado ({})", "Mando virtual {} conectado ({})",
                                    p.kind().label(),
                                    p.target.backend()
                                ));
                                pad = Some(p);
                                failed = None;
                            }
                            Err(e) => {
                                if failed
                                    .as_ref()
                                    .map(|f| f.0 != want || f.2 != e)
                                    .unwrap_or(true)
                                {
                                    log::warn!("virtual {}: {e}", want.label());
                                    engine.warn_toast(e.clone());
                                }
                                failed = Some((want, Instant::now(), e));
                            }
                        }
                    }
                }
            }
        }

        // The parked pad: kept while allowed, its hiding kept current, and
        // plugged in parked when Xbox buttons need one and none is there.
        if let Some(p) = parked.clone() {
            if !park_ok || !p.target.alive() {
                device.virt.set_parked(None);
                engine.pads.destroy(engine, &p);
                parked = None;
            } else {
                engine.pads.park(engine, &p, hide_ok);
            }
        } else if park_ok
            && hide_ok
            && want != VirtualKind::Xbox360
            && usbip::available()
            && park_failed.is_none_or(|t| t.elapsed() >= RETRY_AFTER)
            && profile
                .mappings
                .iter()
                .any(|m| m.enabled && matches!(m.action, Action::Xbox { .. }))
        {
            engine.companion.make_way();
            let mac = device.mac.lock().clone();
            match engine.pads.create_parked(engine, &mac, device) {
                Ok(p) => {
                    device.virt.set_parked(Some(p.target.clone()));
                    log::info!("virtual Xbox 360 controller plugged in parked, for Xbox buttons");
                    parked = Some(p);
                    park_failed = None;
                }
                Err(e) => {
                    if park_failed.is_none() {
                        log::warn!("parked Xbox controller: {e}");
                    }
                    park_failed = Some(Instant::now());
                }
            }
        }

        // Hiding follows the live virtual pad, and stays with the pad while
        // the controller is away.
        let want_hide = pad.is_some() && vo.hide_physical && engine.settings.read().hidhide_control;
        let mut hide_note = None;
        if let Some(p) = &pad {
            if want_hide && !hidhide::available() {
                hide_note = Some(NO_HIDHIDE.to_string());
            } else if want_hide && ids.is_empty() {
                hide_note =
                    Some("Couldn't find this controller's device node to hide.".to_string());
            } else if want_hide {
                if let Err(e) = engine.pads.hide(engine, p, &ids) {
                    log::warn!("HidHide: {e}");
                    hide_note = Some(format!("HidHide: {e}"));
                }
            } else {
                engine.pads.unhide(engine, p);
            }
        }
        if swap_hidden {
            if let Err(e) = engine.hider.set(SWAP_HIDE + device.id, Vec::new()) {
                log::warn!("HidHide: {e}");
            }
        }

        // The microphone device, while the pad in use has no microphone.
        let mic_wanted = wants_microphone_device(
            profile.microphone.enabled,
            device.is_bluetooth() && !device.model.is_dualshock4(),
            pad.as_ref().map(|p| p.kind()),
        );
        let mic_lost = mic.as_ref().is_some_and(|m| !m.alive());
        if mic.is_some() && (!mic_wanted || mic_lost) {
            set_microphone_device(device, None);
            mic = None;
            if mic_lost {
                log::warn!("the controller microphone device was detached by Windows or the usbip-win2 driver");
                mic_failed = Some(Instant::now());
            } else {
                log::info!("controller microphone device removed");
            }
        } else if mic_wanted
            && mic.is_none()
            && mic_failed.is_none_or(|t| t.elapsed() >= RETRY_AFTER)
        {
            match plug_microphone() {
                Ok(m) => {
                    let m = Arc::new(m);
                    set_microphone_device(device, Some(m.clone()));
                    log::info!(
                        "controller microphone plugged in beside the virtual {} controller",
                        pad.as_ref().map(|p| p.kind().label()).unwrap_or_default()
                    );
                    mic = Some(m);
                    mic_failed = None;
                }
                Err(e) => {
                    if mic_failed.is_none() {
                        log::warn!("controller microphone device: {e}");
                        engine.warn_toast(crate::i18n::tr(
                            "Couldn't add the controller microphone to Windows; trying again in 30 seconds.",
                        ));
                    }
                    mic_failed = Some(Instant::now());
                }
            }
        }
        if !mic_wanted {
            mic_failed = None;
        }

        // Status for the UI.
        {
            let target = pad.as_ref().map(|p| p.target.clone());
            let mut st = device.virt.status.lock();
            st.want = want_some;
            st.active = target.as_ref().map(|t| t.kind());
            st.backend = target.as_ref().map(|t| t.backend()).unwrap_or("");
            st.error = failed
                .as_ref()
                .filter(|f| Some(f.0) == want_some && st.active.is_none())
                .map(|f| f.2.clone());
            st.hidden = pad.as_ref().map(|p| p.is_hidden()).unwrap_or(false);
            st.hide_note = hide_note;
            st.game_audio_open = target
                .as_ref()
                .map(|t| t.kind() == VirtualKind::DualSense && t.streaming())
                .unwrap_or(false);
            st.xinput_slot = target
                .as_ref()
                .filter(|t| t.kind() == VirtualKind::Xbox360)
                .and_then(|t| t.xinput_slot());
            st.parked = parked
                .as_ref()
                .map(|p| (p.target.xinput_slot(), p.park_hidden()));
            st.microphone_failed = mic_failed.is_some();
        }

        // Game feedback arrives on the pad's own threads (callbacks).
        std::thread::sleep(Duration::from_millis(50));
    }

    if mic.take().is_some() {
        set_microphone_device(device, None);
    }
    detach(device);
    device.virt.set_parked(None);
    let keep =
        engine.settings.read().persistent_virtual && !engine.shutdown.load(Ordering::Acquire);
    if let Some(p) = pad {
        engine.pads.release(engine, &p, keep);
    }
    if let Some(p) = parked {
        engine.pads.release(engine, &p, keep);
    }
}

/// Whether a parked Xbox pad may stay while the profile wants `want`:
/// beside a DualSense or DualShock 4 pad, and with no virtual pad only
/// while games can't see it.
pub(crate) fn park_allowed(keep_xbox: bool, want: VirtualKind, hide_ok: bool) -> bool {
    keep_xbox
        && match want {
            VirtualKind::Xbox360 => false,
            VirtualKind::DualSense | VirtualKind::DualShock4 => true,
            VirtualKind::Off => hide_ok,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_entries_are_only_our_pads() {
        let list: Vec<String> = [
            r"USB\VID_045E&PID_028E\DSZV1",
            r"HID\VID_045E&PID_028E&IG_00\8&2C1B3F1&0&0000",
            r"USB\VID_054C&PID_05C4\0244535A0001",
            r"USB\VID_054C&PID_0CE6\DSZV2",
            // The physical controller and someone else's entries stay.
            r"HID\{00001124-0000-1000-8000-00805F9B34FB}_VID&0002054C_PID&0CE6\9&1&0&0000",
            r"USB\VID_045E&PID_028E\5&1A2B3C&0&2",
            r"HID\VID_045E&PID_028E&IG_00\7&FFFF&0&0000",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let pad_nodes = vec![r"hid\vid_045e&pid_028e&ig_00\8&2c1b3f1&0&0000".to_string()];
        let stale = stale_pad_entries(&list, &pad_nodes, &[]);
        assert_eq!(stale, list[..4].to_vec());
        // Already in the pending record: undone with it, not twice.
        let pending = vec![r"usb\vid_045e&pid_028e\dszv1".to_string()];
        let stale = stale_pad_entries(&list, &pad_nodes, &pending);
        assert_eq!(stale, list[1..4].to_vec());
        assert!(stale_pad_entries(&list[4..], &[], &[]).is_empty());
    }

    #[test]
    fn microphone_device_only_beside_pads_without_one() {
        use VirtualKind::*;
        // The requested case: an Xbox 360 pad, and a DualShock 4 the same way.
        assert!(wants_microphone_device(true, true, Some(Xbox360)));
        assert!(wants_microphone_device(true, true, Some(DualShock4)));
        // The virtual DualSense has its own; without a pad nothing is added.
        assert!(!wants_microphone_device(true, true, Some(DualSense)));
        assert!(!wants_microphone_device(true, true, None));
        assert!(!wants_microphone_device(true, true, Some(Off)));
        // Microphone off (the default), or no Bluetooth DualSense to send
        // one: no device, so nothing Windows sees changes.
        for pad in [None, Some(Xbox360), Some(DualShock4), Some(DualSense)] {
            assert!(!wants_microphone_device(false, true, pad));
            assert!(!wants_microphone_device(true, false, pad));
        }
    }

    #[test]
    fn parking_rules() {
        use VirtualKind::*;
        assert!(!park_allowed(false, DualSense, true));
        assert!(!park_allowed(true, Xbox360, true));
        assert!(park_allowed(true, DualSense, false));
        assert!(park_allowed(true, DualShock4, true));
        assert!(park_allowed(true, Off, true));
        assert!(!park_allowed(true, Off, false));
    }
}
