use eframe::egui::{self, RichText};

use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

impl App {
    pub(crate) fn page_settings(&mut self, ui: &mut egui::Ui) {
        page_header(ui, "Settings", "");
        let engine = self.engine.clone();
        two_columns(ui, |ui, col| {
            if col == 0 {
                card(ui, "Startup", "", |ui| {
                    let mut s = engine.settings.write();
                    let mut changed = false;
                    let mut auto = s.start_with_windows;
                    if toggle_row(
                        ui,
                        "Start with Windows",
                        "Runs quietly in the tray after sign-in",
                        &mut auto,
                    ) {
                        if crate::platform::set_autostart(auto) {
                            s.start_with_windows = auto;
                            changed = true;
                        } else {
                            engine.warn_toast("Couldn't change the startup entry");
                        }
                    }
                    changed |= toggle_row(
                        ui,
                        "Start minimized",
                        "Open to the tray",
                        &mut s.start_minimized,
                    );
                    changed |= toggle_row(
                        ui,
                        "Close to tray",
                        "The X button keeps it running in the background",
                        &mut s.close_to_tray,
                    );
                    changed |= toggle_row(
                        ui,
                        "Minimize to tray",
                        "The minimize button hides the window to the tray instead of the taskbar",
                        &mut s.minimize_to_tray,
                    );
                    let hint = format!("Every controller starts on \"{}\"; picking another profile still sticks until the app restarts", s.default_profile);
                    changed |= toggle_row(
                        ui,
                        "Start on the default profile",
                        &hint,
                        &mut s.default_on_start,
                    );
                    changed |=
                        toggle_row(ui, "Show tips", "On the Overview page", &mut s.show_tips);
                    let mut scale = s.ui_scale;
                    let moved = row(ui, "Interface size", "Applies on restart", |ui| {
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
                    drop(s);
                    if changed {
                        engine.settings_changed();
                    }
                });
                ui.add_space(12.0);
                card(ui, "Controllers", "", |ui| {
                    let mut s = engine.settings.write();
                    let mut changed = false;
                    row(
                        ui,
                        "Turn off when idle",
                        "Bluetooth only. 0 = never",
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
                        "Drop corrupted radio packets",
                        "Checks each Bluetooth report's CRC so interference never shows up as a ghost input",
                        &mut s.check_input_crc,
                    );
                    drop(s);
                    ui.separator();
                    ui.label(RichText::new("Names").font(heading(14.0)));
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
                        muted(ui, "Connect a controller to name it.");
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
                                if button(ui, "Save").clicked()
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
                                    RichText::new("(no name)").color(FAINT)
                                } else {
                                    RichText::new(&cur)
                                });
                                if button(ui, "Rename").clicked() {
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
                    "Game mod API",
                    "UDP server for games and mods that drive triggers and lights",
                    |ui| {
                        let mut s = engine.settings.write();
                        let mut changed = toggle_row(
                            ui,
                            "Enabled",
                            "Listens on 127.0.0.1 only",
                            &mut s.udp_enabled,
                        );
                        row(ui, "Port", "Mods expect 6969", |ui| {
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
                                pill(ui, "Listening", GOOD);
                            } else if let Some(e) = &st.error {
                                pill(ui, e, BAD);
                            } else {
                                pill(ui, "Off", FAINT);
                            }
                            muted(ui, &format!("{} packet(s) received", st.packets));
                            if let Some(t) = st.last_packet {
                                muted(ui, &format!("· last {:.0}s ago", t.elapsed().as_secs_f32()));
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
                card(ui, "Data & logs", "", |ui| {
                    ui.horizontal(|ui| {
                        if button(ui, "Open data folder").clicked() {
                            crate::platform::open_in_explorer(&engine.data_dir);
                        }
                        egui::ComboBox::from_id_salt("log-level")
                            .width(110.0)
                            .selected_text(format!("{}", self.log_level))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.log_level, log::Level::Warn, "Warn");
                                ui.selectable_value(&mut self.log_level, log::Level::Info, "Info");
                                ui.selectable_value(
                                    &mut self.log_level,
                                    log::Level::Debug,
                                    "Debug",
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
                card(ui, "About", "", |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            crate::APP_TITLE,
                            env!("CARGO_PKG_VERSION")
                        ))
                        .font(heading(14.0)),
                    );
                    muted(ui, "A fast native companion for DualSense and DualSense Edge. Virtual controllers need two free, open-source drivers, usbip-win2 and HidHide, which the Virtual controller page can install in one click. There is no license service.");
                });
            }
        });
    }
}
