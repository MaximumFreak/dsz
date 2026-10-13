use eframe::egui::{self, CornerRadius, RichText, Stroke};

use ds_proto::input::Button;

use super::button_choices;
use crate::inputsim::{key_catalog, key_name, vk_from_egui};
use crate::profile::*;
use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AKind {
    None,
    Key,
    Mouse,
    ScrollUp,
    ScrollDown,
    Media,
    Next,
    Prev,
    Gyro,
    Touch,
    Run,
    Profile,
    Gamepad,
    Xbox,
    TurnOff,
}

fn akind(a: &Action) -> AKind {
    match a {
        Action::None => AKind::None,
        Action::Key { .. } => AKind::Key,
        Action::Mouse { .. } => AKind::Mouse,
        Action::ScrollUp => AKind::ScrollUp,
        Action::ScrollDown => AKind::ScrollDown,
        Action::Media { .. } => AKind::Media,
        Action::NextProfile => AKind::Next,
        Action::PreviousProfile => AKind::Prev,
        Action::ToggleGyroMouse => AKind::Gyro,
        Action::ToggleTouchpadMouse => AKind::Touch,
        Action::Run { .. } => AKind::Run,
        Action::Profile { .. } => AKind::Profile,
        Action::Gamepad { .. } => AKind::Gamepad,
        Action::Xbox { .. } => AKind::Xbox,
        Action::TurnOffController => AKind::TurnOff,
    }
}

const KINDS: [(AKind, &str); 15] = [
    (AKind::None, "Nothing"),
    (AKind::Gamepad, "PlayStation button"),
    (AKind::Xbox, "Xbox button"),
    (AKind::Key, "Keyboard key"),
    (AKind::Mouse, "Mouse button"),
    (AKind::ScrollUp, "Scroll up"),
    (AKind::ScrollDown, "Scroll down"),
    (AKind::Media, "Media key"),
    (AKind::Next, "Next profile"),
    (AKind::Prev, "Previous profile"),
    (AKind::Gyro, "Toggle gyro mouse"),
    (AKind::Touch, "Toggle touchpad mouse"),
    (AKind::Run, "Open a program / file"),
    (AKind::Profile, "Switch to profile"),
    (AKind::TurnOff, "Turn off controller"),
];

fn mouse_label(b: MouseButton) -> &'static str {
    match b {
        MouseButton::Left => "Left",
        MouseButton::Right => "Right",
        MouseButton::Middle => "Middle",
        MouseButton::Back => "Back",
        MouseButton::Forward => "Forward",
    }
}

impl App {
    pub(crate) fn page_mappings(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        p: &mut Profile,
    ) {
        page_header(
            ui,
            "Button mapping",
            "Send keys, mouse buttons, media keys, or app actions from any button or combo, including turning a Bluetooth controller off. With a virtual controller on, a mapping can also hide its button from the game or press a different controller button. Xbox buttons work on any profile, so the Xbox button can open Game Bar (tap) or Task View (hold).",
        );
        let edge = self
            .engine
            .device_list()
            .first()
            .map(|d| d.model.is_edge())
            .unwrap_or(true);
        let virtual_on = p.virtual_out.kind != VirtualKind::Off;
        let xbox_on = p.virtual_out.kind == VirtualKind::Xbox360;
        let can_hide = self.engine.companion.can_hide();
        let profile_names = self.engine.profiles.names();

        // Key capture.
        if let Some(i) = self.capture_key {
            let got = ctx.input(|inp| {
                inp.events.iter().find_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some((k, m)) = got {
                if let (Some(vk), Some(map)) = (vk_from_egui(k), p.mappings.get_mut(i)) {
                    map.action = Action::Key {
                        vk,
                        ctrl: m.ctrl,
                        shift: m.shift,
                        alt: m.alt,
                        win: false,
                    };
                }
                self.capture_key = None;
            }
        }

        let mut remove: Option<usize> = None;
        let mut capture: Option<Option<usize>> = None;
        if p.mappings.is_empty() {
            card(ui, "", "", |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("No mappings yet").font(heading(16.0)));
                    muted(
                        ui,
                        "Add one to turn a button into a key press, a click, or a shortcut.",
                    );
                    ui.add_space(10.0);
                });
            });
            ui.add_space(10.0);
        }
        let row_w = (ui.available_width() - 34.0).max(100.0);
        let p_name = p.name.clone();
        for (i, m) in p.mappings.iter_mut().enumerate() {
            egui::Frame::new()
                .fill(CARD)
                .stroke(Stroke::new(1.0_f32, if m.enabled { BORDER } else { with_alpha(BORDER, 120) }))
                .corner_radius(CornerRadius::same(R_CARD))
                .inner_margin(egui::Margin::symmetric(16, 12))
                .show(ui, |ui| {
                    ui.set_width(row_w);
                    // The remove button sits in the top-right corner, out of
                    // the wrapped row, so a narrow window wraps the row
                    // instead of pushing the card wider.
                    let top = ui.cursor().min;
                    let x = egui::Rect::from_min_size(
                        egui::pos2(top.x + row_w - 20.0, top.y),
                        egui::vec2(20.0, 28.0),
                    );
                    let xr = ui.interact(x, ui.id().with(("map-x", i)), egui::Sense::click()).on_hover_text("Remove");
                    ui.painter().text(
                        x.center(),
                        egui::Align2::CENTER_CENTER,
                        "✕",
                        egui::FontId::proportional(14.0),
                        if xr.hovered() { TEXT } else { MUTED },
                    );
                    if xr.clicked() {
                        remove = Some(i);
                    }
                    ui.scope(|ui| {
                    ui.set_max_width(row_w - 28.0);
                    ui.horizontal_wrapped(|ui| {
                        toggle(ui, &mut m.enabled);
                        fit(ui, 130.0);
                        egui::ComboBox::from_id_salt(("map-btn", i))
                            .width(130.0)
                            .selected_text(RichText::new(m.button.short_name()).strong())
                            .show_ui(ui, |ui| {
                                for b in button_choices(edge) {
                                    ui.selectable_value(&mut m.button, b, b.short_name());
                                }
                            });
                        fit(ui, 120.0);
                        egui::ComboBox::from_id_salt(("map-act", i))
                            .width(120.0)
                            .selected_text(match m.gesture {
                                Gesture::Press => "Press",
                                Gesture::LongPress => "Long press",
                                Gesture::DoubleTap => "Double tap",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut m.gesture, Gesture::Press, "Press");
                                ui.selectable_value(&mut m.gesture, Gesture::LongPress, "Long press");
                                ui.selectable_value(&mut m.gesture, Gesture::DoubleTap, "Double tap");
                            });
                        ui.label(RichText::new("→").color(MUTED).size(16.0));
                        let mut k = akind(&m.action);
                        let before = k;
                        fit(ui, 170.0);
                        egui::ComboBox::from_id_salt(("map-kind", i))
                            .width(170.0)
                            .selected_text(KINDS.iter().find(|x| x.0 == k).map(|x| x.1).unwrap_or(""))
                            .show_ui(ui, |ui| {
                                for (kk, l) in KINDS {
                                    ui.selectable_value(&mut k, kk, l);
                                }
                            });
                        if k != before {
                            m.action = match k {
                                AKind::None => Action::None,
                                AKind::Key => Action::Key { vk: 0x20, ctrl: false, shift: false, alt: false, win: false },
                                AKind::Mouse => Action::Mouse { button: MouseButton::Left },
                                AKind::ScrollUp => Action::ScrollUp,
                                AKind::ScrollDown => Action::ScrollDown,
                                AKind::Media => Action::Media { key: MediaKey::PlayPause },
                                AKind::Next => Action::NextProfile,
                                AKind::Prev => Action::PreviousProfile,
                                AKind::Gyro => Action::ToggleGyroMouse,
                                AKind::Touch => Action::ToggleTouchpadMouse,
                                AKind::Run => Action::Run { path: String::new() },
                                AKind::TurnOff => Action::TurnOffController,
                                AKind::Profile => Action::Profile {
                                    name: profile_names.iter().find(|n| **n != p_name).cloned().unwrap_or_default(),
                                },
                                AKind::Gamepad => Action::Gamepad { button: m.button },
                                AKind::Xbox => Action::Xbox {
                                    button: XboxPadButton::ALL
                                        .into_iter()
                                        .find(|x| x.pad_button() == m.button)
                                        .unwrap_or(XboxPadButton::Guide),
                                },
                            };
                        }
                        match &mut m.action {
                            Action::Key { vk, ctrl, shift, alt, win } => {
                                fit(ui, 110.0);
                                egui::ComboBox::from_id_salt(("map-key", i))
                                    .width(110.0)
                                    .selected_text(key_name(*vk))
                                    .height(320.0)
                                    .show_ui(ui, |ui| {
                                        for c in key_catalog() {
                                            ui.selectable_value(vk, c, key_name(c));
                                        }
                                    });
                                let rec = self.capture_key == Some(i);
                                if ui
                                    .add(egui::Button::new(if rec { "Press a key…" } else { "Record" }).fill(if rec { ACCENT } else { CARD_HI }))
                                    .clicked()
                                {
                                    capture = Some(if rec { None } else { Some(i) });
                                }
                                ui.checkbox(ctrl, "Ctrl");
                                ui.checkbox(shift, "Shift");
                                ui.checkbox(alt, "Alt");
                                ui.checkbox(win, "Win");
                            }
                            Action::Mouse { button } => {
                                fit(ui, 110.0);
                                egui::ComboBox::from_id_salt(("map-mouse", i))
                                    .width(110.0)
                                    .selected_text(mouse_label(*button))
                                    .show_ui(ui, |ui| {
                                        for b in [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward] {
                                            ui.selectable_value(button, b, mouse_label(b));
                                        }
                                    });
                            }
                            Action::Media { key } => {
                                fit(ui, 150.0);
                                egui::ComboBox::from_id_salt(("map-media", i))
                                    .width(150.0)
                                    .selected_text(key.label())
                                    .show_ui(ui, |ui| {
                                        for k in MediaKey::ALL {
                                            ui.selectable_value(key, k, k.label());
                                        }
                                    });
                            }
                            Action::Profile { name } => {
                                fit(ui, 170.0);
                                egui::ComboBox::from_id_salt(("map-prof", i))
                                    .width(170.0)
                                    .selected_text(if name.is_empty() { "Choose…" } else { name.as_str() })
                                    .show_ui(ui, |ui| {
                                        for n in &profile_names {
                                            ui.selectable_value(name, n.clone(), n);
                                        }
                                    });
                            }
                            Action::Gamepad { button } => {
                                fit(ui, 130.0);
                                egui::ComboBox::from_id_salt(("map-pad", i))
                                    .width(130.0)
                                    .selected_text(button.short_name())
                                    .show_ui(ui, |ui| {
                                        for b in button_choices(false) {
                                            ui.selectable_value(button, b, b.short_name());
                                        }
                                    });
                                if !virtual_on {
                                    ui.label(RichText::new("needs a virtual controller").size(12.0).color(WARN));
                                }
                            }
                            Action::Xbox { button } => {
                                fit(ui, 130.0);
                                egui::ComboBox::from_id_salt(("map-xbox", i))
                                    .width(130.0)
                                    .selected_text(button.label())
                                    .show_ui(ui, |ui| {
                                        for b in XboxPadButton::ALL {
                                            ui.selectable_value(button, b, b.label());
                                        }
                                    });
                                if !xbox_on && can_hide {
                                    ui.label(RichText::new("hidden helper Xbox controller").size(12.0).color(MUTED))
                                        .on_hover_text("This profile has no virtual Xbox controller, so DSZ keeps a helper one plugged in, hidden from games with HidHide. Only Windows (Task View, Game Bar) sees it. Needs usbip-win2.");
                                } else if !xbox_on {
                                    ui.label(RichText::new("games see a helper controller").size(12.0).color(WARN))
                                        .on_hover_text("Without HidHide, DSZ plugs in a helper Xbox controller for the press, which games can see, and unplugs it 30 seconds after the last press. Windows needs about a second to see it, so the first press arrives late. Install HidHide on the Virtual controller page to hide it.");
                                }
                            }
                            Action::Run { path } => {
                                ui.add(egui::TextEdit::singleline(path).hint_text("C:\\path\\to\\app.exe").desired_width(220.0));
                                if button(ui, "Browse…").clicked() {
                                    if let Some(f) = rfd::FileDialog::new().pick_file() {
                                        *path = f.display().to_string();
                                    }
                                }
                            }
                            Action::TurnOffController => {
                                ui.label(RichText::new("powers off over Bluetooth").size(12.0).color(MUTED))
                                    .on_hover_text("Drops the Bluetooth link, which turns the controller off. Plugged in over USB, it stays on.");
                            }
                            _ => {}
                        }
                    });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Together with").size(12.5).color(MUTED));
                        let mut chosen: Vec<Button> = m.with.iter().copied().filter(|b| *b != m.button).collect();
                        let label = if chosen.is_empty() {
                            "nothing (single button)".to_string()
                        } else {
                            chosen.iter().map(|b| b.short_name()).collect::<Vec<_>>().join(" + ")
                        };
                        fit(ui, 170.0);
                        egui::ComboBox::from_id_salt(("map-with", i)).width(170.0).selected_text(label).show_ui(ui, |ui| {
                            for b in button_choices(edge) {
                                if b == m.button {
                                    continue;
                                }
                                let mut on = chosen.contains(&b);
                                if ui.checkbox(&mut on, b.short_name()).changed() {
                                    if on {
                                        chosen.push(b);
                                    } else {
                                        chosen.retain(|x| *x != b);
                                    }
                                }
                            }
                        });
                        chosen.sort();
                        m.with = chosen;
                        ui.add_space(10.0);
                        ui.checkbox(&mut m.mute, "Hide from game")
                            .on_hover_text(if m.is_combo() {
                                "While the combo is held, the virtual controller doesn't see these buttons."
                            } else {
                                "The virtual controller never sees this button. Needs a virtual controller."
                            });
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Behavior").size(12.5).color(MUTED));
                        fit(ui, 290.0);
                        segmented(
                            ui,
                            &mut m.mode,
                            &[
                                (HoldStyle::Hold, "Hold"),
                                (HoldStyle::Tap, "Tap"),
                                (HoldStyle::Turbo, "Turbo"),
                                (HoldStyle::Toggle, "Toggle"),
                            ],
                        );
                        if m.mode == HoldStyle::Turbo {
                            fit(ui, 260.0);
                            ui.add(egui::Slider::new(&mut m.turbo_ms, 30..=1000).suffix(" ms").text("interval"));
                        }
                        match m.gesture {
                            Gesture::LongPress => {
                                fit(ui, 260.0);
                                ui.add(egui::Slider::new(&mut m.long_ms, 150..=2000).suffix(" ms").text("hold for"));
                            }
                            Gesture::DoubleTap => {
                                fit(ui, 260.0);
                                ui.add(egui::Slider::new(&mut m.double_ms, 120..=800).suffix(" ms").text("window"));
                            }
                            _ => {}
                        }
                    });
                });
            ui.add_space(8.0);
        }
        if let Some(i) = remove {
            p.mappings.remove(i);
            self.capture_key = None;
        }
        if let Some(c) = capture {
            self.capture_key = c;
        }
        ui.horizontal(|ui| {
            if primary_button(ui, "+ Add mapping").clicked() {
                p.mappings.push(Mapping {
                    action: Action::Key {
                        vk: 0x20,
                        ctrl: false,
                        shift: false,
                        alt: false,
                        win: false,
                    },
                    ..Default::default()
                });
            }
            if !p.mappings.is_empty() && button(ui, "Remove all").clicked() {
                p.mappings.clear();
            }
        });
        ui.add_space(12.0);
        note(
            ui,
            "Hold sends the output while held, Tap sends one click, Turbo repeats, Toggle latches on and off. Long press and double tap fire only when the gesture completes.",
            ACCENT,
        );
    }
}
