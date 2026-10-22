use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, CornerRadius, RichText, Sense, Stroke, StrokeKind};

use crate::device::Device;
use crate::profile::*;
use crate::settings::{FallbackPad, GameRule};
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
        LightMode::Audio => {
            let colors = [
                p.lighting.audio_quiet,
                p.lighting.audio_medium,
                p.lighting.audio_loud,
            ];
            for (i, col) in colors.iter().enumerate() {
                let x = (i as f32 - 1.0) * 8.0;
                pt.circle_filled(c + egui::vec2(x, 0.0), 5.0, rgb(*col));
            }
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
        VirtualKind::Off => (crate::i18n::tr("Native: games see this controller"), MUTED),
        VirtualKind::Xbox360 => (crate::i18n::tr("Virtual Xbox 360 controller"), GOOD),
        VirtualKind::DualSense => (crate::i18n::tr("Virtual DualSense"), ACCENT),
        VirtualKind::DualShock4 => (crate::i18n::tr("Virtual DualShock 4"), VIOLET),
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
        page_header(ui, crate::i18n::tr("Profiles & games"), crate::i18n::tr("Each profile holds lighting, triggers, haptics, motion, and mappings. Games can switch profiles automatically."));
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

        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_profile)
                    .hint_text(crate::i18n::tr("New profile name"))
                    .desired_width(220.0),
            );
            if primary_button(ui, crate::i18n::tr("+ Create")).clicked() {
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
            if button(ui, crate::i18n::tr("Import profile…")).clicked() {
                if let Some(f) = rfd::FileDialog::new()
                    .add_filter(crate::i18n::tr("Profile"), &["json"])
                    .pick_file()
                {
                    match engine.profiles.import(&f) {
                        Ok(n) => engine.toast(crate::localized_format!("Imported \"{n}\"", "\"{n}\" importado", "\"{n}\" importado")),
                        Err(e) => engine.warn_toast(crate::i18n::tr(&e)),
                    }
                }
            }
            if button(ui, crate::i18n::tr("Open profiles folder")).clicked() {
                crate::platform::open_in_explorer(engine.profiles.dir());
            }
            if button(ui, crate::i18n::tr("Backup & restore…"))
                .on_hover_text(crate::i18n::tr("Back up or restore every profile and setting at once, on the Settings page"))
                .clicked()
            {
                self.page = crate::ui::Page::Settings;
                self.focus_backup = Some((std::time::Instant::now(), false));
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
                                                    if button(ui, crate::i18n::tr("Save")).clicked() || commit {
                                                        save = Some((
                                                            old.clone(),
                                                            buf.trim().to_string(),
                                                        ));
                                                    }
                                                    if button(ui, crate::i18n::tr("Cancel")).clicked() {
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
                                                Err(e) => engine.warn_toast(crate::i18n::tr(&e)),
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
                                                pill(ui, crate::i18n::tr("Active"), ACCENT);
                                            }
                                            if prof.name == default {
                                                pill(ui, crate::i18n::tr("Default"), MUTED);
                                            }
                                            if ds_auto.as_deref() == Some(prof.name.as_str()) {
                                                pill(ui, crate::i18n::tr("DualSense games"), ACCENT);
                                            }
                                            let n = games
                                                .iter()
                                                .filter(|g| g.profile == prof.name)
                                                .count();
                                            if n > 0 {
                                                pill(
                                                    ui,
                                                    &crate::localized_format!(
                                                        "{n} game{}", "{n} jogo{}", "{n} juego{}",
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
                                        crate::i18n::tr("Audio haptics"),
                                    ),
                                    (prof.gyro.mode == GyroMode::Mouse, crate::i18n::tr("Gyro")),
                                    (prof.touchpad.mode == TouchMode::Mouse, crate::i18n::tr("Touch mouse")),
                                    (
                                        !matches!(prof.triggers.left, ds_proto::TriggerEffect::Off)
                                            || !matches!(
                                                prof.triggers.right,
                                                ds_proto::TriggerEffect::Off
                                            ),
                                        crate::i18n::tr("Triggers"),
                                    ),
                                    (!prof.mappings.is_empty(), crate::i18n::tr("Mappings")),
                                ];
                                let list: Vec<&str> =
                                    feats.iter().filter(|f| f.0).map(|f| f.1).collect();
                                muted(
                                    ui,
                                    &if list.is_empty() {
                                        crate::i18n::tr("Lighting only").to_string()
                                    } else {
                                        list.join(" · ")
                                    },
                                );
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    if !is_active && primary_button(ui, crate::i18n::tr("Use")).clicked() {
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
                                    if button(ui, crate::i18n::tr("Edit")).clicked() {
                                        self.edit_pin = Some(prof.name.clone());
                                        self.page = crate::ui::Page::Lighting;
                                    }
                                    ui.menu_button(crate::i18n::tr("More ▾"), |ui| {
                                        if ui.button(crate::i18n::tr("Duplicate")).clicked() {
                                            engine.profiles.duplicate(&prof.name);
                                            ui.close();
                                        }
                                        if ui.button(crate::i18n::tr("Rename")).clicked() {
                                            self.rename =
                                                Some((prof.name.clone(), prof.name.clone()));
                                            ui.close();
                                        }
                                        if ui.button(crate::i18n::tr("Make default")).clicked() {
                                            engine.settings.write().default_profile =
                                                prof.name.clone();
                                            engine.settings_changed();
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button(crate::i18n::tr("Export profile…")).clicked() {
                                            if let Some(f) = rfd::FileDialog::new()
                                                .set_file_name(format!(
                                                    "{}.json",
                                                    sanitize_file_name(&prof.name)
                                                ))
                                                .add_filter(crate::i18n::tr("Profile"), &["json"])
                                                .save_file()
                                            {
                                                match engine.profiles.export(&prof.name, &f) {
                                                    Ok(()) => engine.toast(crate::i18n::tr("Profile exported")),
                                                    Err(e) => engine.warn_toast(crate::i18n::tr(&e)),
                                                }
                                            }
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button(RichText::new(crate::i18n::tr("Delete")).color(BAD)).clicked() {
                                            self.confirm_delete = Some(prof.name.clone());
                                            ui.close();
                                        }
                                    });
                                });
                                if self.confirm_delete.as_deref() == Some(prof.name.as_str()) {
                                    ui.add_space(4.0);
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(crate::i18n::tr("Delete this profile?")).color(BAD));
                                        if danger_button(ui, crate::i18n::tr("Delete")).clicked() {
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
                                                    crate::i18n::tr("The last profile can't be deleted"),
                                                );
                                            }
                                            self.confirm_delete = None;
                                        }
                                        if button(ui, crate::i18n::tr("Keep")).clicked() {
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
            crate::i18n::tr("Game profiles"),
            crate::i18n::tr("Switch to a profile while a game runs, and back when it closes"),
            |ui| {
                let mut s = engine.settings.write();
                let mut changed = false;
                changed |= toggle_row(ui, crate::i18n::tr("Switch automatically"), "", &mut s.auto_profiles);
                if let Some((p, exe)) = &*engine.game_override.read() {
                    note(ui, &crate::localized_format!("{exe} is running → \"{p}\"", "{exe} em execução → \"{p}\"", "{exe} en ejecución → \"{p}\""), VIOLET);
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let target = active.clone();
                    egui::ComboBox::from_id_salt("add-running")
                        .width(240.0)
                        .selected_text(crate::i18n::tr("+ Add a running app"))
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
                    if button(ui, crate::i18n::tr("Browse for .exe…")).clicked() {
                        if let Some(f) = rfd::FileDialog::new()
                            .add_filter(crate::i18n::tr("Program"), &["exe"])
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
                    muted(ui, crate::i18n::tr("No games yet. Add one while it runs, or browse for its .exe."));
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
                            pill(ui, crate::i18n::tr("missing"), WARN);
                        }
                        let live = running.iter().any(|r| r.eq_ignore_ascii_case(g.exe.trim()));
                        if live {
                            pill(ui, crate::i18n::tr("running"), GOOD);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("✕").color(MUTED)).frame(false),
                                )
                                .on_hover_text(crate::i18n::tr("Remove"))
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
            crate::i18n::tr("DualSense games"),
            crate::i18n::tr("Games with native DualSense support (found by checking the game's files) get adaptive triggers and haptics through a DualSense profile, without a rule"),
            |ui| {
                let mut s = engine.settings.write();
                let mut changed = toggle_row(
                    ui,
                    crate::i18n::tr("Switch DualSense games automatically"),
                    crate::i18n::tr("A rule under Game profiles wins over this"),
                    &mut s.auto_dualsense,
                );
                if s.auto_dualsense {
                    row(ui, crate::i18n::tr("Profile"), crate::i18n::tr("Used while a DualSense game runs"), |ui| {
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
                            pill(ui, crate::i18n::tr("missing"), WARN);
                        }
                    });
                    if let Some(p) = engine.profiles.find(&s.dualsense_profile) {
                        if p.virtual_out.kind != VirtualKind::DualSense {
                            note(
                                ui,
                                crate::i18n::tr("This profile does not use a virtual DualSense, so over Bluetooth the game cannot send its haptics to the controller."),
                                WARN,
                            );
                        }
                    }
                    let found = engine.dsdetect.detected();
                    ui.add_space(4.0);
                    if found.is_empty() {
                        muted(ui, crate::i18n::tr("No DualSense games seen yet. They show up here the first time they run."));
                    } else {
                        ui.label(RichText::new(crate::i18n::tr("Seen so far")).color(MUTED));
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
                ui.add_space(8.0);
                row(
                    ui,
                    crate::i18n::tr("Other games"),
                    crate::i18n::tr("No native DualSense support, and no rule above"),
                    |ui| {
                        let before = s.fallback_pad;
                        egui::ComboBox::from_id_salt("fallback-pad")
                            .width(200.0)
                            .selected_text(crate::i18n::tr(s.fallback_pad.label()))
                            .show_ui(ui, |ui| {
                                for pad in [FallbackPad::Off, FallbackPad::DualShock4, FallbackPad::Xbox360] {
                                    ui.selectable_value(&mut s.fallback_pad, pad, crate::i18n::tr(pad.label()));
                                }
                            });
                        changed |= before != s.fallback_pad;
                    },
                );
                muted(
                    ui,
                    crate::i18n::tr("Steam, GOG, and Xbox app installs, while Switch automatically is on. DualShock 4 is the one games such as Dead Cells use for PlayStation prompts. A rule above wins."),
                );
                drop(s);
                if changed {
                    engine.settings_changed();
                }
            },
        );
    }
}
