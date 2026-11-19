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
    // Started again as administrator: the copy without the rights is quitting.
    let elevated = args.iter().any(|a| a == "--elevated");
    let show = args.iter().any(|a| a == "--show");

    // Debug snapshots run beside the real app without touching controllers.
    let snapshot = cfg!(debug_assertions) && std::env::var_os("DSZ_SNAPSHOT").is_some();
    let wait = if updated || moved || elevated { std::time::Duration::from_secs(20) } else { Default::default() };
    if !snapshot && !platform::single_instance(wait) {
        platform::activate_existing(APP_TITLE);
        return;
    }

    let data = platform::data_dir();
    // Run as administrator: hand over to a copy with the rights before
    // touching the log or the controllers. If the prompt is declined, run
    // without them.
    let mut admin_declined = false;
    if !snapshot && !elevated && !platform::is_elevated() && settings::Settings::load(&data).run_as_admin {
        let pass: Vec<&str> = ["--minimized", "--updated", "--moved"]
            .into_iter()
            .filter(|f| args.iter().any(|a| a == f))
            .chain(std::iter::once("--elevated"))
            .collect();
        if platform::start_elevated(&pass.join(" ")) {
            return;
        }
        admin_declined = true;
    }
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
        engine.toast(crate::localized_format!("Updated to DSZ {}", "Atualizado para DSZ {}", "Actualizado a DSZ {}", "已更新到 DSZ {}", env!("CARGO_PKG_VERSION")));
    }
    if admin_declined {
        log::warn!("not running as administrator: the prompt was declined");
        engine.warn_toast(crate::i18n::tr("Running without administrator rights"));
    }
    if moved {
        engine.toast(crate::localized_format!("Settings are in {}", "As configurações estão em {}", "La configuración está en {}", "设置位于 {}", engine.data_dir.display()));
    }

    let start_hidden = !updated && !moved && !show && (minimized_flag || engine.settings.read().start_minimized);
    if let Err(e) = ui::run(engine.clone(), start_hidden) {
        log::error!("UI failed: {e}");
    }
    engine.shutdown();
    log::info!("bye");
}
