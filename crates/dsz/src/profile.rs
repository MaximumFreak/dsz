//! Controller profiles: the data model and the on-disk store.
//!
//! Every struct is `#[serde(default)]`, so a file that sets only some
//! fields (the shipped defaults, or one edited by hand) loads with the rest
//! filled in; unknown fields are ignored.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ds_proto::input::Button;
use ds_proto::TriggerEffect;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- lighting

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LightMode {
    Static,
    Rainbow,
    Breathing,
    Strobe,
    Battery,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerLeds {
    Player(u8),
    Custom(u8),
    Battery,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LedBrightness {
    High,
    Medium,
    Low,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MuteLed {
    Off,
    On,
    Pulse,
    /// Lit while the mute button toggle is engaged (press Mute to flip).
    Toggle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lighting {
    pub mode: LightMode,
    pub color: [u8; 3],
    /// Second color: breathing fades between `color` and this; strobe alternates.
    pub color2: [u8; 3],
    /// 0..=100
    pub brightness: u8,
    /// 0..=1, animation speed.
    pub speed: f32,
    pub player_leds: PlayerLeds,
    pub led_brightness: LedBrightness,
    pub mute_led: MuteLed,
    pub low_battery_flash: bool,
    pub low_battery_threshold: u8,
    pub low_battery_color: [u8; 3],
}

impl Default for Lighting {
    fn default() -> Self {
        Lighting {
            mode: LightMode::Static,
            color: [0x2D, 0x7F, 0xF9],
            color2: [0x00, 0x00, 0x00],
            brightness: 80,
            speed: 0.5,
            player_leds: PlayerLeds::Player(1),
            led_brightness: LedBrightness::High,
            mute_led: MuteLed::Off,
            low_battery_flash: true,
            low_battery_threshold: 15,
            low_battery_color: [0xFF, 0x30, 0x20],
        }
    }
}

// ---------------------------------------------------------------- triggers / rumble

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Triggers {
    pub left: TriggerEffect,
    pub right: TriggerEffect,
}

impl Default for Triggers {
    fn default() -> Self {
        Triggers {
            left: TriggerEffect::Off,
            right: TriggerEffect::Off,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rumble {
    /// Motor power reduction, 0..=7 in 12.5 % steps; 0 is full power. The
    /// same encoding as the output report's nibble, dualsensectl's
    /// attenuation, and DS4Windows' haptic power level.
    pub power_reduction: u8,
    /// Firmware "improved rumble emulation".
    pub enhanced: bool,
}

impl Default for Rumble {
    fn default() -> Self {
        Rumble {
            power_reduction: 0,
            enhanced: true,
        }
    }
}

// ---------------------------------------------------------------- haptics

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioSource {
    /// Event haptics only (buttons, triggers, rumble).
    None,
    /// Whatever the PC is playing (WASAPI loopback).
    SystemAudio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Latency {
    Short,
    Medium,
    Long,
}

impl Latency {
    /// FIFO target in packet windows (10.667 ms each).
    pub fn prime_windows(self) -> usize {
        match self {
            Latency::Short => 1,
            Latency::Medium => 2,
            Latency::Long => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ButtonHaptics {
    pub enabled: bool,
    /// 0..=1
    pub intensity: f32,
    pub frequency: f32,
    pub duration_ms: f32,
}

impl Default for ButtonHaptics {
    fn default() -> Self {
        ButtonHaptics {
            enabled: false,
            intensity: 0.5,
            frequency: 170.0,
            duration_ms: 45.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerHaptics {
    pub enabled: bool,
    pub intensity: f32,
    pub frequency: f32,
}

impl Default for TriggerHaptics {
    fn default() -> Self {
        TriggerHaptics {
            enabled: false,
            intensity: 0.4,
            frequency: 80.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Haptics {
    /// Stream PCM haptics over Bluetooth.
    pub enabled: bool,
    pub source: AudioSource,
    /// Output device to capture; empty is the Windows default.
    pub device: String,
    pub gain: f32,
    pub low_pass_hz: f32,
    pub high_pass_hz: f32,
    /// Keep left/right separate; off sends a mono mix to both actuators.
    pub stereo: bool,
    pub latency: Latency,
    pub buttons: ButtonHaptics,
    pub triggers: TriggerHaptics,
    /// How strongly classic rumble requests are rendered as haptics.
    pub rumble_intensity: f32,
    /// Always play rumble on the PCM stream ("rumble to haptics").
    /// Off, rumble goes to the controller's own rumble emulation unless
    /// something else is streaming.
    pub rumble_as_haptics: bool,
}

impl Default for Haptics {
    fn default() -> Self {
        Haptics {
            enabled: true,
            source: AudioSource::None,
            device: String::new(),
            gain: 1.6,
            low_pass_hz: 650.0,
            high_pass_hz: 25.0,
            stereo: true,
            latency: Latency::Medium,
            buttons: ButtonHaptics::default(),
            triggers: TriggerHaptics::default(),
            rumble_intensity: 1.0,
            rumble_as_haptics: false,
        }
    }
}

// ---------------------------------------------------------------- input modes

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TouchMode {
    /// Touchpad left to the game.
    Passthrough,
    /// Touchpad drives the Windows cursor.
    Mouse,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Touchpad {
    pub mode: TouchMode,
    pub sensitivity: f32,
    pub acceleration: f32,
    pub tap_to_click: bool,
    pub click_to_click: bool,
    /// Physical click on the right half is a right click.
    pub right_half_right_click: bool,
    pub two_finger_scroll: bool,
    pub scroll_speed: f32,
    pub natural_scroll: bool,
}

impl Default for Touchpad {
    fn default() -> Self {
        Touchpad {
            mode: TouchMode::Passthrough,
            sensitivity: 1.0,
            acceleration: 0.0,
            tap_to_click: true,
            click_to_click: true,
            right_half_right_click: false,
            two_finger_scroll: true,
            scroll_speed: 1.0,
            natural_scroll: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GyroMode {
    Off,
    Mouse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GyroActivation {
    Always,
    WhileHeld(Button),
    WhileNotHeld(Button),
    Toggle(Button),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GyroAxis {
    /// Turn the controller flat (yaw) to move horizontally.
    Yaw,
    /// Lean the controller (roll) to move horizontally.
    Roll,
    /// Both, for a "player space" feel.
    Combined,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Gyro {
    pub mode: GyroMode,
    pub activation: GyroActivation,
    /// Pixels per degree of rotation.
    pub sensitivity: f32,
    /// Vertical multiplier on top of `sensitivity`.
    pub vertical_ratio: f32,
    pub axis: GyroAxis,
    pub invert_x: bool,
    pub invert_y: bool,
    /// Degrees per second below which small motion is softened.
    pub deadzone: f32,
    /// 0..=1, smoothing for slow motion.
    pub smoothing: f32,
    /// Extra speed for fast flicks, 0 = none.
    pub acceleration: f32,
}

impl Default for Gyro {
    fn default() -> Self {
        Gyro {
            mode: GyroMode::Off,
            activation: GyroActivation::WhileHeld(Button::L2),
            sensitivity: 12.0,
            vertical_ratio: 1.0,
            axis: GyroAxis::Yaw,
            invert_x: false,
            invert_y: false,
            deadzone: 1.2,
            smoothing: 0.35,
            acceleration: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StickMode {
    Passthrough,
    Mouse,
    Scroll,
    Wasd,
    Arrows,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StickSettings {
    pub mode: StickMode,
    /// 0..=1 of full deflection.
    pub deadzone: f32,
    /// Pixels per second at full deflection.
    pub mouse_speed: f32,
    /// Response exponent, 1 = linear.
    pub curve: f32,
    pub scroll_speed: f32,
    /// Direction-key threshold, 0..=1.
    pub key_threshold: f32,
}

impl Default for StickSettings {
    fn default() -> Self {
        StickSettings {
            mode: StickMode::Passthrough,
            deadzone: 0.12,
            mouse_speed: 1400.0,
            curve: 1.8,
            scroll_speed: 1.0,
            key_threshold: 0.5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Sticks {
    pub left: StickSettings,
    pub right: StickSettings,
}

// ---------------------------------------------------------------- mappings

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKey {
    PlayPause,
    Next,
    Previous,
    Stop,
    VolumeUp,
    VolumeDown,
    Mute,
}

impl MediaKey {
    pub const ALL: [MediaKey; 7] = [
        MediaKey::PlayPause,
        MediaKey::Next,
        MediaKey::Previous,
        MediaKey::Stop,
        MediaKey::VolumeUp,
        MediaKey::VolumeDown,
        MediaKey::Mute,
    ];
    pub fn vk(self) -> u16 {
        match self {
            MediaKey::PlayPause => 0xB3,
            MediaKey::Next => 0xB0,
            MediaKey::Previous => 0xB1,
            MediaKey::Stop => 0xB2,
            MediaKey::VolumeUp => 0xAF,
            MediaKey::VolumeDown => 0xAE,
            MediaKey::Mute => 0xAD,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            MediaKey::PlayPause => "Play / pause",
            MediaKey::Next => "Next track",
            MediaKey::Previous => "Previous track",
            MediaKey::Stop => "Stop",
            MediaKey::VolumeUp => "Volume up",
            MediaKey::VolumeDown => "Volume down",
            MediaKey::Mute => "Mute audio",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Default)]
pub enum Action {
    #[default]
    None,
    Key {
        vk: u16,
        #[serde(default)]
        ctrl: bool,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        alt: bool,
        #[serde(default)]
        win: bool,
    },
    Mouse {
        button: MouseButton,
    },
    ScrollUp,
    ScrollDown,
    Media {
        key: MediaKey,
    },
    NextProfile,
    PreviousProfile,
    ToggleGyroMouse,
    ToggleTouchpadMouse,
    Run {
        path: String,
    },
    /// Switch this controller to a named profile.
    Profile {
        name: String,
    },
    /// Press a button on the virtual controller (needs one active).
    Gamepad {
        button: Button,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Gesture {
    /// While the button is down.
    Press,
    /// Held longer than the long-press time.
    LongPress,
    /// Two presses inside the double-tap window.
    DoubleTap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HoldStyle {
    /// Output is held while the gesture is held.
    Hold,
    /// One short click per activation.
    Tap,
    /// Repeats while held.
    Turbo,
    /// First activation holds, second releases.
    Toggle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mapping {
    pub enabled: bool,
    pub button: Button,
    /// Buttons that must be held together with `button` (a combo).
    pub with: Vec<Button>,
    /// Hide the button(s) from the virtual controller, so the game never
    /// sees them. A combo hides its buttons only while the combo is held.
    pub mute: bool,
    pub gesture: Gesture,
    pub mode: HoldStyle,
    pub action: Action,
    pub turbo_ms: u32,
    pub long_ms: u32,
    pub double_ms: u32,
}

impl Mapping {
    /// Every button the mapping listens to.
    pub fn buttons(&self) -> impl Iterator<Item = Button> + '_ {
        std::iter::once(self.button)
            .chain(self.with.iter().copied().filter(move |b| *b != self.button))
    }
    pub fn is_combo(&self) -> bool {
        self.with.iter().any(|b| *b != self.button)
    }
}

impl Default for Mapping {
    fn default() -> Self {
        Mapping {
            enabled: true,
            button: Button::Cross,
            with: Vec::new(),
            mute: false,
            gesture: Gesture::Press,
            mode: HoldStyle::Hold,
            action: Action::None,
            turbo_ms: 80,
            long_ms: 500,
            double_ms: 400,
        }
    }
}

// ---------------------------------------------------------------- virtual controller

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VirtualKind {
    /// Games see the physical controller.
    Off,
    Xbox360,
    /// A USB DualSense with the audio endpoint for game haptics.
    DualSense,
    /// A wired DualShock 4, for games that show PlayStation
    /// prompts only for one.
    DualShock4,
}

impl VirtualKind {
    pub fn label(self) -> &'static str {
        match self {
            VirtualKind::Off => "Off (native)",
            VirtualKind::Xbox360 => "Xbox 360",
            VirtualKind::DualSense => "DualSense",
            VirtualKind::DualShock4 => "DualShock 4",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeadzoneShape {
    None,
    /// Inside a circle of `size` percent the stick reads centered.
    Radial,
    /// Each axis is centered independently inside `size` percent.
    Axial,
    /// Inner and outer zones with a response curve; see `CurveDeadzone`.
    Curve,
}

/// A scaled radial deadzone: the stick reads centered inside the inner zone,
/// full past the outer zone, and is rescaled in between, then shaped by a
/// response curve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CurveDeadzone {
    // The six controls are DS4Windows' stick settings (Ryochan7/DS4Windows):
    // dead zone, max zone, sensitivity curve, anti-dead zone, square stick,
    // and max output. The math below is DSZ's own.
    /// Inner zone, percent of full deflection.
    pub inner: f32,
    /// Outer zone, percent: deflection past it reads as full.
    pub outer: f32,
    /// Response curve, -6..=6: 0 is linear, below 0 gentler near the
    /// center, above 0 quicker.
    pub response: f32,
    /// Output just past the inner zone, percent, for games that add a
    /// deadzone of their own.
    pub lift: f32,
    /// Stretch the circle to the square, so the corners reach full X and Y.
    pub square_corners: bool,
    /// Largest output, percent.
    pub max_output: f32,
}

impl Default for CurveDeadzone {
    fn default() -> Self {
        CurveDeadzone {
            inner: 7.0,
            outer: 96.0,
            response: 0.0,
            lift: 0.0,
            square_corners: true,
            max_output: 100.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StickDeadzone {
    pub shape: DeadzoneShape,
    /// Percent of full deflection, for radial and axial.
    pub size: u8,
    pub curve: CurveDeadzone,
}

impl Default for StickDeadzone {
    fn default() -> Self {
        StickDeadzone {
            shape: DeadzoneShape::Radial,
            size: 8,
            curve: CurveDeadzone::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VirtualOutput {
    pub kind: VirtualKind,
    /// Hide the physical controller from games while the virtual one is up.
    pub hide_physical: bool,
    /// Game rumble drives the controller (as haptics over Bluetooth).
    pub game_rumble: bool,
    /// Adaptive-trigger effects the game sends to the virtual DualSense.
    pub game_triggers: bool,
    /// Lightbar, player, and mute LEDs the game sends.
    pub game_leds: bool,
    /// Game haptics: the virtual DualSense's actuator channels.
    pub game_haptics: bool,
    /// 0..=100
    pub game_haptics_level: u8,
    pub left_deadzone: StickDeadzone,
    pub right_deadzone: StickDeadzone,
    /// Invert left X, left Y, right X, right Y.
    pub invert: [bool; 4],
    /// Trigger range in percent, (start, end); below start reads 0, above end full.
    pub left_trigger_range: [u8; 2],
    pub right_trigger_range: [u8; 2],
    /// Pass touch points to the virtual DualSense even in touchpad-mouse mode.
    pub touch_passthrough: bool,
}

impl Default for VirtualOutput {
    fn default() -> Self {
        VirtualOutput {
            kind: VirtualKind::Off,
            hide_physical: true,
            game_rumble: true,
            game_triggers: true,
            game_leds: false,
            game_haptics: true,
            game_haptics_level: 100,
            left_deadzone: StickDeadzone::default(),
            right_deadzone: StickDeadzone::default(),
            invert: [false; 4],
            left_trigger_range: [0, 100],
            right_trigger_range: [0, 100],
            touch_passthrough: true,
        }
    }
}

// ---------------------------------------------------------------- profile

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub lighting: Lighting,
    pub triggers: Triggers,
    pub rumble: Rumble,
    pub haptics: Haptics,
    pub touchpad: Touchpad,
    pub gyro: Gyro,
    pub sticks: Sticks,
    pub mappings: Vec<Mapping>,
    #[serde(rename = "virtual")]
    pub virtual_out: VirtualOutput,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            name: "Default".into(),
            lighting: Lighting::default(),
            triggers: Triggers::default(),
            rumble: Rumble::default(),
            haptics: Haptics::default(),
            touchpad: Touchpad::default(),
            gyro: Gyro::default(),
            sticks: Sticks::default(),
            mappings: Vec::new(),
            virtual_out: VirtualOutput::default(),
        }
    }
}

impl Profile {
    pub fn named(name: &str) -> Profile {
        Profile {
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// The profiles a fresh install starts with, from `defaults/`: "Xbox"
    /// (a virtual Xbox 360 pad, touchpad as mouse) and "DualSense" (a
    /// virtual DualSense with game haptics, PS opens the Game Bar). A long
    /// press on Mute switches between them.
    pub fn starter_set() -> Vec<Profile> {
        [
            include_str!("../defaults/Xbox.json"),
            include_str!("../defaults/DualSense.json"),
        ]
        .iter()
        .map(|t| serde_json::from_str(t).expect("shipped default profile"))
        .collect()
    }
}

// ---------------------------------------------------------------- store

pub fn sanitize_file_name(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        s = "profile".into();
    }
    s
}

pub struct ProfileStore {
    list: RwLock<Vec<Arc<Profile>>>,
    rev: AtomicU64,
    dir: PathBuf,
    dirty: Mutex<HashSet<String>>,
    removed: Mutex<Vec<String>>,
}

impl ProfileStore {
    pub fn load(dir: &Path) -> ProfileStore {
        let _ = std::fs::create_dir_all(dir);
        let mut list: Vec<Arc<Profile>> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            let mut paths: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
                .collect();
            paths.sort();
            for p in paths {
                match std::fs::read_to_string(&p).map(|t| {
                    serde_json::from_str::<Profile>(t.trim_start_matches(crate::settings::BOM))
                }) {
                    Ok(Ok(mut prof)) => {
                        if prof.name.trim().is_empty() {
                            prof.name = p
                                .file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                        }
                        if !list.iter().any(|q| q.name.eq_ignore_ascii_case(&prof.name)) {
                            list.push(Arc::new(prof));
                        }
                    }
                    Ok(Err(e)) => log::warn!("profile {} did not parse: {e}", p.display()),
                    Err(e) => log::warn!("profile {} unreadable: {e}", p.display()),
                }
            }
        }
        let store = ProfileStore {
            list: RwLock::new(list),
            rev: AtomicU64::new(1),
            dir: dir.to_path_buf(),
            dirty: Mutex::new(HashSet::new()),
            removed: Mutex::new(Vec::new()),
        };
        if store.list.read().is_empty() {
            for p in Profile::starter_set() {
                store.dirty.lock().insert(p.name.clone());
                store.list.write().push(Arc::new(p));
            }
            store.flush();
        }
        store
    }

    pub fn revision(&self) -> u64 {
        self.rev.load(Ordering::Acquire)
    }

    fn bump(&self) {
        self.rev.fetch_add(1, Ordering::AcqRel);
    }

    pub fn names(&self) -> Vec<String> {
        self.list.read().iter().map(|p| p.name.clone()).collect()
    }

    pub fn all(&self) -> Vec<Arc<Profile>> {
        self.list.read().clone()
    }

    pub fn find(&self, name: &str) -> Option<Arc<Profile>> {
        self.list.read().iter().find(|p| p.name == name).cloned()
    }

    /// The named profile (an exact match first, then ignoring case, the
    /// same rule as [`exists`](Self::exists)), or the first one.
    pub fn get_or_first(&self, name: &str) -> Arc<Profile> {
        let l = self.list.read();
        l.iter()
            .find(|p| p.name == name)
            .or_else(|| l.iter().find(|p| p.name.eq_ignore_ascii_case(name)))
            .or_else(|| l.first())
            .cloned()
            .unwrap_or_else(|| Arc::new(Profile::default()))
    }

    pub fn exists(&self, name: &str) -> bool {
        self.list
            .read()
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(name))
    }

    /// Replace a profile by name (the name inside `p` must already exist).
    pub fn update(&self, p: Profile) {
        let mut l = self.list.write();
        if let Some(slot) = l.iter_mut().find(|q| q.name == p.name) {
            if **slot == p {
                return;
            }
            self.dirty.lock().insert(p.name.clone());
            *slot = Arc::new(p);
            drop(l);
            self.bump();
        }
    }

    pub fn unique_name(&self, base: &str) -> String {
        let base = base.trim();
        let base = if base.is_empty() { "Profile" } else { base };
        if !self.exists(base) {
            return base.to_string();
        }
        for n in 1.. {
            let cand = format!("{base} ({n})");
            if !self.exists(&cand) {
                return cand;
            }
        }
        unreachable!()
    }

    pub fn add(&self, mut p: Profile) -> String {
        p.name = self.unique_name(&p.name);
        let name = p.name.clone();
        self.dirty.lock().insert(name.clone());
        self.list.write().push(Arc::new(p));
        self.bump();
        name
    }

    pub fn duplicate(&self, name: &str) -> Option<String> {
        let src = self.find(name)?;
        let mut p = (*src).clone();
        p.name = format!("{} copy", src.name);
        Some(self.add(p))
    }

    pub fn rename(&self, old: &str, new: &str) -> Result<(), String> {
        let new = new.trim();
        if new.is_empty() {
            return Err("Name can't be empty".into());
        }
        if new.len() > 64 {
            return Err("Name is too long".into());
        }
        if !new.eq_ignore_ascii_case(old) && self.exists(new) {
            return Err("A profile with that name already exists".into());
        }
        let mut l = self.list.write();
        let Some(slot) = l.iter_mut().find(|q| q.name == old) else {
            return Err("Profile not found".into());
        };
        let mut p = (**slot).clone();
        p.name = new.to_string();
        *slot = Arc::new(p);
        drop(l);
        self.removed.lock().push(old.to_string());
        self.dirty.lock().insert(new.to_string());
        self.bump();
        Ok(())
    }

    pub fn delete(&self, name: &str) -> bool {
        let mut l = self.list.write();
        if l.len() <= 1 {
            return false;
        }
        let before = l.len();
        l.retain(|p| p.name != name);
        let removed = l.len() != before;
        drop(l);
        if removed {
            self.removed.lock().push(name.to_string());
            self.bump();
        }
        removed
    }

    pub fn import(&self, path: &Path) -> Result<String, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut p: Profile = serde_json::from_str(text.trim_start_matches(crate::settings::BOM))
            .map_err(|e| format!("Not a profile: {e}"))?;
        if p.name.trim().is_empty() {
            p.name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
        }
        Ok(self.add(p))
    }

    pub fn export(&self, name: &str, path: &Path) -> Result<(), String> {
        let p = self.find(name).ok_or("Profile not found")?;
        let text = serde_json::to_string_pretty(&*p).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    fn file_for(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{}.json", sanitize_file_name(name)))
    }

    /// Write dirty profiles and remove deleted files. Writes go through a
    /// temp file and a rename so a crash never leaves half a profile.
    pub fn flush(&self) {
        let removed: Vec<String> = std::mem::take(&mut *self.removed.lock());
        for name in removed {
            if !self.exists(&name) {
                let _ = std::fs::remove_file(self.file_for(&name));
            }
        }
        let dirty: Vec<String> = self.dirty.lock().drain().collect();
        for name in dirty {
            let Some(p) = self.find(&name) else { continue };
            let path = self.file_for(&name);
            let tmp = path.with_extension("json.tmp");
            match serde_json::to_string_pretty(&*p) {
                Ok(text) => {
                    if std::fs::write(&tmp, text).is_ok() {
                        if let Err(e) = std::fs::rename(&tmp, &path) {
                            log::warn!("saving profile {name}: {e}");
                        }
                    }
                }
                Err(e) => log::warn!("serializing profile {name}: {e}"),
            }
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for p in Profile::starter_set() {
            let s = serde_json::to_string(&p).unwrap();
            let q: Profile = serde_json::from_str(&s).unwrap();
            assert_eq!(p, q);
        }
    }

    #[test]
    fn shipped_defaults() {
        let set = Profile::starter_set();
        let names: Vec<&str> = set.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Xbox", "DualSense"]);
        assert_eq!(set[0].virtual_out.kind, VirtualKind::Xbox360);
        assert_eq!(set[0].touchpad.mode, TouchMode::Mouse);
        assert_eq!(set[1].virtual_out.kind, VirtualKind::DualSense);
        assert!(set[1].mappings.iter().any(|m| m.button == Button::Ps));
        // The files hold only what differs from the defaults; the rest is
        // filled in from them.
        for p in &set {
            assert!(p
                .mappings
                .iter()
                .any(|m| m.button == Button::Mute && m.gesture == Gesture::LongPress));
            assert_eq!(p.virtual_out.left_deadzone, StickDeadzone::default());
            assert_eq!(p.gyro, Gyro::default());
        }
        let d = crate::settings::Settings::default();
        assert_eq!(d.default_profile, "Xbox");
        assert_eq!(d.dualsense_profile, "DualSense");
    }

    #[test]
    fn partial_file_loads() {
        let p: Profile =
            serde_json::from_str(r#"{"name":"x","lighting":{"brightness":5}}"#).unwrap();
        assert_eq!(p.name, "x");
        assert_eq!(p.lighting.brightness, 5);
        assert_eq!(p.lighting.mode, LightMode::Static);
    }

    #[test]
    fn file_names() {
        assert_eq!(sanitize_file_name("a/b:c. "), "a_b_c");
        assert_eq!(sanitize_file_name("..."), "profile");
    }
}
