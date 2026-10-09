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

use ds_proto::virtual_pad as vp;
use parking_lot::Mutex;

use super::{audio_default, hidhide, usbip, Target};
use crate::device::Device;
use crate::engine::Engine;
use crate::profile::VirtualKind;

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
}

impl Pad {
    pub fn kind(&self) -> VirtualKind {
        self.target.kind()
    }

    pub fn is_hidden(&self) -> bool {
        !self.hidden.lock().is_empty()
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

/// Plug in a pad of `kind`. Game feedback follows `route`.
fn plug(kind: VirtualKind, route: &Arc<Route>) -> Result<Target, String> {
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
    };
    let sink = usbip::DsSink {
        on_output,
        mirror: route.mirror.clone(),
    };
    usbip::UsbPad::create(model, sink).map_err(|e| {
        log::warn!("usbip-win2: {e}");
        format!(
            "usbip-win2 couldn't attach the virtual {} controller ({e}). Games see the real controller instead.",
            kind.label()
        )
    })
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
        let route = Arc::new(Route::default());
        if let Some(d) = device {
            *route.owner.lock() = Arc::downgrade(d);
            *route.mirror.lock() = d.mirror.lock().clone();
        }
        if kind == VirtualKind::DualSense && device.is_none() && usbip::available() {
            // The controller's own service keeps Windows from making the new
            // audio device the default; a pad plugged in for nobody needs
            // its own watch.
            if let Some(mut g) = audio_default::Guard::take() {
                std::thread::Builder::new()
                    .name("audio-default-guard".into())
                    .spawn(move || {
                        while !g.expired() {
                            std::thread::sleep(std::time::Duration::from_millis(250));
                            g.check();
                        }
                    })
                    .ok();
            }
        }
        let target = Arc::new(plug(kind, &route)?);
        let pad = Arc::new(Pad {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            target,
            route,
            mac: Mutex::new(mac.to_string()),
            owner: Mutex::new(device.map(|d| d.id)),
            hidden: Mutex::new(Vec::new()),
        });
        self.pads.lock().push(pad.clone());
        Ok(pad)
    }

    /// The pad a newly connected controller takes over: the one it had
    /// before, else one that belongs to nobody yet.
    pub fn claim(&self, device: &Arc<Device>) -> Option<Arc<Pad>> {
        let mac = device.mac.lock().clone();
        let pads = self.pads.lock();
        let free = |p: &&Arc<Pad>| p.owner.lock().is_none() && p.alive();
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
            log::info!(
                "keeping the virtual {} controller connected while its controller is away",
                pad.kind().label()
            );
        } else {
            self.destroy(engine, pad);
        }
    }

    /// Unplug a pad and undo its hiding.
    pub fn destroy(&self, engine: &Engine, pad: &Arc<Pad>) {
        self.pads.lock().retain(|p| !Arc::ptr_eq(p, pad));
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
        let kind = profile.virtual_out.kind;
        if kind == VirtualKind::Off {
            return;
        }
        match self.create(kind, "", None) {
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
            }
            Err(e) => log::warn!("virtual {} at startup: {e}", kind.label()),
        }
    }

    /// Keep pads nobody drives in line with settings and profiles: unplug
    /// them when persistence is off or their profile no longer wants that
    /// kind (a game rule, an edit), replug when it wants the other kind.
    pub fn maintain(&self, engine: &Engine) {
        let (on, hide_ok, last) = {
            let s = engine.settings.read();
            (
                s.persistent_virtual,
                s.hidhide_control,
                s.last_controller.clone(),
            )
        };
        let idle: Vec<Arc<Pad>> = self
            .list()
            .into_iter()
            .filter(|p| p.owner.lock().is_none())
            .collect();
        for p in idle {
            let mac = p.mac.lock().clone();
            let profile = engine.profile_for_mac(if mac.is_empty() { &last } else { &mac });
            let want = profile.virtual_out.kind;
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
                self.destroy(engine, &p);
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
            }
        }
    }
}
