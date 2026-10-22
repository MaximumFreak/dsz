#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! DSZ - DualSense Zen: a fast native companion for DualSense and DualSense
//! Edge on Windows. Lighting, adaptive triggers, Bluetooth haptics,
//! gyro/touchpad/stick mouse, mappings, per-game profiles, and a UDP mod
//! API for game mods.

mod applog;
mod audio_led;
mod backup;
mod composer;
mod device;
mod dsdetect;
mod dsu;
mod engine;
mod flight;
mod games;
mod haptics;
mod hid;
mod i18n;
mod icon_art;
mod install;
mod inputsim;
mod pipeline;
mod platform;
mod portable;
mod profile;
mod settings;
mod tray;
mod udp;
mod ui;
mod update;
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
    // Started by an update: the old version is still quitting.
    let updated = args.iter().any(|a| a == "--updated");
    // Started after the settings moved (portable mode on or off): the same.
    let moved = args.iter().any(|a| a == "--moved");

    // Debug snapshots run beside the real app without touching controllers.
    let snapshot = cfg!(debug_assertions) && std::env::var_os("DSZ_SNAPSHOT").is_some();
    let wait = if updated || moved { std::time::Duration::from_secs(20) } else { Default::default() };
    if !snapshot && !platform::single_instance(wait) {
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

    update::clean_up();
    platform::opt_out_of_power_throttling();
    platform::timer_resolution_1ms();
    let engine = engine::Engine::new(data);
    if !snapshot {
        engine.start();
    }
    if updated {
        engine.toast(crate::localized_format!("Updated to DSZ {}", "Atualizado para DSZ {}", "Actualizado a DSZ {}", env!("CARGO_PKG_VERSION")));
    }
    if moved {
        engine.toast(crate::localized_format!("Settings are in {}", "As configurações estão em {}", "La configuración está en {}", engine.data_dir.display()));
    }

    let start_hidden = !updated && !moved && (minimized_flag || engine.settings.read().start_minimized);
    if let Err(e) = ui::run(engine.clone(), start_hidden) {
        log::error!("UI failed: {e}");
    }
    engine.shutdown();
    log::info!("bye");
}
