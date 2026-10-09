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
use crate::profile::VirtualKind;

pub use pool::{Pad, Pool};
pub use probe::probe;

/// One live virtual controller.
pub type Target = usbip::UsbPad;

impl usbip::UsbPad {
    pub fn kind(&self) -> VirtualKind {
        match self.model() {
            usbip::Model::DualSense => VirtualKind::DualSense,
            usbip::Model::Xbox360 => VirtualKind::Xbox360,
            usbip::Model::DualShock4 => VirtualKind::DualShock4,
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
    pub error: Option<String>,
    pub hidden: bool,
    pub hide_note: Option<String>,
    /// A game has the virtual DualSense's audio endpoint open.
    pub game_audio_open: bool,
}

/// Per-controller handle the reader thread submits to.
#[derive(Default)]
pub struct Slot {
    target: RwLock<Option<Arc<Target>>>,
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
    want: BTreeMap<u64, Vec<String>>,
    applied_for: Vec<String>,
    applied: hidhide::Changes,
}

impl Hider {
    pub fn new(data_dir: &std::path::Path) -> Hider {
        let file = data_dir.join("hidhide-changes.json");
        // Undo whatever a previous run left behind.
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Ok(ch) = serde_json::from_str::<hidhide::Changes>(&text) {
                match hidhide::restore(&ch) {
                    Ok(()) => log::info!(
                        "HidHide: undid leftovers from the last run ({} node(s))",
                        ch.devices_added.len()
                    ),
                    Err(e) => log::warn!("HidHide: could not undo leftovers: {e}"),
                }
            }
            let _ = std::fs::remove_file(&file);
        }
        Hider {
            file,
            state: Mutex::new(HiderState::default()),
        }
    }

    /// Ask for `ids` to be hidden on behalf of controller `dev` (empty to
    /// release). Returns whether they are hidden now.
    pub fn set(&self, dev: u64, ids: Vec<String>) -> Result<bool, String> {
        let mut st = self.state.lock();
        if ids.is_empty() {
            st.want.remove(&dev);
        } else {
            st.want.insert(dev, ids);
        }
        let mut union: Vec<String> = st.want.values().flatten().cloned().collect();
        union.sort();
        union.dedup();
        if union == st.applied_for {
            return Ok(!union.is_empty());
        }
        if !st.applied.is_empty() {
            hidhide::restore(&st.applied).map_err(|e| e.to_string())?;
            st.applied = Default::default();
            st.applied_for.clear();
            let _ = std::fs::remove_file(&self.file);
        }
        if union.is_empty() {
            log::info!("HidHide: physical controller visible again");
            return Ok(false);
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let ch = hidhide::hide(&union, &exe).map_err(|e| e.to_string())?;
        if let Ok(t) = serde_json::to_string_pretty(&ch) {
            let _ = std::fs::write(&self.file, t);
        }
        log::info!("HidHide: hiding {} node(s) from other apps", union.len());
        st.applied = ch;
        st.applied_for = union;
        Ok(true)
    }

    pub fn release_all(&self) {
        let mut st = self.state.lock();
        st.want.clear();
        if !st.applied.is_empty() {
            if let Err(e) = hidhide::restore(&st.applied) {
                log::warn!("HidHide: restore failed: {e}");
            }
            st.applied = Default::default();
            let _ = std::fs::remove_file(&self.file);
        }
        st.applied_for.clear();
    }
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
const NO_HIDHIDE: &str =
    "Install HidHide to hide the physical controller; until then games may see both.";

/// Point `device` at `pad`: input goes to it, its game audio comes back.
fn attach(device: &Device, pad: &Pad) {
    if let Some(a) = pad.target.game_audio() {
        *device.game_audio.lock() = Some(a);
    }
    device.virt.set(Some(pad.target.clone()));
}

fn detach(device: &Device) {
    device.virt.set(None);
    *device.game_audio.lock() = None;
    device.feedback.lock().clear_game();
    device.kick();
}

/// Per-controller service: keeps the virtual pad the profile asks for,
/// hides the physical one, and relays game feedback. Blocks until the
/// controller goes away; the pad itself may stay (see [`pool`]).
pub fn run(engine: &Arc<Engine>, device: &Arc<Device>) {
    let mut cache = ProfileCache::default();
    let mut failed: Option<(VirtualKind, Instant, String)> = None;
    let ids = hide_ids(device);
    let mut audio_guard: Option<audio_default::Guard> = None;
    let mut audio_checked = Instant::now();

    // Take over the pad this controller had before it went away, or the
    // one plugged in at startup.
    let mut pad = engine.pads.claim(device);
    if let Some(p) = &pad {
        attach(device, p);
        log::info!(
            "{} is driving the virtual {} controller",
            device.model.name(),
            p.kind().label()
        );
    }

    while !device.stop.load(Ordering::Acquire) {
        let profile = cache.get(engine, device);
        let vo = &profile.virtual_out;
        let want = vo.kind;
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

        if have != want_some {
            if let Some(p) = pad.take() {
                detach(device);
                engine.pads.destroy(engine, &p);
                engine.toast("Virtual controller disconnected");
            }
            if let Some(want) = want_some {
                let retry = match &failed {
                    Some((k, at, _)) => *k != want || at.elapsed() >= RETRY_AFTER,
                    None => true,
                };
                if retry {
                    if want == VirtualKind::DualSense && usbip::available() {
                        audio_guard = audio_default::Guard::take();
                    }
                    let mac = device.mac.lock().clone();
                    match engine.pads.create(want, &mac, Some(device)) {
                        Ok(p) => {
                            attach(device, &p);
                            engine.toast(format!(
                                "Virtual {} controller connected ({})",
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

        // Windows makes a new USB audio device the default; undo that.
        if let Some(g) = audio_guard.as_mut() {
            if audio_checked.elapsed() >= Duration::from_millis(250) {
                audio_checked = Instant::now();
                g.check();
            }
            if g.expired() || pad.is_none() {
                audio_guard = None;
            }
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
        }

        // Game feedback arrives on the pad's own threads (callbacks).
        std::thread::sleep(Duration::from_millis(50));
    }

    detach(device);
    if let Some(p) = pad {
        let keep =
            engine.settings.read().persistent_virtual && !engine.shutdown.load(Ordering::Acquire);
        engine.pads.release(engine, &p, keep);
    }
}
