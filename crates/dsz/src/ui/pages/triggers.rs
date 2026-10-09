use std::sync::Arc;

use ds_proto::{TriggerEffect, TriggerPreset};
use eframe::egui::{self, pos2, CornerRadius, Rect, RichText, Sense, Stroke};

use crate::device::Device;
use crate::profile::Profile;
use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

fn presets() -> Vec<(&'static str, TriggerEffect)> {
    vec![
        ("Off", TriggerEffect::Off),
        (
            "Light",
            TriggerEffect::Feedback {
                position: 1,
                strength: 2,
            },
        ),
        (
            "Heavy",
            TriggerEffect::Feedback {
                position: 0,
                strength: 7,
            },
        ),
        (
            "Pistol",
            TriggerEffect::Weapon {
                start: 4,
                end: 6,
                strength: 7,
            },
        ),
        (
            "Rifle",
            TriggerEffect::Weapon {
                start: 2,
                end: 5,
                strength: 5,
            },
        ),
        (
            "Bow",
            TriggerEffect::Bow {
                start: 1,
                end: 6,
                strength: 6,
                snap: 7,
            },
        ),
        (
            "Machine gun",
            TriggerEffect::Vibration {
                position: 3,
                amplitude: 7,
                frequency: 18,
            },
        ),
        (
            "Engine",
            TriggerEffect::Machine {
                start: 1,
                end: 9,
                amp_a: 2,
                amp_b: 6,
                frequency: 7,
                period: 4,
            },
        ),
        (
            "Galloping",
            TriggerEffect::Galloping {
                start: 0,
                end: 9,
                first_foot: 2,
                second_foot: 5,
                frequency: 5,
            },
        ),
        (
            "Brake",
            TriggerEffect::Slope {
                start: 2,
                end: 9,
                start_strength: 1,
                end_strength: 8,
            },
        ),
        (
            "Rigid",
            TriggerEffect::Preset {
                preset: TriggerPreset::Rigid,
            },
        ),
        (
            "Click",
            TriggerEffect::Preset {
                preset: TriggerPreset::GameCube,
            },
        ),
    ]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Off,
    Feedback,
    Weapon,
    Vibration,
    Slope,
    MultiFeedback,
    MultiVibration,
    Bow,
    Galloping,
    Machine,
    Preset,
    Other,
}

fn kind(e: &TriggerEffect) -> Kind {
    match e {
        TriggerEffect::Off => Kind::Off,
        TriggerEffect::Feedback { .. } => Kind::Feedback,
        TriggerEffect::Weapon { .. } => Kind::Weapon,
        TriggerEffect::Vibration { .. } => Kind::Vibration,
        TriggerEffect::Slope { .. } => Kind::Slope,
        TriggerEffect::MultiFeedback { .. } => Kind::MultiFeedback,
        TriggerEffect::MultiVibration { .. } => Kind::MultiVibration,
        TriggerEffect::Bow { .. } => Kind::Bow,
        TriggerEffect::Galloping { .. } => Kind::Galloping,
        TriggerEffect::Machine { .. } => Kind::Machine,
        TriggerEffect::Preset { .. } => Kind::Preset,
        _ => Kind::Other,
    }
}

fn default_for(k: Kind) -> TriggerEffect {
    match k {
        Kind::Off | Kind::Other => TriggerEffect::Off,
        Kind::Feedback => TriggerEffect::Feedback {
            position: 2,
            strength: 5,
        },
        Kind::Weapon => TriggerEffect::Weapon {
            start: 3,
            end: 6,
            strength: 7,
        },
        Kind::Vibration => TriggerEffect::Vibration {
            position: 0,
            amplitude: 6,
            frequency: 25,
        },
        Kind::Slope => TriggerEffect::Slope {
            start: 1,
            end: 8,
            start_strength: 1,
            end_strength: 8,
        },
        Kind::MultiFeedback => TriggerEffect::MultiFeedback {
            strengths: [0, 0, 2, 3, 4, 5, 6, 7, 8, 8],
        },
        Kind::MultiVibration => TriggerEffect::MultiVibration {
            amplitudes: [0, 0, 0, 3, 5, 7, 8, 8, 8, 8],
            frequency: 20,
        },
        Kind::Bow => TriggerEffect::Bow {
            start: 1,
            end: 6,
            strength: 6,
            snap: 7,
        },
        Kind::Galloping => TriggerEffect::Galloping {
            start: 0,
            end: 9,
            first_foot: 2,
            second_foot: 5,
            frequency: 5,
        },
        Kind::Machine => TriggerEffect::Machine {
            start: 1,
            end: 9,
            amp_a: 2,
            amp_b: 6,
            frequency: 7,
            period: 4,
        },
        Kind::Preset => TriggerEffect::Preset {
            preset: TriggerPreset::Hard,
        },
    }
}

/// Zone strip: 10 bars of the effect's per-zone level, with the live
/// trigger position drawn over it.
fn zone_strip(ui: &mut egui::Ui, e: &TriggerEffect, pressure: u8) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 86.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(10), FIELD);
    let inner = rect.shrink2(egui::vec2(10.0, 10.0));
    let z = e.zone_preview();
    let vib = e.is_vibration();
    let gap = 4.0;
    let w = (inner.width() - gap * 9.0) / 10.0;
    for (i, v) in z.iter().enumerate() {
        let x = inner.left() + i as f32 * (w + gap);
        let col = Rect::from_min_max(pos2(x, inner.top()), pos2(x + w, inner.bottom() - 14.0));
        p.rect_filled(col, CornerRadius::same(4), CARD_HI);
        if *v > 0.0 {
            let mut f = col;
            f.set_top(col.bottom() - col.height() * v);
            let c = if vib { VIOLET } else { ACCENT };
            p.rect_filled(f, CornerRadius::same(4), mix(with_alpha(c, 255), c, *v));
            if vib {
                // Ripple marks for vibration zones.
                let mut y = f.top() + 4.0;
                while y < f.bottom() - 2.0 {
                    p.line_segment(
                        [pos2(f.left() + 3.0, y), pos2(f.right() - 3.0, y)],
                        Stroke::new(1.0_f32, with_alpha(egui::Color32::WHITE, 50)),
                    );
                    y += 5.0;
                }
            }
        }
        p.text(
            pos2(col.center().x, inner.bottom() - 4.0),
            egui::Align2::CENTER_CENTER,
            format!("{i}"),
            egui::FontId::proportional(10.5),
            FAINT,
        );
    }
    // Live position.
    let t = pressure as f32 / 255.0;
    let x = egui::lerp(inner.left()..=inner.right(), t);
    p.line_segment(
        [pos2(x, rect.top() + 4.0), pos2(x, rect.bottom() - 18.0)],
        Stroke::new(2.0_f32, WARN),
    );
    p.circle_filled(pos2(x, rect.top() + 6.0), 3.5, WARN);
}

/// Editable bars for the zone effects. Drag to paint values 0..=8.
fn zone_editor(ui: &mut egui::Ui, v: &mut [u8; 10], color: egui::Color32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 120.0),
        Sense::click_and_drag(),
    );
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(10), FIELD);
    let inner = rect.shrink(10.0);
    let gap = 4.0;
    let w = (inner.width() - gap * 9.0) / 10.0;
    let mut changed = false;
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.dragged() || resp.clicked() {
            let i = (((pos.x - inner.left()) / (w + gap)).floor() as i32).clamp(0, 9) as usize;
            let val = (((inner.bottom() - pos.y) / inner.height()) * 8.0)
                .round()
                .clamp(0.0, 8.0) as u8;
            if v[i] != val {
                v[i] = val;
                changed = true;
            }
        }
    }
    for (i, val) in v.iter().enumerate() {
        let x = inner.left() + i as f32 * (w + gap);
        let col = Rect::from_min_max(pos2(x, inner.top()), pos2(x + w, inner.bottom()));
        p.rect_filled(col, CornerRadius::same(4), CARD_HI);
        if *val > 0 {
            let mut f = col;
            f.set_top(col.bottom() - col.height() * (*val as f32 / 8.0));
            p.rect_filled(f, CornerRadius::same(4), color);
        }
        p.text(
            pos2(col.center().x, col.top() + 9.0),
            egui::Align2::CENTER_CENTER,
            format!("{val}"),
            egui::FontId::proportional(11.0),
            MUTED,
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    changed
}

fn editor(ui: &mut egui::Ui, id: &str, e: &mut TriggerEffect) {
    let mut k = kind(e);
    let before = k;
    egui::ComboBox::from_id_salt(format!("{id}-kind"))
        .width(240.0)
        .selected_text(e.kind_name())
        .show_ui(ui, |ui| {
            for (kk, label) in [
                (Kind::Off, "Off"),
                (Kind::Feedback, "Resistance"),
                (Kind::Weapon, "Weapon (click point)"),
                (Kind::Vibration, "Vibration"),
                (Kind::Slope, "Slope (ramping resistance)"),
                (Kind::MultiFeedback, "Zones: resistance"),
                (Kind::MultiVibration, "Zones: vibration"),
                (Kind::Bow, "Bow"),
                (Kind::Galloping, "Galloping"),
                (Kind::Machine, "Machine"),
                (Kind::Preset, "Classic preset"),
            ] {
                ui.selectable_value(&mut k, kk, label);
            }
        });
    if k != before {
        *e = default_for(k);
    }
    ui.add_space(6.0);
    let zone = |ui: &mut egui::Ui, label: &str, v: &mut u8, lo: u8, hi: u8| {
        slider_row(ui, label, "", v, lo..=hi, "");
    };
    match e {
        TriggerEffect::Off => muted(ui, "No resistance. The trigger feels normal."),
        TriggerEffect::Feedback { position, strength } => {
            zone(ui, "Starts at zone", position, 0, 9);
            zone(ui, "Strength", strength, 1, 8);
        }
        TriggerEffect::Weapon {
            start,
            end,
            strength,
        } => {
            zone(ui, "Resistance from", start, 2, 7);
            *end = (*end).max(*start + 1);
            zone(ui, "Breaks at", end, *start + 1, 8);
            zone(ui, "Strength", strength, 1, 8);
        }
        TriggerEffect::Vibration {
            position,
            amplitude,
            frequency,
        } => {
            zone(ui, "Starts at zone", position, 0, 9);
            zone(ui, "Amplitude", amplitude, 1, 8);
            row(ui, "Frequency", "", |ui| {
                ui.add(egui::Slider::new(frequency, 1..=80).suffix(" Hz"))
            });
        }
        TriggerEffect::Slope {
            start,
            end,
            start_strength,
            end_strength,
        } => {
            zone(ui, "From zone", start, 0, 8);
            *end = (*end).max(*start + 1);
            zone(ui, "To zone", end, *start + 1, 9);
            zone(ui, "Start strength", start_strength, 1, 8);
            zone(ui, "End strength", end_strength, 1, 8);
        }
        TriggerEffect::MultiFeedback { strengths } => {
            muted(ui, "Drag across the bars to paint resistance per zone.");
            zone_editor(ui, strengths, ACCENT);
        }
        TriggerEffect::MultiVibration {
            amplitudes,
            frequency,
        } => {
            muted(ui, "Drag across the bars to paint vibration per zone.");
            zone_editor(ui, amplitudes, VIOLET);
            row(ui, "Frequency", "", |ui| {
                ui.add(egui::Slider::new(frequency, 1..=80).suffix(" Hz"))
            });
        }
        TriggerEffect::Bow {
            start,
            end,
            strength,
            snap,
        } => {
            zone(ui, "Draw from", start, 0, 7);
            *end = (*end).max(*start + 1);
            zone(ui, "Draw to", end, *start + 1, 8);
            zone(ui, "Tension", strength, 1, 8);
            zone(ui, "Snap back", snap, 1, 8);
        }
        TriggerEffect::Galloping {
            start,
            end,
            first_foot,
            second_foot,
            frequency,
        } => {
            zone(ui, "From zone", start, 0, 8);
            *end = (*end).max(*start + 1);
            zone(ui, "To zone", end, *start + 1, 9);
            zone(ui, "Second foot", second_foot, 1, 7);
            *first_foot = (*first_foot).min(*second_foot - 1);
            zone(ui, "First foot", first_foot, 0, *second_foot - 1);
            row(ui, "Speed", "", |ui| {
                ui.add(egui::Slider::new(frequency, 1..=40).suffix(" Hz"))
            });
        }
        TriggerEffect::Machine {
            start,
            end,
            amp_a,
            amp_b,
            frequency,
            period,
        } => {
            zone(ui, "From zone", start, 0, 8);
            *end = (*end).max(*start + 1);
            zone(ui, "To zone", end, *start + 1, 9);
            zone(ui, "Amplitude A", amp_a, 0, 7);
            zone(ui, "Amplitude B", amp_b, 0, 7);
            row(ui, "Frequency", "", |ui| {
                ui.add(egui::Slider::new(frequency, 1..=80).suffix(" Hz"))
            });
            zone(ui, "Period", period, 0, 20);
        }
        TriggerEffect::Preset { preset } => {
            row(ui, "Preset", "Classic feel presets", |ui| {
                egui::ComboBox::from_id_salt(format!("{id}-preset"))
                    .selected_text(preset.name())
                    .show_ui(ui, |ui| {
                        for pr in TriggerPreset::ALL {
                            ui.selectable_value(preset, pr, pr.name());
                        }
                    });
            });
        }
        TriggerEffect::Custom { .. } | TriggerEffect::Raw { .. } => {
            let b = e.encode();
            muted(ui, "Imported raw effect bytes:");
            ui.label(RichText::new(format!("{:02X?}", b)).monospace().color(TEXT));
        }
    }
}

impl App {
    pub(crate) fn page_triggers(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Profile,
        dev: Option<&Arc<Device>>,
    ) {
        page_header(
            ui,
            "Adaptive triggers",
            "Resistance and vibration for L2 and R2. Pull a trigger to feel it while you edit.",
        );
        let input = dev.map(|d| d.live.lock().input).unwrap_or_default();
        let mut copy: Option<(bool, TriggerEffect)> = None;
        two_columns(ui, |ui, col| {
            let left = col == 0;
            let (title, pressure) = if left {
                ("L2", input.l2)
            } else {
                ("R2", input.r2)
            };
            let e = if left {
                &mut p.triggers.left
            } else {
                &mut p.triggers.right
            };
            card(ui, title, "", |ui| {
                zone_strip(ui, e, pressure);
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for (name, eff) in presets() {
                        let sel = *e == eff;
                        let b = egui::Button::new(RichText::new(name).size(12.5).color(if sel {
                            egui::Color32::WHITE
                        } else {
                            TEXT
                        }))
                        .fill(if sel { ACCENT } else { CARD_HI })
                        .corner_radius(CornerRadius::same(14));
                        if ui.add(b).clicked() {
                            *e = eff;
                        }
                    }
                });
                ui.add_space(10.0);
                editor(ui, title, e);
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if button(
                        ui,
                        if left {
                            "Copy to R2  →"
                        } else {
                            "←  Copy to L2"
                        },
                    )
                    .clicked()
                    {
                        copy = Some((left, e.clone()));
                    }
                    let bytes = e.encode();
                    ui.label(
                        RichText::new(format!("{:02X?}", bytes))
                            .monospace()
                            .size(11.0)
                            .color(FAINT),
                    );
                });
            });
        });
        if let Some((from_left, e)) = copy {
            if from_left {
                p.triggers.right = e;
            } else {
                p.triggers.left = e;
            }
        }
    }
}
