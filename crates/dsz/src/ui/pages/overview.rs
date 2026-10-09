use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, RichText};

use crate::device::{Device, HapticPulse, Link};
use crate::profile::Profile;
use crate::ui::controller_view::{self, View};
use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

fn stat(ui: &mut egui::Ui, k: &str, v: impl Into<RichText>) {
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);
        ui.label(RichText::new(k).color(MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(v.into());
        });
    });
}

impl App {
    pub(crate) fn page_overview(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Profile,
        dev: Option<&Arc<Device>>,
    ) {
        let Some(d) = dev else {
            self.empty_state(ui, p);
            return;
        };
        let live = d.snapshot();
        let input = live.input;
        let lightbar = self.preview_color(p, &input, d);
        let name = d.display_name(&self.engine);
        page_header(
            ui,
            &name,
            &format!(
                "{} over {}  ·  profile \"{}\"",
                d.model.name(),
                if d.is_bluetooth() { "Bluetooth" } else { "USB" },
                self.engine.profile_name_for(d)
            ),
        );

        let avail = ui.available_width();
        let wide = avail > 900.0;
        let view_w = if wide { avail * 0.6 } else { avail };
        // Take each lock once, up front: a second lock of the same mutex
        // inside one expression waits on the first forever.
        let overrides = d.overrides.lock().clone();
        let toggles = *d.toggles.lock();
        let link = d.link.lock().clone();
        let draw_view = |ui: &mut egui::Ui| {
            card(ui, "", "", |ui| {
                let h = (view_w * 0.62).clamp(260.0, 520.0);
                controller_view::draw(
                    ui,
                    egui::vec2(ui.available_width(), h),
                    &View {
                        input: &input,
                        lightbar,
                        player_leds: overrides.player_leds.unwrap_or_else(|| {
                            crate::composer::profile_player_leds(&p.lighting, &input)
                        }),
                        mute_led: !matches!(p.lighting.mute_led, crate::profile::MuteLed::Off)
                            && (p.lighting.mute_led != crate::profile::MuteLed::Toggle
                                || toggles.mute_engaged),
                        model: d.model,
                        connected: link == Link::Live || link == Link::Quiet,
                    },
                );
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if primary_button(ui, "Identify")
                        .on_hover_text("Flash the lightbar and buzz")
                        .clicked()
                    {
                        d.identify();
                    }
                    if button(ui, "Rumble test").clicked() {
                        d.test_rumble(200, 160, 400);
                    }
                    if d.is_bluetooth() && p.haptics.enabled {
                        if button(ui, "Haptic L").clicked() {
                            d.pulse(HapticPulse {
                                left: 0.9,
                                right: 0.0,
                                freq: 160.0,
                                ms: 140.0,
                            });
                        }
                        if button(ui, "Haptic R").clicked() {
                            d.pulse(HapticPulse {
                                left: 0.0,
                                right: 0.9,
                                freq: 160.0,
                                ms: 140.0,
                            });
                        }
                    }
                    if button(
                        ui,
                        if live.calibrating {
                            "Calibrating…"
                        } else {
                            "Calibrate gyro"
                        },
                    )
                    .on_hover_text("Put the controller down on a flat surface first")
                    .clicked()
                    {
                        d.calibrate_request.store(true, Ordering::Release);
                    }
                    if d.is_bluetooth() && danger_button(ui, "Turn off").clicked() {
                        let mac = d.mac.lock().clone();
                        std::thread::spawn(move || crate::platform::bt_disconnect(&mac));
                    }
                });
            });
        };
        let draw_stats = |ui: &mut egui::Ui| {
            card(ui, "Status", "", |ui| {
                let link = d.link.lock().clone();
                let (lt, lc) = match &link {
                    Link::Live => ("Connected".to_string(), GOOD),
                    Link::Quiet => ("No signal".to_string(), WARN),
                    Link::Opening => ("Connecting".to_string(), MUTED),
                    Link::Error(e) => (e.clone(), BAD),
                };
                stat(ui, "Link", RichText::new(lt).color(lc));
                let bat = input.battery_percent;
                let bcol = if bat <= 15 {
                    BAD
                } else if bat <= 30 {
                    WARN
                } else {
                    GOOD
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Battery").color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let status = match input.battery_status {
                            ds_proto::BatteryStatus::Charging => " · charging",
                            ds_proto::BatteryStatus::Full => " · full",
                            ds_proto::BatteryStatus::Error => " · error",
                            _ => "",
                        };
                        ui.label(RichText::new(format!("{bat}%{status}")).color(TEXT));
                        meter(ui, bat as f32 / 100.0, bcol, 90.0);
                    });
                });
                let s = &d.stats;
                stat(
                    ui,
                    "Input rate",
                    format!("{} Hz", s.input_hz.load(Ordering::Relaxed)),
                );
                stat(
                    ui,
                    "Control writes",
                    format!("{} /s", s.output_hz.load(Ordering::Relaxed)),
                );
                if d.is_bluetooth() {
                    stat(
                        ui,
                        "Haptic stream",
                        format!("{} packets/s", s.stream_hz.load(Ordering::Relaxed)),
                    );
                }
                stat(
                    ui,
                    "Write time",
                    format!(
                        "{:.2} ms",
                        s.write_us.load(Ordering::Relaxed) as f32 / 1000.0
                    ),
                );
                let errs =
                    s.write_errors.load(Ordering::Relaxed) + s.crc_errors.load(Ordering::Relaxed);
                stat(
                    ui,
                    "Recovered errors",
                    RichText::new(format!(
                        "{} write · {} CRC",
                        s.write_errors.load(Ordering::Relaxed),
                        s.crc_errors.load(Ordering::Relaxed)
                    ))
                    .color(if errs > 0 { WARN } else { TEXT }),
                );
                ui.separator();
                let mac = d.mac.lock().clone();
                stat(
                    ui,
                    "MAC",
                    RichText::new(if mac.is_empty() { "—".into() } else { mac }).monospace(),
                );
                let fw = d.firmware.lock().clone();
                stat(
                    ui,
                    "Firmware",
                    if fw.is_empty() { "—".to_string() } else { fw },
                );
                stat(
                    ui,
                    "Gyro",
                    if live.gyro_active {
                        RichText::new("driving the mouse").color(ACCENT)
                    } else {
                        RichText::new(format!(
                            "{:+.0} / {:+.0} / {:+.0} °/s",
                            live.gyro_dps[0], live.gyro_dps[1], live.gyro_dps[2]
                        ))
                    },
                );
            });
        };
        if wide {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(view_w - 12.0);
                    draw_view(ui);
                });
                ui.add_space(12.0);
                ui.vertical(|ui| draw_stats(ui));
            });
        } else {
            draw_view(ui);
            ui.add_space(12.0);
            draw_stats(ui);
        }
        ui.add_space(12.0);
        self.tips(ui);
    }

    fn preview_color(&mut self, p: &Profile, input: &ds_proto::InputState, d: &Device) -> [u8; 3] {
        let o = d.overrides.lock().rgb;
        o.unwrap_or_else(|| self.preview.lightbar(p, input, Instant::now()))
    }

    fn tips(&mut self, ui: &mut egui::Ui) {
        if !self.engine.settings.read().show_tips {
            return;
        }
        card(ui, "Tips", "", |ui| {
            for tip in [
                "Choose what games see under Virtual controller: this controller as-is, an Xbox 360 pad, or a DualSense with game haptics. Mappings that hide a button or press a controller button need one of the virtual pads.",
                "Haptics & audio streams to the actuators over Bluetooth only while there is something to play. The rest of the time the controller's own rumble emulation plays rumble.",
                "Profiles & games switches profiles per game. Games with native DualSense support switch to your DualSense profile without a rule.",
                "Close other controller apps (DS4Windows and the like) while DSZ runs; only one app can drive the controller.",
                "Steam Input also reads and writes the controller. If buttons double up, or lights and triggers flicker in a Steam game, turn Steam Input off for that game.",
                "Game mods that drive triggers and lights over UDP connect on port 6969 (Settings).",
            ] {
                muted(ui, &format!("• {tip}"));
            }
            ui.add_space(4.0);
            if button(ui, "Hide tips")
                .on_hover_text("Turn them back on in Settings")
                .clicked()
            {
                self.engine.settings.write().show_tips = false;
                self.engine.settings_changed();
            }
        });
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, p: &Profile) {
        page_header(
            ui,
            "No controller yet",
            "Connect a DualSense or DualSense Edge and it appears here within a second.",
        );
        card(ui, "", "", |ui| {
            let mut input = ds_proto::InputState::default();
            // Debug snapshots (DSZ_DEMO) show a controller with a few inputs live.
            let demo = cfg!(debug_assertions) && std::env::var_os("DSZ_DEMO").is_some();
            if demo {
                use ds_proto::input::Button;
                for b in [Button::Cross, Button::DpadLeft, Button::R1, Button::PaddleLeft] {
                    input.buttons.set(b, true);
                }
                (input.l2, input.lx, input.ly) = (150, 60, 90);
                input.touch[0].active = true;
                (input.touch[0].x, input.touch[0].y) = (1300, 400);
            }
            let lb = if demo {
                [0x00, 0x78, 0xD7]
            } else {
                self.preview.lightbar(p, &input, Instant::now())
            };
            controller_view::draw(
                ui,
                egui::vec2(ui.available_width(), 340.0),
                &View {
                    input: &input,
                    lightbar: lb,
                    player_leds: if demo { 0b00100 } else { 0 },
                    mute_led: demo,
                    model: if demo { ds_proto::Model::DualSenseEdge } else { ds_proto::Model::DualSense },
                    connected: demo,
                },
            );
            ui.add_space(8.0);
            ui.label(RichText::new("Pair over Bluetooth").font(heading(15.0)));
            muted(ui, "Hold Create and PS until the lightbar flashes, then add it from Windows Settings → Bluetooth & devices. A USB cable works too.");
        });
        ui.add_space(12.0);
        self.tips(ui);
    }
}
