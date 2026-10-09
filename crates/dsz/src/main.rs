#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! DSZ - DualSense Zen: a fast native companion for DualSense and DualSense
//! Edge on Windows. Lighting, adaptive triggers, Bluetooth haptics,
//! gyro/touchpad/stick mouse, mappings, per-game profiles, and a UDP mod
//! API for game mods.

mod applog;
mod composer;
mod device;
mod dsdetect;
mod engine;
mod games;
mod haptics;
mod hid;
mod icon_art;
mod install;
mod inputsim;
mod pipeline;
mod platform;
mod profile;
mod settings;
mod tray;
mod udp;
mod ui;
mod virt;

pub const APP_TITLE: &str = "DSZ - DualSense Zen";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--probe-virtual") {
        crate::platform::attach_console();
        virt::probe(args.get(i + 1).map(|s| s.as_str()).unwrap_or(""));
        return;
    }
    let minimized_flag = args.iter().any(|a| a == "--minimized");

    // Debug snapshots run beside the real app without touching controllers.
    let snapshot = cfg!(debug_assertions) && std::env::var_os("DSZ_SNAPSHOT").is_some();
    if !snapshot && !platform::single_instance() {
        platform::activate_existing(APP_TITLE);
        return;
    }

    let data = platform::data_dir();
    applog::init(&data);
    log::info!(
        "{APP_TITLE} {} starting, data in {}",
        env!("CARGO_PKG_VERSION"),
        data.display()
    );
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
    }));

    platform::opt_out_of_power_throttling();
    platform::timer_resolution_1ms();
    let engine = engine::Engine::new(data);
    if !snapshot {
        engine.start();
    }

    let start_hidden = minimized_flag || engine.settings.read().start_minimized;
    if let Err(e) = ui::run(engine.clone(), start_hidden) {
        log::error!("UI failed: {e}");
    }
    engine.shutdown();
    log::info!("bye");
}
