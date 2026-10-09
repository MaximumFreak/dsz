//! Process-wide state and background services: device hot-plug, profile and
//! settings persistence, game watcher, UDP mod server.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};

use crate::device::Device;
use crate::haptics::audio::AudioHub;
use crate::profile::{Profile, ProfileStore};
use crate::settings::Settings;

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub at: Instant,
    pub warn: bool,
}

#[derive(Default, Clone, Debug)]
pub struct UdpStatus {
    pub listening: bool,
    pub error: Option<String>,
    pub packets: u64,
    pub last_packet: Option<Instant>,
    pub last_text: String,
}

pub struct Engine {
    pub data_dir: PathBuf,
    pub devices: RwLock<Vec<Arc<Device>>>,
    pub profiles: ProfileStore,
    pub settings: RwLock<Settings>,
    settings_rev: AtomicU64,
    settings_dirty: AtomicBool,
    /// Profile forced by a running game, if any: (profile, exe).
    pub game_override: RwLock<Option<(String, String)>>,
    /// A game whose profile was overridden by a manual pick: its rule stays
    /// off until it closes.
    pub game_dismissed: Mutex<Option<String>>,
    pub dsdetect: crate::dsdetect::Detector,
    /// Controllers given a profile since the app started (for
    /// `default_on_start`).
    session_assigned: Mutex<std::collections::HashSet<String>>,
    pub audio: Arc<AudioHub>,
    pub udp: Mutex<UdpStatus>,
    pub toasts: Mutex<Vec<Toast>>,
    pub shutdown: AtomicBool,
    next_id: AtomicU64,
    open_failures: Mutex<HashMap<String, (Instant, u32)>>,
    /// HidHide changes made for virtual controllers.
    pub hider: crate::virt::Hider,
    /// Virtual pads, which outlive controller connections.
    pub pads: crate::virt::Pool,
}

/// Cached profile for a hot loop: refetched only when something changed.
#[derive(Default)]
pub struct ProfileCache {
    key: (u64, u64),
    name: String,
    profile: Option<Arc<Profile>>,
}

impl ProfileCache {
    pub fn get(&mut self, engine: &Engine, device: &Device) -> Arc<Profile> {
        let key = (engine.profiles.revision(), engine.settings_revision());
        if key != self.key || self.profile.is_none() {
            let name = engine.profile_name_for(device);
            self.profile = Some(engine.profiles.get_or_first(&name));
            self.name = name;
            self.key = key;
        }
        self.profile.clone().unwrap()
    }
}

impl Engine {
    pub fn new(data_dir: PathBuf) -> Arc<Engine> {
        let profiles = ProfileStore::load(&data_dir.join("profiles"));
        let mut settings = Settings::load(&data_dir);
        if !profiles.exists(&settings.default_profile) {
            settings.default_profile = profiles.names().first().cloned().unwrap_or_default();
        }
        let hider = crate::virt::Hider::new(&data_dir);
        Arc::new(Engine {
            hider,
            dsdetect: crate::dsdetect::Detector::new(&data_dir),
            data_dir,
            devices: RwLock::new(Vec::new()),
            profiles,
            settings: RwLock::new(settings),
            settings_rev: AtomicU64::new(1),
            settings_dirty: AtomicBool::new(false),
            game_override: RwLock::new(None),
            game_dismissed: Mutex::new(None),
            session_assigned: Mutex::new(Default::default()),
            audio: AudioHub::new(),
            udp: Mutex::new(UdpStatus::default()),
            toasts: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            open_failures: Mutex::new(HashMap::new()),
            pads: Default::default(),
        })
    }

    pub fn settings_revision(&self) -> u64 {
        self.settings_rev.load(Ordering::Acquire)
    }

    /// Make cached profiles refresh without saving anything.
    pub fn bump_config(&self) {
        self.settings_rev.fetch_add(1, Ordering::AcqRel);
    }

    /// Save `settings` soon without refreshing profiles (bookkeeping only).
    pub fn settings_save_later(&self) {
        self.settings_dirty.store(true, Ordering::Release);
    }

    /// Call after changing `settings`.
    pub fn settings_changed(&self) {
        self.settings_rev.fetch_add(1, Ordering::AcqRel);
        self.settings_dirty.store(true, Ordering::Release);
    }

    pub fn toast(&self, text: impl Into<String>) {
        self.toasts.lock().push(Toast {
            text: text.into(),
            at: Instant::now(),
            warn: false,
        });
    }

    pub fn warn_toast(&self, text: impl Into<String>) {
        self.toasts.lock().push(Toast {
            text: text.into(),
            at: Instant::now(),
            warn: true,
        });
    }

    /// The profile name a device should be using right now.
    pub fn profile_name_for(&self, device: &Device) -> String {
        let mac = device.mac.lock().clone();
        self.profile_name_for_mac(&mac)
    }

    /// The profile the controller with `mac` uses (empty: an unknown one).
    pub fn profile_for_mac(&self, mac: &str) -> Arc<Profile> {
        self.profiles.get_or_first(&self.profile_name_for_mac(mac))
    }

    pub fn profile_name_for_mac(&self, mac: &str) -> String {
        let s = self.settings.read();
        if s.auto_profiles {
            if let Some((p, _)) = &*self.game_override.read() {
                if self.profiles.exists(p) {
                    // The stored spelling: a rule may differ in case.
                    return self.profiles.get_or_first(p).name.clone();
                }
            }
        }
        if let Some(p) = self.saved_assignment(&s, mac) {
            return p;
        }
        s.default_profile.clone()
    }

    /// The controller's saved profile, unless the app starts everyone on
    /// the default and nothing was picked this session.
    fn saved_assignment(&self, s: &crate::settings::Settings, mac: &str) -> Option<String> {
        if s.default_on_start && !self.session_assigned.lock().contains(mac) {
            return None;
        }
        s.device_profiles
            .get(mac)
            .filter(|p| self.profiles.exists(p))
            .map(|p| self.profiles.get_or_first(p).name.clone())
    }

    pub fn assigned_profile(&self, device: &Device) -> String {
        let s = self.settings.read();
        let mac = device.mac.lock().clone();
        self.saved_assignment(&s, &mac)
            .unwrap_or_else(|| s.default_profile.clone())
    }

    pub fn set_device_profile(&self, device: &Device, name: &str) {
        let mac = device.mac.lock().clone();
        {
            let mut s = self.settings.write();
            if mac.is_empty() {
                s.default_profile = name.to_string();
            } else {
                self.session_assigned.lock().insert(mac.clone());
                s.device_profiles.insert(mac, name.to_string());
            }
        }
        // A manual pick beats a running game's profile until that game closes.
        if let Some((_, exe)) = self.game_override.write().take() {
            log::info!("profile \"{name}\" picked while {exe} runs: game profile off until it closes");
            *self.game_dismissed.lock() = Some(exe);
            self.bump_config();
        }
        // A manual choice clears mod overrides that belonged to the old profile.
        *device.overrides.lock() = Default::default();
        self.settings_changed();
        device.kick();
    }

    pub fn cycle_profile(&self, device: &Device, delta: i32) {
        let names = self.profiles.names();
        if names.is_empty() {
            return;
        }
        let cur = self.profile_name_for(device);
        let i = names.iter().position(|n| *n == cur).unwrap_or(0) as i32;
        let n = names.len() as i32;
        let next = names[((i + delta).rem_euclid(n)) as usize].clone();
        self.set_device_profile(device, &next);
        self.toast(format!("Profile: {next}"));
        device.identify_light();
    }

    pub fn rename_profile_refs(&self, old: &str, new: &str) {
        let mut s = self.settings.write();
        for v in s.device_profiles.values_mut() {
            if v == old {
                *v = new.to_string();
            }
        }
        if s.default_profile == old {
            s.default_profile = new.to_string();
        }
        for g in s.games.iter_mut() {
            if g.profile == old {
                g.profile = new.to_string();
            }
        }
        drop(s);
        self.settings_changed();
    }

    pub fn add_device(&self, d: Arc<Device>) {
        let mac = d.mac.lock().clone();
        if !mac.is_empty() && self.settings.read().last_controller != mac {
            self.settings.write().last_controller = mac;
            self.settings_dirty.store(true, Ordering::Release);
        }
        self.devices.write().push(d.clone());
        self.toast(format!(
            "{} connected ({})",
            d.model.name(),
            if d.is_bluetooth() { "Bluetooth" } else { "USB" }
        ));
    }

    pub fn remove_device(&self, id: u64) {
        let mut v = self.devices.write();
        if let Some(i) = v.iter().position(|d| d.id == id) {
            let d = v.remove(i);
            drop(v);
            self.toast(format!("{} disconnected", d.model.name()));
        }
    }

    pub fn device_list(&self) -> Vec<Arc<Device>> {
        self.devices.read().clone()
    }

    pub fn note_open_failure(&self, path: &str, e: &std::io::Error) {
        let mut m = self.open_failures.lock();
        let entry = m.entry(path.to_string()).or_insert((Instant::now(), 0));
        entry.0 = Instant::now();
        entry.1 += 1;
        if entry.1 == 1 {
            self.warn_toast(format!(
                "Couldn't open a controller ({e}). Another controller app or Steam Input may be holding it."
            ));
        }
    }

    fn open_backoff(&self, path: &str) -> bool {
        let m = self.open_failures.lock();
        if let Some((at, n)) = m.get(path) {
            let wait = Duration::from_millis((500u64 << (*n).min(5)).min(15_000));
            return at.elapsed() < wait;
        }
        false
    }

    /// Start background services. Returns immediately.
    pub fn start(self: &Arc<Self>) {
        let e = self.clone();
        std::thread::Builder::new()
            .name("hotplug".into())
            .spawn(move || e.hotplug_loop())
            .unwrap();
        let e = self.clone();
        std::thread::Builder::new()
            .name("saver".into())
            .spawn(move || e.saver_loop())
            .unwrap();
        let e = self.clone();
        std::thread::Builder::new()
            .name("games".into())
            .spawn(move || crate::games::run(e))
            .unwrap();
        let e = self.clone();
        std::thread::Builder::new()
            .name("udp".into())
            .spawn(move || crate::udp::run(e))
            .unwrap();
    }

    fn hotplug_loop(self: Arc<Self>) {
        let mut running: HashMap<String, std::thread::JoinHandle<()>> = HashMap::new();
        // Before any controller: the virtual pad takes the first free slot.
        self.pads.startup(&self);
        while !self.shutdown.load(Ordering::Acquire) {
            let present = crate::hid::enumerate();
            running.retain(|_, h| !h.is_finished());
            for info in present {
                if running.contains_key(&info.path) || self.open_backoff(&info.path) {
                    continue;
                }
                let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                let e = self.clone();
                let path = info.path.clone();
                let h = std::thread::Builder::new()
                    .name(format!("device-{id}"))
                    .spawn(move || crate::device::run(e, info, id))
                    .unwrap();
                running.insert(path, h);
            }
            // Clear a stale failure once a device opens fine.
            {
                let live: Vec<String> = self
                    .devices
                    .read()
                    .iter()
                    .map(|d| d.info.path.clone())
                    .collect();
                self.open_failures.lock().retain(|p, _| !live.contains(p));
            }
            self.pads.maintain(&self);
            std::thread::sleep(Duration::from_millis(700));
        }
        for d in self.device_list() {
            d.stop.store(true, Ordering::Release);
        }
        for (_, h) in running {
            let _ = h.join();
        }
    }

    fn saver_loop(self: Arc<Self>) {
        while !self.shutdown.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(400));
            self.flush();
        }
        self.flush();
    }

    pub fn flush(&self) {
        self.profiles.flush();
        if self.settings_dirty.swap(false, Ordering::AcqRel) {
            self.settings.read().save(&self.data_dir);
        }
    }

    /// Stop everything and leave controllers calm. Waits briefly.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        for d in self.device_list() {
            d.stop.store(true, Ordering::Release);
        }
        self.flush();
        let t0 = Instant::now();
        while !self.devices.read().is_empty() && t0.elapsed() < Duration::from_millis(800) {
            std::thread::sleep(Duration::from_millis(20));
        }
        self.pads.clear(self);
        self.hider.release_all();
    }
}

impl Device {
    /// Short white flash on profile change.
    pub fn identify_light(&self) {
        self.feedback.lock().identify_until = Some(Instant::now() + Duration::from_millis(300));
        self.kick();
    }
}
