use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, CornerRadius, RichText, Sense, Stroke, StrokeKind};

use crate::device::Device;
use crate::profile::*;
use crate::settings::GameRule;
use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

fn profile_swatch(ui: &mut egui::Ui, p: &Profile) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), Sense::hover());
    let pt = ui.painter();
    let c = r.center();
    match p.lighting.mode {
        LightMode::Rainbow => {
            for k in 0..12 {
                let a0 = k as f32 / 12.0 * std::f32::consts::TAU;
                let col = crate::composer::hsv(k as f32 * 30.0, 1.0, 1.0);
                let pos = c + egui::vec2(a0.cos(), a0.sin()) * 11.0;
                pt.circle_filled(pos, 5.0, rgb(col));
            }
        }
        LightMode::Off => {
            pt.circle_stroke(c, 14.0, Stroke::new(1.5_f32, BORDER));
        }
        LightMode::Battery => {
            pt.circle_filled(c, 15.0, rgb(crate::composer::battery_color(70)));
        }
        _ => {
            pt.circle_filled(c, 17.0, with_alpha(rgb(p.lighting.color), 50));
            pt.circle_filled(c, 13.0, rgb(p.lighting.color));
        }
    }
}

/// What games see with this profile: a small pad glyph and the virtual
/// controller's name, in that controller's color.
fn virtual_badge(ui: &mut egui::Ui, kind: VirtualKind) {
    let (name, color) = match kind {
        VirtualKind::Off => ("Native: games see this controller", MUTED),
        VirtualKind::Xbox360 => ("Virtual Xbox 360 controller", GOOD),
        VirtualKind::DualSense => ("Virtual DualSense", ACCENT),
        VirtualKind::DualShock4 => ("Virtual DualShock 4", VIOLET),
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 12.0), Sense::hover());
        let pt = ui.painter();
        let body = Stroke::new(1.4_f32, color);
        pt.rect_stroke(
            egui::Rect::from_center_size(r.center(), egui::vec2(16.0, 9.0)),
            CornerRadius::same(4),
            body,
            StrokeKind::Middle,
        );
        pt.circle_filled(r.center() + egui::vec2(-4.0, 0.0), 1.4, color);
        pt.circle_filled(r.center() + egui::vec2(4.0, 0.0), 1.4, color);
        ui.label(RichText::new(name).size(12.5).color(color));
    });
}

impl App {
    fn running_exes(&mut self) -> Vec<String> {
        let stale = self
            .processes
            .as_ref()
            .map(|(t, _)| t.elapsed() > Duration::from_secs(4))
            .unwrap_or(true);
        if stale {
            let skip = [
                "svchost.exe",
                "system",
                "registry",
                "csrss.exe",
                "wininit.exe",
                "services.exe",
                "lsass.exe",
                "smss.exe",
                "winlogon.exe",
                "dwm.exe",
                "explorer.exe",
                "fontdrvhost.exe",
                "conhost.exe",
                "runtimebroker.exe",
                "searchhost.exe",
                "sihost.exe",
                "taskhostw.exe",
                "ctfmon.exe",
                "dsz.exe",
                "[system process]",
                "memory compression",
                "dllhost.exe",
            ];
            let mut v: Vec<String> = crate::platform::process_names()
                .into_iter()
                .filter(|n| n.ends_with(".exe") && !skip.contains(&n.as_str()))
                .collect();
            v.sort();
            self.processes = Some((Instant::now(), v));
        }
        self.processes
            .as_ref()
            .map(|x| x.1.clone())
            .unwrap_or_default()
    }

    pub(crate) fn page_profiles(
        &mut self,
        ui: &mut egui::Ui,
        _p: &mut Profile,
        dev: Option<&Arc<Device>>,
    ) {
        page_header(ui, "Profiles & games", "Each profile holds lighting, triggers, haptics, motion, and mappings. Games can switch profiles automatically.");
        let engine = self.engine.clone();
        let active = match dev {
            Some(d) => engine.profile_name_for(d),
            None => engine.settings.read().default_profile.clone(),
        };
        let (default, games, ds_auto) = {
            let s = engine.settings.read();
            (
                s.default_profile.clone(),
                s.games.clone(),
                s.auto_dualsense.then(|| s.dualsense_profile.clone()),
            )
        };

        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_profile)
                    .hint_text("New profile name")
                    .desired_width(220.0),
            );
            if primary_button(ui, "+ Create").clicked() {
                let name = if self.new_profile.trim().is_empty() {
                    "New profile"
                } else {
                    self.new_profile.trim()
                };
                let n = engine.profiles.add(Profile::named(name));
                self.new_profile.clear();
                self.edit_pin = Some(n);
                self.page = crate::ui::Page::Lighting;
            }
            if button(ui, "Import file…").clicked() {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter("Profile", &["json"])
                    .pick_file()
                {
                    match engine.profiles.import(&f) {
                        Ok(n) => engine.toast(format!("Imported \"{n}\"")),
                        Err(e) => engine.warn_toast(e),
                    }
                }
            }
            if button(ui, "Open folder").clicked() {
                crate::platform::open_in_explorer(engine.profiles.dir());
            }
        });
        ui.add_space(10.0);

        let all = engine.profiles.all();
        let gap = 12.0;
        let per_row = (((ui.available_width() + gap) / (290.0 + gap)).floor() as usize).max(1);
        let card_w = (ui.available_width() + gap) / per_row as f32 - gap - 30.0;
        for chunk in all.chunks(per_row) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
                for prof in chunk.iter() {
                    let is_active = prof.name == active;
                    egui::Frame::new()
                        .fill(if is_active { CARD_HI } else { CARD })
                        .stroke(Stroke::new(
                            1.0_f32,
                            if is_active { ACCENT } else { BORDER },
                        ))
                        .corner_radius(CornerRadius::same(R_CARD))
                        .inner_margin(egui::Margin::same(14))
                        .show(ui, |ui| {
                            ui.set_width(card_w);
                            ui.vertical(|ui| {
                                ui.set_width(card_w);
                                ui.horizontal(|ui| {
                                    profile_swatch(ui, prof);
                                    ui.vertical(|ui| {
                                        let mut save: Option<(String, String)> = None;
                                        let mut cancel = false;
                                        let mut renaming = false;
                                        if let Some((old, buf)) = &mut self.rename {
                                            if *old == prof.name {
                                                renaming = true;
                                                let r = ui.add(
                                                    egui::TextEdit::singleline(buf)
                                                        .desired_width(150.0),
                                                );
                                                let commit = r.lost_focus()
                                                    && ui
                                                        .input(|i| i.key_pressed(egui::Key::Enter));
                                                ui.horizontal(|ui| {
                                                    if button(ui, "Save").clicked() || commit {
                                                        save = Some((
                                                            old.clone(),
                                                            buf.trim().to_string(),
                                                        ));
                                                    }
                                                    if button(ui, "Cancel").clicked() {
                                                        cancel = true;
                                                    }
                                                });
                                            }
                                        }
                                        if let Some((o, n)) = save {
                                            match engine.profiles.rename(&o, &n) {
                                                Ok(()) => {
                                                    engine.rename_profile_refs(&o, &n);
                                                    if self.edit_pin.as_deref() == Some(o.as_str())
                                                    {
                                                        self.edit_pin = Some(n);
                                                    }
                                                    self.rename = None;
                                                }
                                                Err(e) => engine.warn_toast(e),
                                            }
                                        }
                                        if cancel {
                                            self.rename = None;
                                        }
                                        if renaming {
                                            return;
                                        }
                                        ui.label(RichText::new(&prof.name).font(heading(15.0)));
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 4.0;
                                            if is_active {
                                                pill(ui, "Active", ACCENT);
                                            }
                                            if prof.name == default {
                                                pill(ui, "Default", MUTED);
                                            }
                                            if ds_auto.as_deref() == Some(prof.name.as_str()) {
                                                pill(ui, "DualSense games", ACCENT);
                                            }
                                            let n = games
                                                .iter()
                                                .filter(|g| g.profile == prof.name)
                                                .count();
                                            if n > 0 {
                                                pill(
                                                    ui,
                                                    &format!(
                                                        "{n} game{}",
                                                        if n == 1 { "" } else { "s" }
                                                    ),
                                                    VIOLET,
                                                );
                                            }
                                        });
                                    });
                                });
                                ui.add_space(6.0);
                                virtual_badge(ui, prof.virtual_out.kind);
                                ui.add_space(2.0);
                                let feats = [
                                    (
                                        prof.haptics.enabled
                                            && prof.haptics.source == AudioSource::SystemAudio,
                                        "Audio haptics",
                                    ),
                                    (prof.gyro.mode == GyroMode::Mouse, "Gyro"),
                                    (prof.touchpad.mode == TouchMode::Mouse, "Touch mouse"),
                                    (
                                        !matches!(prof.triggers.left, ds_proto::TriggerEffect::Off)
                                            || !matches!(
                                                prof.triggers.right,
                                                ds_proto::TriggerEffect::Off
                                            ),
                                        "Triggers",
                                    ),
                                    (!prof.mappings.is_empty(), "Mappings"),
                                ];
                                let list: Vec<&str> =
                                    feats.iter().filter(|f| f.0).map(|f| f.1).collect();
                                muted(
                                    ui,
                                    &if list.is_empty() {
                                        "Lighting only".to_string()
                                    } else {
                                        list.join(" · ")
                                    },
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    if !is_active && primary_button(ui, "Use").clicked() {
                                        match dev {
                                            Some(d) => engine.set_device_profile(d, &prof.name),
                                            None => {
                                                engine.settings.write().default_profile =
                                                    prof.name.clone();
                                                engine.settings_changed();
                                            }
                                        }
                                        self.edit_pin = None;
                                    }
                                    if button(ui, "Edit").clicked() {
                                        self.edit_pin = Some(prof.name.clone());
                                        self.page = crate::ui::Page::Lighting;
                                    }
                                    ui.menu_button("More ▾", |ui| {
                                        if ui.button("Duplicate").clicked() {
                                            engine.profiles.duplicate(&prof.name);
                                            ui.close();
                                        }
                                        if ui.button("Rename").clicked() {
                                            self.rename =
                                                Some((prof.name.clone(), prof.name.clone()));
                                            ui.close();
                                        }
                                        if ui.button("Make default").clicked() {
                                            engine.settings.write().default_profile =
                                                prof.name.clone();
                                            engine.settings_changed();
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button("Export file…").clicked() {
                                            if let Some(f) = rfd::FileDialog::new()
                                                .set_file_name(format!(
                                                    "{}.json",
                                                    sanitize_file_name(&prof.name)
                                                ))
                                                .add_filter("Profile", &["json"])
                                                .save_file()
                                            {
                                                match engine.profiles.export(&prof.name, &f) {
                                                    Ok(()) => engine.toast("Profile exported"),
                                                    Err(e) => engine.warn_toast(e),
                                                }
                                            }
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button(RichText::new("Delete").color(BAD)).clicked() {
                                            self.confirm_delete = Some(prof.name.clone());
                                            ui.close();
                                        }
                                    });
                                });
                                if self.confirm_delete.as_deref() == Some(prof.name.as_str()) {
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("Delete this profile?").color(BAD));
                                        if danger_button(ui, "Delete").clicked() {
                                            if engine.profiles.delete(&prof.name) {
                                                let first = engine
                                                    .profiles
                                                    .names()
                                                    .first()
                                                    .cloned()
                                                    .unwrap_or_default();
                                                engine.rename_profile_refs(&prof.name, &first);
                                                if self.edit_pin.as_deref()
                                                    == Some(prof.name.as_str())
                                                {
                                                    self.edit_pin = None;
                                                }
                                            } else {
                                                engine.warn_toast(
                                                    "The last profile can't be deleted",
                                                );
                                            }
                                            self.confirm_delete = None;
                                        }
                                        if button(ui, "Keep").clicked() {
                                            self.confirm_delete = None;
                                        }
                                    });
                                }
                            });
                        });
                }
            });
            ui.add_space(gap);
        }
        ui.add_space(16.0);
        let running = self.running_exes();
        let names = engine.profiles.names();
        card(
            ui,
            "Game profiles",
            "Switch to a profile while a game runs, and back when it closes",
            |ui| {
                let mut s = engine.settings.write();
                let mut changed = false;
                changed |= toggle_row(ui, "Switch automatically", "", &mut s.auto_profiles);
                if let Some((p, exe)) = &*engine.game_override.read() {
                    note(ui, &format!("{exe} is running → \"{p}\""), VIOLET);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let target = active.clone();
                    egui::ComboBox::from_id_salt("add-running")
                        .width(240.0)
                        .selected_text("+ Add a running app")
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            for exe in &running {
                                if ui.selectable_label(false, exe).clicked() {
                                    s.games.push(GameRule {
                                        enabled: true,
                                        exe: exe.clone(),
                                        profile: target.clone(),
                                    });
                                    changed = true;
                                }
                            }
                        });
                    if button(ui, "Browse for .exe…").clicked() {
                        if let Some(f) = rfd::FileDialog::new()
                            .add_filter("Program", &["exe"])
                            .pick_file()
                        {
                            if let Some(n) = f.file_name() {
                                s.games.push(GameRule {
                                    enabled: true,
                                    exe: n.to_string_lossy().into_owned(),
                                    profile: target.clone(),
                                });
                                changed = true;
                            }
                        }
                    }
                });
                ui.add_space(6.0);
                if s.games.is_empty() {
                    muted(ui, "No games yet. Add one while it runs, or browse for its .exe.");
                }
                let mut remove = None;
                for (i, g) in s.games.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        changed |= toggle(ui, &mut g.enabled).changed();
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut g.exe)
                                .hint_text("game.exe")
                                .desired_width(230.0),
                        );
                        changed |= r.changed();
                        ui.label(RichText::new("→").color(MUTED));
                        let before = g.profile.clone();
                        egui::ComboBox::from_id_salt(("game-prof", i))
                            .width(180.0)
                            .selected_text(&g.profile)
                            .show_ui(ui, |ui| {
                                for n in &names {
                                    ui.selectable_value(&mut g.profile, n.clone(), n);
                                }
                            });
                        changed |= before != g.profile;
                        if !names.contains(&g.profile) {
                            pill(ui, "missing", WARN);
                        }
                        let live = running.iter().any(|r| r.eq_ignore_ascii_case(g.exe.trim()));
                        if live {
                            pill(ui, "running", GOOD);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("✕").color(MUTED)).frame(false),
                                )
                                .on_hover_text("Remove")
                                .clicked()
                            {
                                remove = Some(i);
                            }
                        });
                    });
                }
                if let Some(i) = remove {
                    s.games.remove(i);
                    changed = true;
                }
                drop(s);
                if changed {
                    engine.settings_changed();
                }
            },
        );
        ui.add_space(16.0);
        card(
            ui,
            "DualSense games",
            "Games with native DualSense support (found by checking the game's files) get adaptive triggers and haptics through a DualSense profile, without a rule",
            |ui| {
                let mut s = engine.settings.write();
                let mut changed = toggle_row(
                    ui,
                    "Switch DualSense games automatically",
                    "A rule under Game profiles wins over this",
                    &mut s.auto_dualsense,
                );
                if s.auto_dualsense {
                    row(ui, "Profile", "Used while a DualSense game runs", |ui| {
                        let before = s.dualsense_profile.clone();
                        egui::ComboBox::from_id_salt("ds-auto-profile")
                            .width(180.0)
                            .selected_text(&s.dualsense_profile)
                            .show_ui(ui, |ui| {
                                for n in &names {
                                    ui.selectable_value(&mut s.dualsense_profile, n.clone(), n);
                                }
                            });
                        changed |= before != s.dualsense_profile;
                        if !names.contains(&s.dualsense_profile) {
                            pill(ui, "missing", WARN);
                        }
                    });
                    if let Some(p) = engine.profiles.find(&s.dualsense_profile) {
                        if p.virtual_out.kind != VirtualKind::DualSense {
                            note(
                                ui,
                                "This profile does not use a virtual DualSense, so over Bluetooth the game cannot send its haptics to the controller.",
                                WARN,
                            );
                        }
                    }
                    let found = engine.dsdetect.detected();
                    ui.add_space(4.0);
                    if found.is_empty() {
                        muted(ui, "No DualSense games seen yet. They show up here the first time they run.");
                    } else {
                        ui.label(RichText::new("Seen so far").color(MUTED));
                        for (path, why) in found {
                            let name = std::path::Path::new(&path)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or(path.clone());
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&name).strong());
                                muted(ui, &why);
                            });
                        }
                    }
                }
                drop(s);
                if changed {
                    engine.settings_changed();
                }
            },
        );
    }
}
