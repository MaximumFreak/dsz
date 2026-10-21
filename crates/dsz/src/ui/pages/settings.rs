use eframe::egui::{self, RichText};

use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

impl App {
    pub(crate) fn page_settings(&mut self, ui: &mut egui::Ui) {
        page_header(ui, crate::i18n::tr("Settings"), "");
        let engine = self.engine.clone();
        card(ui, crate::i18n::tr("Language"), "", |ui| {
            let mut language = engine.settings.read().language;
            let previous = language;
            row(
                ui,
                crate::i18n::tr("Interface language"),
                crate::i18n::tr("Applied immediately and saved automatically"),
                |ui| {
                    egui::ComboBox::from_id_salt("interface-language")
                        .selected_text(language.label())
                        .show_ui(ui, |ui| {
                            for value in crate::i18n::Language::ALL {
                                ui.selectable_value(
                                    &mut language,
                                    value,
                                    value.label(),
                                );
                            }
                        });
                },
            );
            if language != previous {
                engine.settings.write().language = language;
                crate::i18n::set_language(language);
                engine.settings_changed();
                ui.ctx().request_repaint();
            }
        });
        ui.add_space(12.0);
        two_columns(ui, |ui, col| {
            if col == 0 {
                card(ui, crate::i18n::tr("Startup"), "", |ui| {
                    let mut s = engine.settings.write();
                    let mut changed = false;
                    let mut auto = s.start_with_windows;
                    if toggle_row(
                        ui,
                        crate::i18n::tr("Start with Windows"),
                        crate::i18n::tr("Runs quietly in the tray after sign-in"),
                        &mut auto,
                    ) {
                        if crate::platform::set_autostart(auto) {
                            s.start_with_windows = auto;
                            changed = true;
                        } else {
                            engine.warn_toast(crate::i18n::tr("Couldn't change the startup entry"));
                        }
                    }
                    changed |= toggle_row(
                        ui,
                        crate::i18n::tr("Start minimized"),
                        crate::i18n::tr("Open to the tray"),
                        &mut s.start_minimized,
                    );
                    changed |= toggle_row(
                        ui,
                        crate::i18n::tr("Close to tray"),
                        crate::i18n::tr("The X button keeps it running in the background"),
                        &mut s.close_to_tray,
                    );
                    changed |= toggle_row(
                        ui,
                        crate::i18n::tr("Minimize to tray"),
                        crate::i18n::tr("The minimize button hides the window to the tray instead of the taskbar"),
                        &mut s.minimize_to_tray,
                    );
                    let hint = crate::localized_format!("Every controller starts on \"{}\"; picking another profile still sticks until the app restarts", "Todos os controles iniciam em \"{}\"; outro perfil escolhido permanece até reiniciar o aplicativo", s.default_profile);
                    changed |= toggle_row(
                        ui,
                        crate::i18n::tr("Start on the default profile"),
                        &hint,
                        &mut s.default_on_start,
                    );
                    changed |=
                        toggle_row(ui, crate::i18n::tr("Show tips"), crate::i18n::tr("On the Overview page"), &mut s.show_tips);
                    let mut scale = s.ui_scale;
                    let moved = row(ui, crate::i18n::tr("Interface size"), crate::i18n::tr("On top of the Windows display scale"), |ui| {
                        ui.add(
                            egui::Slider::new(&mut scale, 0.8..=1.6)
                                .step_by(0.05)
                                .fixed_decimals(2)
                                .suffix("×"),
                        )
                        .changed()
                    });
                    if moved {
                        s.ui_scale = scale;
                        changed = true;
                    }
                    // Applied once the slider is let go: zooming under a
                    // dragged slider moves it away from the pointer.
                    let want = s.ui_scale.clamp(0.75, 2.0);
                    if (ui.ctx().zoom_factor() - want).abs() > 0.001
                        && !ui.input(|i| i.pointer.any_down())
                    {
                        ui.ctx().set_zoom_factor(want);
                    }
                    drop(s);
                    if changed {
                        engine.settings_changed();
                    }
                });
                ui.add_space(12.0);
                card(ui, crate::i18n::tr("Controllers"), "", |ui| {
                    let mut s = engine.settings.write();
                    let mut changed = false;
                    row(
                        ui,
                        crate::i18n::tr("Turn off when idle"),
                        crate::i18n::tr("Bluetooth only. 0 = never"),
                        |ui| {
                            changed |= ui
                                .add(
                                    egui::Slider::new(&mut s.idle_off_minutes, 0..=60)
                                        .suffix(" min"),
                                )
                                .changed();
                        },
                    );
                    changed |= toggle_row(
                        ui,
                        crate::i18n::tr("Drop corrupted radio packets"),
                        crate::i18n::tr("Checks each Bluetooth report's CRC so interference never shows up as a ghost input"),
                        &mut s.check_input_crc,
                    );
                    drop(s);
                    ui.separator();
                    ui.label(RichText::new(crate::i18n::tr("Names")).font(heading(14.0)));
                    let devices = engine.device_list();
                    let mut known: Vec<String> = engine
                        .settings
                        .read()
                        .device_profiles
                        .keys()
                        .cloned()
                        .collect();
                    for d in &devices {
                        let m = d.mac.lock().clone();
                        if !m.is_empty() && !known.contains(&m) {
                            known.push(m);
                        }
                    }
                    if known.is_empty() {
                        muted(ui, crate::i18n::tr("Connect a controller to name it."));
                    }
                    for mac in known {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&mac).monospace().size(12.0).color(MUTED));
                            let cur = engine
                                .settings
                                .read()
                                .device_names
                                .get(&mac)
                                .cloned()
                                .unwrap_or_default();
                            let editing = self
                                .device_name_edit
                                .as_ref()
                                .map(|(m, _)| *m == mac)
                                .unwrap_or(false);
                            if editing {
                                let buf = &mut self.device_name_edit.as_mut().unwrap().1;
                                let r =
                                    ui.add(egui::TextEdit::singleline(buf).desired_width(180.0));
                                if button(ui, crate::i18n::tr("Save")).clicked()
                                    || (r.lost_focus()
                                        && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                                {
                                    let name = buf.trim().to_string();
                                    let mut s = engine.settings.write();
                                    if name.is_empty() {
                                        s.device_names.remove(&mac);
                                    } else {
                                        s.device_names.insert(mac.clone(), name);
                                    }
                                    drop(s);
                                    engine.settings_changed();
                                    self.device_name_edit = None;
                                }
                            } else {
                                ui.label(if cur.is_empty() {
                                    RichText::new(crate::i18n::tr("(no name)")).color(FAINT)
                                } else {
                                    RichText::new(&cur)
                                });
                                if button(ui, crate::i18n::tr("Rename")).clicked() {
                                    self.device_name_edit = Some((mac.clone(), cur));
                                }
                            }
                        });
                    }
                    if changed {
                        engine.settings_changed();
                    }
                });
            } else {
                card(
                    ui,
                    crate::i18n::tr("Game mod API"),
                    crate::i18n::tr("UDP server for games and mods that drive triggers and lights"),
                    |ui| {
                        let mut s = engine.settings.write();
                        let mut changed = toggle_row(
                            ui,
                            crate::i18n::tr("Enabled"),
                            crate::i18n::tr("Listens on 127.0.0.1 only"),
                            &mut s.udp_enabled,
                        );
                        row(ui, crate::i18n::tr("Port"), crate::i18n::tr("Mods expect 6969"), |ui| {
                            let mut port = s.udp_port as u32;
                            if ui
                                .add(egui::DragValue::new(&mut port).range(1024..=65535))
                                .changed()
                            {
                                s.udp_port = port as u16;
                                changed = true;
                            }
                        });
                        drop(s);
                        if changed {
                            engine.settings_changed();
                        }
                        let st = engine.udp.lock().clone();
                        ui.horizontal(|ui| {
                            if st.listening {
                                pill(ui, crate::i18n::tr("Listening"), GOOD);
                            } else if let Some(e) = &st.error {
                                pill(ui, e, BAD);
                            } else {
                                pill(ui, crate::i18n::tr("Off"), FAINT);
                            }
                            muted(ui, &crate::localized_format!("{} packet(s) received", "{} pacote(s) recebido(s)", st.packets));
                            if let Some(t) = st.last_packet {
                                muted(ui, &crate::localized_format!("· last {:.0}s ago", "· último há {:.0}s", t.elapsed().as_secs_f32()));
                            }
                        });
                        if !st.last_text.is_empty() {
                            ui.label(
                                RichText::new(&st.last_text)
                                    .monospace()
                                    .size(11.0)
                                    .color(FAINT),
                            );
                        }
                    },
                );
                ui.add_space(12.0);
                self.motion_server_card(ui);
                ui.add_space(12.0);
                self.backup_card(ui);
                ui.add_space(12.0);
                card(ui, crate::i18n::tr("Logs"), "", |ui| {
                    ui.horizontal(|ui| {
                        muted(ui, crate::i18n::tr("Show"));
                        egui::ComboBox::from_id_salt("log-level")
                            .width(110.0)
                            .selected_text(format!("{}", self.log_level))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.log_level, log::Level::Warn, crate::i18n::tr("Warn"));
                                ui.selectable_value(&mut self.log_level, log::Level::Info, crate::i18n::tr("Info"));
                                ui.selectable_value(
                                    &mut self.log_level,
                                    log::Level::Debug,
                                    crate::i18n::tr("Debug"),
                                );
                            });
                    });
                    let lw = (ui.available_width() - 22.0).max(100.0);
                    egui::Frame::new()
                        .fill(FIELD)
                        .corner_radius(egui::CornerRadius::same(R_CTRL))
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_width(lw);
                            egui::ScrollArea::vertical()
                                .max_height(260.0)
                                .stick_to_bottom(true)
                                .show(ui, |ui| {
                                    for (lvl, t, text) in crate::applog::recent(300, self.log_level)
                                    {
                                        let c = match lvl {
                                            log::Level::Error => BAD,
                                            log::Level::Warn => WARN,
                                            log::Level::Info => TEXT,
                                            _ => MUTED,
                                        };
                                        ui.label(
                                            RichText::new(format!("{t}  {text}"))
                                                .monospace()
                                                .size(11.5)
                                                .color(c),
                                        );
                                    }
                                });
                        });
                });
                ui.add_space(12.0);
                self.updates_card(ui);
                ui.add_space(12.0);
                card(ui, crate::i18n::tr("About"), "", |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            crate::APP_TITLE,
                            env!("CARGO_PKG_VERSION")
                        ))
                        .font(heading(14.0)),
                    );
                    muted(ui, crate::i18n::tr("A fast native companion for DualSense and DualSense Edge. Virtual controllers need two free, open-source drivers, usbip-win2 and HidHide, which the Virtual controller page can install in one click. There is no license service."));
                    muted(
                        ui,
                        &crate::localized_format!(
                            "Controller speaker: libopus {}, built in. Controller drawings and icon: OpenController (MIT).", "Alto-falante: libopus {} integrado. Desenhos e ícone: OpenController (MIT).",
                            crate::haptics::opus::version()
                        ),
                    );
                });
            }
        });
    }

    /// Emulator motion: the DSU (cemuhook) server.
    fn motion_server_card(&mut self, ui: &mut egui::Ui) {
        use crate::dsu::{Status, PORT};
        let engine = self.engine.clone();
        card(
            ui,
            crate::i18n::tr("Emulator motion"),
            crate::i18n::tr("Gyro and accelerometer for Dolphin, Cemu, Ryujinx and yuzu-family emulators"),
            |ui| {
                let mut on = engine.settings.read().dsu_enabled;
                if toggle_row(
                    ui,
                    crate::i18n::tr("Motion server (DSU)"),
                    &crate::localized_format!("The cemuhook protocol, on 127.0.0.1:{PORT} only", "Protocolo cemuhook, somente em 127.0.0.1:{PORT}"),
                    &mut on,
                ) {
                    engine.settings.write().dsu_enabled = on;
                    engine.settings_changed();
                }
                ui.horizontal(|ui| match engine.dsu.status() {
                    Status::Off => {
                        pill(ui, crate::i18n::tr("Off"), FAINT);
                    }
                    Status::Listening { clients } => {
                        pill(ui, crate::i18n::tr("Listening"), GOOD);
                        muted(
                            ui,
                            &match clients {
                                0 => crate::i18n::tr("No emulator connected").to_string(),
                                1 => "1 emulator connected".to_string(),
                                n => format!("{n} emulators connected"),
                            },
                        );
                    }
                    Status::Failed(e) => {
                        pill(ui, &e, BAD);
                    }
                });
                ui.add_space(4.0);
                muted(ui, &crate::localized_format!("Dolphin: Controllers > Alternate Input Sources > enable DSU Client (127.0.0.1, port {PORT}), then pick the DSUClient device in the Wii Remote's Motion Input tab. Cemu: Input settings > API DSUController. Ryujinx: Input > Motion > CemuHook compatible motion (127.0.0.1, port {PORT}). Yuzu-family emulators: Controls > Motion > CemuhookUDP.", "Dolphin: Controllers > Alternate Input Sources > ative DSU Client (127.0.0.1, porta {PORT}) e escolha DSUClient na aba Motion Input do Wii Remote. Cemu: Input settings > API DSUController. Ryujinx: Input > Motion > CemuHook compatible motion (127.0.0.1, porta {PORT}). Emuladores da família Yuzu: Controls > Motion > CemuhookUDP."));
                muted(ui, crate::i18n::tr("Buttons and sticks still come from the virtual controller. Set the profile's gyro to Off while playing, or the gyro moves the mouse as well."));
            },
        );
    }

    /// Updates: the daily check, a check now, and the update itself.
    fn updates_card(&mut self, ui: &mut egui::Ui) {
        use crate::update::{self, Status};
        let engine = self.engine.clone();
        card(ui, crate::i18n::tr("Updates"), "", |ui| {
            let mut on = engine.settings.read().check_updates;
            if toggle_row(
                ui,
                crate::i18n::tr("Check for updates"),
                crate::i18n::tr("Asks GitHub once a day whether a newer DSZ is out"),
                &mut on,
            ) {
                engine.settings.write().check_updates = on;
                engine.settings_changed();
            }
            ui.add_space(6.0);
            let ctx = ui.ctx().clone();
            let wake = move || ctx.request_repaint();
            match update::status() {
                Some(Status::Available(r)) => {
                    note(
                        ui,
                        &crate::localized_format!(
                            "DSZ {} is available (this is {}). Updating restarts DSZ, so the virtual controllers reconnect: best done outside a game.", "O DSZ {} está disponível (esta é a versão {}). A atualização reinicia o DSZ e reconecta os controles virtuais: prefira atualizar fora de um jogo.",
                            r.version,
                            env!("CARGO_PKG_VERSION")
                        ),
                        ACCENT,
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if primary_button(ui, crate::i18n::tr("Download")).clicked() {
                            update::download(r.clone(), wake.clone());
                        }
                        if button(ui, crate::i18n::tr("What's new")).clicked() {
                            crate::platform::open_url(&r.page);
                        }
                    });
                    ui.add_space(4.0);
                    muted(ui, crate::i18n::tr("Your browser downloads the new dsz.exe; DSZ then checks it against GitHub's checksum and installs it when you say so."));
                }
                Some(Status::Waiting(r)) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        muted(
                            ui,
                            &crate::localized_format!(
                                "Waiting for your browser to download DSZ {} to the Downloads folder…", "Aguardando o navegador baixar o DSZ {} na pasta Downloads…",
                                r.version
                            ),
                        );
                    });
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if button(ui, "I saved it somewhere else…").clicked() {
                            if let Some(f) = rfd::FileDialog::new()
                                .add_filter("DSZ", &["exe"])
                                .pick_file()
                            {
                                if let Err(e) = update::use_file(r.clone(), f) {
                                    engine.warn_toast(e);
                                }
                            }
                        }
                        if button(ui, crate::i18n::tr("Download again")).clicked() {
                            update::open_download(&r);
                        }
                        if button(ui, crate::i18n::tr("Cancel")).clicked() {
                            update::cancel();
                        }
                    });
                }
                Some(Status::Ready(r, file)) => {
                    note(
                        ui,
                        &crate::localized_format!(
                            "DSZ {} is downloaded and checked ({}). Installing restarts DSZ, so the virtual controllers reconnect.", "O DSZ {} foi baixado e verificado ({}). A instalação reinicia o DSZ e reconecta os controles virtuais.",
                            r.version,
                            file.display()
                        ),
                        GOOD,
                    );
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if primary_button(ui, crate::i18n::tr("Install and restart")).clicked() {
                            update::install(r.clone(), file.clone(), wake.clone());
                        }
                        if button(ui, crate::i18n::tr("Not now")).clicked() {
                            update::cancel();
                        }
                    });
                }
                Some(Status::Restarting) => muted(ui, crate::i18n::tr("Restarting into the new version…")),
                status => {
                    let text = match &status {
                        Some(Status::Checking) => crate::i18n::tr("Checking…").to_string(),
                        Some(Status::UpToDate) => {
                            format!("DSZ {} is the latest version.", env!("CARGO_PKG_VERSION"))
                        }
                        Some(Status::Failed(e)) => e.clone(),
                        _ => crate::localized_format!("This is DSZ {}.", "Esta é a versão {} do DSZ.", env!("CARGO_PKG_VERSION")),
                    };
                    if let Some(Status::Failed(_)) = status {
                        note(ui, &text, WARN);
                    } else {
                        muted(ui, &text);
                    }
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        let busy = status == Some(Status::Checking);
                        if ui
                            .add_enabled_ui(!busy, |ui| button(ui, crate::i18n::tr("Check now")))
                            .inner
                            .clicked()
                        {
                            update::check_in_background(wake.clone());
                        }
                        if button(ui, crate::i18n::tr("All releases")).clicked() {
                            crate::platform::open_url(update::RELEASES);
                        }
                    });
                }
            }
        });
    }

    /// Backup & restore: one file with every profile and the app settings,
    /// and the folder they all live in.
    fn backup_card(&mut self, ui: &mut egui::Ui) {
        let engine = self.engine.clone();
        // Lit up for a moment after "Backup & restore…" on Profiles.
        const GLOW_FOR: f32 = 2.0;
        let glow = match self.focus_backup {
            Some((at, _)) if at.elapsed().as_secs_f32() < GLOW_FOR => {
                ui.ctx().request_repaint();
                1.0 - (at.elapsed().as_secs_f32() / GLOW_FOR).powi(3)
            }
            _ => 0.0,
        };
        let (_, resp) = card_glow(ui, crate::i18n::tr("Backup & restore"), "", glow, |ui| {
            muted(
                ui,
                crate::i18n::tr("Save all profiles and app settings to one file, to keep a copy before troubleshooting or to move them to another PC."),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if primary_button(ui, crate::i18n::tr("Back up all settings…")).clicked() {
                    if let Some(f) = rfd::FileDialog::new()
                        .set_file_name(crate::backup::suggested_name())
                        .add_filter(crate::i18n::tr("DSZ backup"), &["json"])
                        .save_file()
                    {
                        match crate::backup::export(&engine, &f) {
                            Ok(()) => engine.toast(crate::i18n::tr("Backup saved")),
                            Err(e) => engine.warn_toast(crate::localized_format!("Couldn't save the backup: {e}", "Não foi possível salvar o backup: {e}")),
                        }
                    }
                }
                if button(ui, crate::i18n::tr("Restore from backup…")).clicked() {
                    if let Some(f) = rfd::FileDialog::new()
                        .add_filter(crate::i18n::tr("DSZ backup"), &["json"])
                        .pick_file()
                    {
                        match crate::backup::read(&f) {
                            Ok(b) => self.restore_pending = Some((f, b)),
                            Err(e) => {
                                self.restore_pending = None;
                                engine.warn_toast(e);
                            }
                        }
                    }
                }
                if button(ui, crate::i18n::tr("Open settings folder")).clicked() {
                    crate::platform::open_in_explorer(&engine.data_dir);
                }
            });
            if let Some((path, b)) = self.restore_pending.clone() {
                ui.add_space(8.0);
                let file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                note(
                    ui,
                    &crate::localized_format!(
                        "Restore \"{file}\" ({})? This replaces all your current profiles and settings. They are saved first to the {} folder, so you can go back.", "Restaurar \"{file}\" ({})? Seus perfis e configurações serão substituídos. Uma cópia será salva antes na pasta {} para permitir a recuperação.",
                        b.summary(),
                        crate::backup::AUTO_DIR
                    ),
                    WARN,
                );
                ui.horizontal(|ui| {
                    if danger_button(ui, crate::i18n::tr("Restore")).clicked() {
                        match crate::backup::restore(&engine, &b) {
                            Ok(saved) => engine.toast(crate::localized_format!(
                                "Settings restored. The previous ones are in {}", "Configurações restauradas. As anteriores estão em {}",
                                saved.display()
                            )),
                            Err(e) => engine.warn_toast(e),
                        }
                        self.edit_pin = None;
                        self.restore_pending = None;
                    } else if button(ui, crate::i18n::tr("Cancel")).clicked() {
                        self.restore_pending = None;
                    }
                });
            }
            ui.add_space(6.0);
            muted(
                ui,
                &crate::localized_format!(
                    "Everything is kept in {} (profiles in its profiles folder).", "Tudo fica em {} (os perfis ficam na subpasta profiles).",
                    engine.data_dir.display()
                ),
            );
            if crate::portable::switchable() {
                ui.add_space(6.0);
                let was = crate::portable::is_portable(&engine.data_dir);
                let mut on = was;
                if toggle_row(
                    ui,
                    crate::i18n::tr("Keep settings beside dsz.exe"),
                    &crate::localized_format!(
                        "Portable: settings, profiles and the log go in a {} folder next to the app. Switching moves them and restarts DSZ", "Portátil: configurações, perfis e registros ficam em uma pasta {} ao lado do aplicativo. A troca move esses arquivos e reinicia o DSZ",
                        crate::portable::FOLDER
                    ),
                    &mut on,
                ) && on != was
                {
                    if let Err(e) = crate::portable::switch(engine.clone(), on) {
                        engine.warn_toast(e);
                    }
                }
            }
        });
        match &mut self.focus_backup {
            Some((_, scrolled @ false)) => {
                resp.scroll_to_me(Some(egui::Align::Center));
                *scrolled = true;
            }
            Some((at, true)) if at.elapsed().as_secs_f32() >= GLOW_FOR => self.focus_backup = None,
            _ => {}
        }
    }
}
