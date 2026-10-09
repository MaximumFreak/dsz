use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, color_picker, CornerRadius, Sense, Stroke};

use crate::device::Device;
use crate::profile::*;
use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

const SWATCHES: [[u8; 3]; 12] = [
    [0x2D, 0x7F, 0xF9],
    [0x00, 0x30, 0xFF],
    [0x8B, 0x5C, 0xF6],
    [0xE9, 0x3D, 0x82],
    [0xFF, 0x20, 0x20],
    [0xFF, 0x6A, 0x00],
    [0xFF, 0xC4, 0x00],
    [0x30, 0xE0, 0x70],
    [0x00, 0xD4, 0xC0],
    [0x00, 0xB7, 0xFF],
    [0xFF, 0xFF, 0xFF],
    [0x00, 0x00, 0x00],
];

impl App {
    pub(crate) fn page_lighting(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Profile,
        dev: Option<&Arc<Device>>,
    ) {
        page_header(ui, "Lighting", "Lightbar, player indicator, and mute light. Changes show on the controller as you make them.");
        let input = dev.map(|d| d.live.lock().input).unwrap_or_default();
        let now = Instant::now();

        two_columns(ui, |ui, col| {
            if col == 0 {
                card(ui, "Lightbar", "", |ui| {
                    // Live preview strip.
                    let color = self.preview.lightbar(p, &input, now);
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 46.0),
                        Sense::hover(),
                    );
                    let pt = ui.painter();
                    pt.rect_filled(r, CornerRadius::same(12), FIELD);
                    let bar = r.shrink2(egui::vec2(r.width() * 0.2, 17.0));
                    for k in (1..8).rev() {
                        pt.rect_filled(
                            bar.expand(k as f32 * 2.2),
                            CornerRadius::same(10 + k as u8 * 2),
                            with_alpha(rgb(color), (40 - k * 5) as u8),
                        );
                    }
                    pt.rect_filled(bar, CornerRadius::same(6), rgb(color));
                    ui.add_space(10.0);

                    let l = &mut p.lighting;
                    segmented(
                        ui,
                        &mut l.mode,
                        &[
                            (LightMode::Static, "Static"),
                            (LightMode::Rainbow, "Rainbow"),
                            (LightMode::Breathing, "Breathing"),
                            (LightMode::Strobe, "Strobe"),
                            (LightMode::Battery, "Battery"),
                            (LightMode::Off, "Off"),
                        ],
                    );
                    ui.add_space(8.0);
                    match l.mode {
                        LightMode::Static | LightMode::Breathing | LightMode::Strobe => {
                            row(ui, if l.mode == LightMode::Static { "Color" } else { "Color A" }, "", |ui| {
                                color_picker::color_edit_button_srgb(ui, &mut l.color);
                            });
                            if let Some(c) = swatches(ui, l.color, &SWATCHES) {
                                l.color = c;
                            }
                            if l.mode != LightMode::Static {
                                row(ui, "Color B", "Breathing fades to this, strobe flashes to it", |ui| {
                                    color_picker::color_edit_button_srgb(ui, &mut l.color2);
                                });
                            }
                        }
                        LightMode::Rainbow => muted(ui, "Cycles the full hue wheel."),
                        LightMode::Battery => muted(ui, "Red when low, amber in the middle, green when full. Pulses while charging."),
                        LightMode::Off => muted(ui, "Lightbar off."),
                    }
                    if l.mode != LightMode::Off {
                        slider_row(ui, "Brightness", "", &mut l.brightness, 0..=100, "%");
                    }
                    if matches!(
                        l.mode,
                        LightMode::Rainbow | LightMode::Breathing | LightMode::Strobe
                    ) {
                        let mut sp = (l.speed * 100.0).round();
                        if slider_row(ui, "Speed", "", &mut sp, 0.0..=100.0, "%") {
                            l.speed = sp / 100.0;
                        }
                    }
                });
                ui.add_space(12.0);
                card(
                    ui,
                    "Low battery warning",
                    "Blinks the lightbar twice every few seconds when the battery runs low",
                    |ui| {
                        let l = &mut p.lighting;
                        toggle_row(
                            ui,
                            "Blink when low",
                            "Not while charging",
                            &mut l.low_battery_flash,
                        );
                        if l.low_battery_flash {
                            slider_row(
                                ui,
                                "Threshold",
                                "",
                                &mut l.low_battery_threshold,
                                5..=50,
                                "%",
                            );
                            row(ui, "Warning color", "", |ui| {
                                color_picker::color_edit_button_srgb(ui, &mut l.low_battery_color);
                            });
                        }
                    },
                );
            } else {
                card(
                    ui,
                    "Player indicator",
                    "The five LEDs under the touchpad",
                    |ui| {
                        let l = &mut p.lighting;
                        // LED preview / custom editor.
                        let mask = match l.player_leds {
                            PlayerLeds::Player(n) => ds_proto::output::player_pattern(n),
                            PlayerLeds::Custom(m) => m,
                            PlayerLeds::Off => 0,
                            PlayerLeds::Battery => {
                                let n = (input.battery_percent / 20).min(4) as usize;
                                [0x01, 0x03, 0x07, 0x0F, 0x1F][n]
                            }
                        };
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 5.0 * 34.0) / 2.0);
                            for k in 0..5u8 {
                                let (r, resp) =
                                    ui.allocate_exact_size(egui::vec2(34.0, 34.0), Sense::click());
                                let lit = mask & (1 << k) != 0;
                                let pt = ui.painter();
                                if lit {
                                    pt.circle_filled(
                                        r.center(),
                                        13.0,
                                        with_alpha(egui::Color32::WHITE, 30),
                                    );
                                }
                                pt.circle_filled(
                                    r.center(),
                                    7.0,
                                    if lit { egui::Color32::WHITE } else { BORDER },
                                );
                                if resp.hovered() {
                                    pt.circle_stroke(r.center(), 11.0, Stroke::new(1.0_f32, MUTED));
                                }
                                if resp.clicked() {
                                    l.player_leds = PlayerLeds::Custom(mask ^ (1 << k));
                                }
                            }
                        });
                        muted(ui, "Click an LED to make a custom pattern.");
                        ui.add_space(4.0);
                        let mut kind = match l.player_leds {
                            PlayerLeds::Player(n) => n as i32,
                            PlayerLeds::Custom(_) => 10,
                            PlayerLeds::Battery => 11,
                            PlayerLeds::Off => 0,
                        };
                        if segmented(
                            ui,
                            &mut kind,
                            &[
                                (1, "P1"),
                                (2, "P2"),
                                (3, "P3"),
                                (4, "P4"),
                                (5, "P5"),
                                (10, "Custom"),
                                (11, "Battery"),
                                (0, "Off"),
                            ],
                        ) {
                            l.player_leds = match kind {
                                1..=5 => PlayerLeds::Player(kind as u8),
                                10 => PlayerLeds::Custom(mask),
                                11 => PlayerLeds::Battery,
                                _ => PlayerLeds::Off,
                            };
                        }
                        ui.add_space(6.0);
                        row(ui, "LED brightness", "Player and mute LEDs", |ui| {
                            segmented(
                                ui,
                                &mut l.led_brightness,
                                &[
                                    (LedBrightness::Low, "Low"),
                                    (LedBrightness::Medium, "Medium"),
                                    (LedBrightness::High, "High"),
                                ],
                            );
                        });
                    },
                );
                ui.add_space(12.0);
                card(ui, "Mute button light", "", |ui| {
                    let l = &mut p.lighting;
                    segmented(
                        ui,
                        &mut l.mute_led,
                        &[
                            (MuteLed::Off, "Off"),
                            (MuteLed::On, "On"),
                            (MuteLed::Pulse, "Pulse"),
                            (MuteLed::Toggle, "Toggle with button"),
                        ],
                    );
                    if l.mute_led == MuteLed::Toggle {
                        ui.add_space(4.0);
                        muted(ui, "Press the mute button to switch the light on and off.");
                    }
                });
            }
        });
    }
}
