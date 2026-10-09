use std::sync::atomic::Ordering;
use std::sync::Arc;

use ds_proto::input::Button;
use eframe::egui::{self, pos2, CornerRadius, RichText, Sense, Stroke, StrokeKind};

use super::button_choices;
use crate::device::Device;
use crate::profile::*;
use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

fn button_combo(ui: &mut egui::Ui, id: &str, b: &mut Button, edge: bool) {
    egui::ComboBox::from_id_salt(id)
        .width(150.0)
        .selected_text(b.short_name())
        .show_ui(ui, |ui| {
            for c in button_choices(edge) {
                ui.selectable_value(b, c, c.short_name());
            }
        });
}

fn gyro_bars(ui: &mut egui::Ui, dps: [f32; 3]) {
    for (label, v) in [("Pitch", dps[0]), ("Yaw", dps[1]), ("Roll", dps[2])] {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).size(12.5).color(MUTED));
            let (r, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width() - 70.0, 10.0),
                Sense::hover(),
            );
            let p = ui.painter();
            p.rect_filled(r, CornerRadius::same(5), FIELD);
            let c = r.center().x;
            let t = (v / 360.0).clamp(-1.0, 1.0);
            let x = c + t * r.width() / 2.0;
            let bar = egui::Rect::from_min_max(pos2(c.min(x), r.top()), pos2(c.max(x), r.bottom()));
            p.rect_filled(bar, CornerRadius::same(5), ACCENT);
            p.line_segment(
                [pos2(c, r.top() - 2.0), pos2(c, r.bottom() + 2.0)],
                Stroke::new(1.0_f32, FAINT),
            );
            ui.label(
                RichText::new(format!("{v:+5.0}°/s"))
                    .size(12.0)
                    .monospace()
                    .color(TEXT),
            );
        });
    }
}

fn touch_preview(ui: &mut egui::Ui, input: &ds_proto::InputState) {
    let w = ui.available_width();
    let h = w * 1080.0 / 1920.0 * 0.6;
    let (r, _) = ui.allocate_exact_size(egui::vec2(w, h), Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, CornerRadius::same(12), FIELD);
    p.rect_stroke(
        r,
        CornerRadius::same(12),
        Stroke::new(1.0_f32, BORDER),
        StrokeKind::Inside,
    );
    p.line_segment(
        [
            pos2(r.center().x, r.top() + 8.0),
            pos2(r.center().x, r.bottom() - 8.0),
        ],
        Stroke::new(1.0_f32, with_alpha(BORDER, 140)),
    );
    for (k, t) in input.touch.iter().enumerate() {
        if t.active {
            let x = egui::lerp(r.left()..=r.right(), t.x as f32 / 1919.0);
            let y = egui::lerp(r.top()..=r.bottom(), t.y as f32 / 1079.0);
            let c = if k == 0 { ACCENT } else { VIOLET };
            p.circle_filled(pos2(x, y), 16.0, with_alpha(c, 60));
            p.circle_filled(pos2(x, y), 7.0, c);
        }
    }
    if input.buttons.has(Button::Touchpad) {
        p.rect_stroke(
            r,
            CornerRadius::same(12),
            Stroke::new(2.0_f32, ACCENT),
            StrokeKind::Inside,
        );
    }
}

fn stick_preview(ui: &mut egui::Ui, x: u8, y: u8, deadzone: f32, active: bool) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(96.0, 96.0), Sense::hover());
    let p = ui.painter();
    let c = r.center();
    let rad = 44.0;
    p.circle_filled(c, rad, FIELD);
    p.circle_stroke(c, rad, Stroke::new(1.0_f32, BORDER));
    p.circle_filled(c, rad * deadzone.clamp(0.0, 1.0), with_alpha(WARN, 30));
    p.circle_stroke(
        c,
        rad * deadzone.clamp(0.0, 1.0),
        Stroke::new(1.0_f32, with_alpha(WARN, 120)),
    );
    let fx = (x as f32 - 128.0) / 128.0;
    let fy = (y as f32 - 128.0) / 128.0;
    let dot = c + egui::vec2(fx, fy) * rad;
    p.line_segment([c, dot], Stroke::new(1.0_f32, with_alpha(ACCENT, 120)));
    p.circle_filled(dot, 6.0, if active { ACCENT } else { MUTED });
}

impl App {
    pub(crate) fn page_input(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Profile,
        dev: Option<&Arc<Device>>,
    ) {
        page_header(
            ui,
            "Motion & touch",
            "Aim with the gyro, drive the cursor with the touchpad, or turn a stick into a mouse, scroll wheel, or keys.",
        );
        let mut live = dev.map(|d| d.snapshot()).unwrap_or_default();
        if dev.is_none() {
            let s = &mut live.input;
            (s.lx, s.ly, s.rx, s.ry) = (128, 128, 128, 128);
        }
        let edge = dev.map(|d| d.model.is_edge()).unwrap_or(true);

        two_columns(ui, |ui, col| {
            if col == 0 {
                card(
                    ui,
                    "Gyro mouse",
                    "Tilt and turn the controller to move the cursor or aim",
                    |ui| {
                        let g = &mut p.gyro;
                        let mut on = g.mode == GyroMode::Mouse;
                        if toggle_row(ui, "Enabled", "", &mut on) {
                            g.mode = if on { GyroMode::Mouse } else { GyroMode::Off };
                        }
                        if g.mode == GyroMode::Mouse {
                            let (mut kind, mut b) = match g.activation {
                                GyroActivation::Always => (0, Button::R1),
                                GyroActivation::WhileHeld(b) => (1, b),
                                GyroActivation::WhileNotHeld(b) => (2, b),
                                GyroActivation::Toggle(b) => (3, b),
                            };
                            row(ui, "Active", "", |ui| {
                                if kind != 0 {
                                    button_combo(ui, "gyro-btn", &mut b, edge);
                                }
                                egui::ComboBox::from_id_salt("gyro-act")
                                    .width(150.0)
                                    .selected_text(
                                        [
                                            "Always",
                                            "While holding",
                                            "Unless holding",
                                            "Toggle with",
                                        ][kind],
                                    )
                                    .show_ui(ui, |ui| {
                                        for (i, l) in [
                                            "Always",
                                            "While holding",
                                            "Unless holding",
                                            "Toggle with",
                                        ]
                                        .iter()
                                        .enumerate()
                                        {
                                            ui.selectable_value(&mut kind, i, *l);
                                        }
                                    });
                            });
                            g.activation = match kind {
                                0 => GyroActivation::Always,
                                1 => GyroActivation::WhileHeld(b),
                                2 => GyroActivation::WhileNotHeld(b),
                                _ => GyroActivation::Toggle(b),
                            };
                            slider_row(
                                ui,
                                "Sensitivity",
                                "Pixels per degree",
                                &mut g.sensitivity,
                                1.0..=80.0,
                                "",
                            );
                            slider_row(
                                ui,
                                "Vertical ratio",
                                "",
                                &mut g.vertical_ratio,
                                0.2..=2.0,
                                "×",
                            );
                            row(ui, "Horizontal from", "", |ui| {
                                segmented(
                                    ui,
                                    &mut g.axis,
                                    &[
                                        (GyroAxis::Yaw, "Turn"),
                                        (GyroAxis::Roll, "Lean"),
                                        (GyroAxis::Combined, "Both"),
                                    ],
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Invert").color(TEXT));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.checkbox(&mut g.invert_y, "Vertical");
                                        ui.checkbox(&mut g.invert_x, "Horizontal");
                                    },
                                );
                            });
                            slider_row(
                                ui,
                                "Steadiness",
                                "Softens tiny hand tremor",
                                &mut g.deadzone,
                                0.0..=8.0,
                                " °/s",
                            );
                            let mut sm = (g.smoothing * 100.0).round();
                            if slider_row(
                                ui,
                                "Smoothing",
                                "Only slow motion is smoothed",
                                &mut sm,
                                0.0..=100.0,
                                "%",
                            ) {
                                g.smoothing = sm / 100.0;
                            }
                            let mut ac = (g.acceleration * 100.0).round();
                            if slider_row(
                                ui,
                                "Acceleration",
                                "Extra speed for fast flicks",
                                &mut ac,
                                0.0..=200.0,
                                "%",
                            ) {
                                g.acceleration = ac / 100.0;
                            }
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            let (t, c) = if live.calibrating {
                                ("Calibrating — keep still", WARN)
                            } else if live.gyro_active {
                                ("Moving the cursor", ACCENT)
                            } else {
                                ("Live gyro", MUTED)
                            };
                            pill(ui, t, c);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Some(d) = dev {
                                        if button(ui, "Calibrate")
                                            .on_hover_text(
                                                "Lay the controller flat and still for 2 seconds",
                                            )
                                            .clicked()
                                        {
                                            d.calibrate_request.store(true, Ordering::Release);
                                        }
                                    }
                                },
                            );
                        });
                        gyro_bars(ui, live.gyro_dps);
                        muted(ui, "Calibration also happens on its own whenever the controller rests still for a moment.");
                    },
                );
            } else {
                card(ui, "Touchpad", "", |ui| {
                    let t = &mut p.touchpad;
                    let mut on = t.mode == TouchMode::Mouse;
                    if toggle_row(
                        ui,
                        "Touchpad as mouse",
                        "Off leaves the touchpad to the game",
                        &mut on,
                    ) {
                        t.mode = if on {
                            TouchMode::Mouse
                        } else {
                            TouchMode::Passthrough
                        };
                    }
                    touch_preview(ui, &live.input);
                    if t.mode == TouchMode::Mouse {
                        slider_row(ui, "Speed", "", &mut t.sensitivity, 0.1..=4.0, "×");
                        slider_row(ui, "Acceleration", "Faster swipes move further; 0 is steady", &mut t.acceleration, 0.0..=2.0, "");
                        toggle_row(
                            ui,
                            "Tap to click",
                            "One finger is a left click, two a right click",
                            &mut t.tap_to_click,
                        );
                        toggle_row(
                            ui,
                            "Press to click",
                            "Press with two fingers for a right click; hold the press and slide to drag",
                            &mut t.click_to_click,
                        );
                        if t.click_to_click {
                            toggle_row(
                                ui,
                                "Right side is right click",
                                "Off: a press anywhere is a left click",
                                &mut t.right_half_right_click,
                            );
                        }
                        toggle_row(ui, "Two-finger scroll", "", &mut t.two_finger_scroll);
                        if t.two_finger_scroll {
                            slider_row(ui, "Scroll speed", "", &mut t.scroll_speed, 0.1..=4.0, "×");
                            toggle_row(ui, "Natural scrolling", "The page follows your fingers", &mut t.natural_scroll);
                        }
                    }
                });
                ui.add_space(12.0);
                card(
                    ui,
                    "Sticks",
                    "Without a virtual controller the game still sees the stick; with one, a stick set to mouse, scroll, or keys reads centered",
                    |ui| {
                        for (left, label) in [(true, "Left stick"), (false, "Right stick")] {
                            let (x, y) = if left {
                                (live.input.lx, live.input.ly)
                            } else {
                                (live.input.rx, live.input.ry)
                            };
                            let s = if left {
                                &mut p.sticks.left
                            } else {
                                &mut p.sticks.right
                            };
                            ui.horizontal(|ui| {
                                stick_preview(
                                    ui,
                                    x,
                                    y,
                                    s.deadzone,
                                    s.mode != StickMode::Passthrough,
                                );
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(label).font(heading(14.0)));
                                    egui::ComboBox::from_id_salt(format!("stick-{left}"))
                                        .width(170.0)
                                        .selected_text(match s.mode {
                                            StickMode::Passthrough => "Game only",
                                            StickMode::Mouse => "Mouse",
                                            StickMode::Scroll => "Scroll wheel",
                                            StickMode::Wasd => "W A S D",
                                            StickMode::Arrows => "Arrow keys",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Passthrough,
                                                "Game only",
                                            );
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Mouse,
                                                "Mouse",
                                            );
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Scroll,
                                                "Scroll wheel",
                                            );
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Wasd,
                                                "W A S D",
                                            );
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Arrows,
                                                "Arrow keys",
                                            );
                                        });
                                });
                            });
                            if s.mode != StickMode::Passthrough {
                                let mut dz = (s.deadzone * 100.0).round();
                                if slider_row(ui, "Deadzone", "", &mut dz, 0.0..=60.0, "%") {
                                    s.deadzone = dz / 100.0;
                                }
                                match s.mode {
                                    StickMode::Mouse => {
                                        slider_row(
                                            ui,
                                            "Speed",
                                            "",
                                            &mut s.mouse_speed,
                                            200.0..=4000.0,
                                            " px/s",
                                        );
                                        slider_row(
                                            ui,
                                            "Curve",
                                            "Higher is finer near the center",
                                            &mut s.curve,
                                            1.0..=3.5,
                                            "",
                                        );
                                    }
                                    StickMode::Scroll => {
                                        slider_row(
                                            ui,
                                            "Scroll speed",
                                            "",
                                            &mut s.scroll_speed,
                                            0.1..=4.0,
                                            "×",
                                        );
                                    }
                                    _ => {
                                        let mut th = (s.key_threshold * 100.0).round();
                                        if slider_row(
                                            ui,
                                            "Press point",
                                            "",
                                            &mut th,
                                            10.0..=95.0,
                                            "%",
                                        ) {
                                            s.key_threshold = th / 100.0;
                                        }
                                    }
                                }
                            }
                            if left {
                                ui.add_space(4.0);
                                ui.separator();
                            }
                        }
                    },
                );
            }
        });
    }
}
