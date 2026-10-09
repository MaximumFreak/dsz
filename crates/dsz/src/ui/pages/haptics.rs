use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::device::{Device, HapticPulse};
use crate::haptics::audio::AudioHub;
use crate::profile::*;
use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;

impl App {
    fn audio_device_list(&mut self) -> Vec<String> {
        let stale = self
            .audio_devices
            .as_ref()
            .map(|(t, _)| t.elapsed() > Duration::from_secs(5))
            .unwrap_or(true);
        if stale {
            self.audio_devices = Some((Instant::now(), AudioHub::output_devices()));
        }
        self.audio_devices
            .as_ref()
            .map(|x| x.1.clone())
            .unwrap_or_default()
    }

    pub(crate) fn page_haptics(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Profile,
        dev: Option<&Arc<Device>>,
    ) {
        page_header(
            ui,
            "Haptics & audio",
            "Wireless haptic feedback over Bluetooth: feel game audio, button clicks, and rumble through the voice-coil actuators.",
        );
        let bt = dev.map(|d| d.is_bluetooth()).unwrap_or(true);
        if !bt {
            note(
                ui,
                "This controller is on USB. PCM haptics stream over Bluetooth only, so rumble and button clicks use classic rumble here.",
                WARN,
            );
            ui.add_space(10.0);
        }
        let devices = self.audio_device_list();
        let capture = self.engine.audio.status.lock().clone();

        two_columns(ui, |ui, col| {
            if col == 0 {
                card(ui, "Wireless haptics", "", |ui| {
                    let h = &mut p.haptics;
                    toggle_row(
                        ui,
                        "Stream haptics",
                        "Over Bluetooth, while there is something to play: system audio, clicks, trigger texture, or game haptics",
                        &mut h.enabled,
                    );
                    if let Some(d) = dev {
                        let streaming = d.stats.stream_hz.load(Ordering::Relaxed);
                        let lv = [
                            d.haptic_level[0].load(Ordering::Relaxed) as f32 / 255.0,
                            d.haptic_level[1].load(Ordering::Relaxed) as f32 / 255.0,
                        ];
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Left").color(MUTED));
                            meter(ui, lv[0], ACCENT, 120.0);
                            ui.add_space(8.0);
                            ui.label(RichText::new("Right").color(MUTED));
                            meter(ui, lv[1], ACCENT, 120.0);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let (t, c) = if !h.enabled || !bt {
                                        ("off".to_string(), FAINT)
                                    } else if streaming > 0 {
                                        (format!("{streaming} pkt/s"), GOOD)
                                    } else if d.pcm_mode.load(Ordering::Relaxed) {
                                        ("idle".to_string(), MUTED)
                                    } else {
                                        ("standby".to_string(), MUTED)
                                    };
                                    pill(ui, &t, c);
                                },
                            );
                        });
                        let under = d.stats.stream_underruns.load(Ordering::Relaxed);
                        if under > 0 {
                            muted(
                                ui,
                                &format!("{under} audio gap(s) smoothed over since connect"),
                            );
                        }
                    }
                    if !h.enabled {
                        return;
                    }
                    ui.add_space(6.0);
                    row(ui, "Source", "", |ui| {
                        segmented(
                            ui,
                            &mut h.source,
                            &[
                                (AudioSource::None, "Events only"),
                                (AudioSource::SystemAudio, "System audio"),
                            ],
                        );
                    });
                    if h.source == AudioSource::SystemAudio {
                        row(
                            ui,
                            "Capture from",
                            "What the PC plays on this output",
                            |ui| {
                                let label = if h.device.is_empty() {
                                    "Windows default".to_string()
                                } else {
                                    h.device.clone()
                                };
                                egui::ComboBox::from_id_salt("audio-dev")
                                    .width(240.0)
                                    .selected_text(label)
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut h.device,
                                            String::new(),
                                            "Windows default",
                                        );
                                        for d in &devices {
                                            ui.selectable_value(&mut h.device, d.clone(), d);
                                        }
                                    });
                            },
                        );
                        let peak = self.engine.audio.peak();
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Input").color(MUTED));
                            meter(ui, peak, VIOLET, 160.0);
                            if capture.running {
                                ui.label(
                                    RichText::new(format!(
                                        "{} · {} Hz · {} ch",
                                        capture.device, capture.rate, capture.channels
                                    ))
                                    .size(12.0)
                                    .color(FAINT),
                                );
                            } else if let Some(e) = &capture.error {
                                ui.label(RichText::new(e).size(12.0).color(BAD));
                            }
                        });
                        ui.add_space(4.0);
                        slider_row(ui, "Intensity", "", &mut h.gain, 0.1..=6.0, "×");
                        slider_row(
                            ui,
                            "Low-pass",
                            "Higher keeps more detail; lower is punchier",
                            &mut h.low_pass_hz,
                            100.0..=1400.0,
                            " Hz",
                        );
                        slider_row(
                            ui,
                            "High-pass",
                            "Removes sub-bass rumble the actuators can't play",
                            &mut h.high_pass_hz,
                            5.0..=200.0,
                            " Hz",
                        );
                        toggle_row(
                            ui,
                            "Stereo",
                            "Left audio on the left grip, right on the right",
                            &mut h.stereo,
                        );
                    }
                    row(
                        ui,
                        "Latency",
                        "Buffer length: shorter is snappier, longer rides out radio hiccups",
                        |ui| {
                            segmented(
                                ui,
                                &mut h.latency,
                                &[
                                    (Latency::Short, "Short"),
                                    (Latency::Medium, "Medium"),
                                    (Latency::Long, "Long"),
                                ],
                            );
                        },
                    );
                });
            } else {
                card(
                    ui,
                    "Event haptics",
                    "Generated feedback that does not depend on game audio",
                    |ui| {
                        let h = &mut p.haptics;
                        toggle_row(
                            ui,
                            "Button clicks",
                            "A crisp tap under the grip you pressed",
                            &mut h.buttons.enabled,
                        );
                        if h.buttons.enabled {
                            let mut pct = (h.buttons.intensity * 100.0).round();
                            if slider_row(ui, "Strength", "", &mut pct, 5.0..=100.0, "%") {
                                h.buttons.intensity = pct / 100.0;
                            }
                            slider_row(
                                ui,
                                "Pitch",
                                "",
                                &mut h.buttons.frequency,
                                60.0..=400.0,
                                " Hz",
                            );
                            slider_row(
                                ui,
                                "Length",
                                "",
                                &mut h.buttons.duration_ms,
                                15.0..=150.0,
                                " ms",
                            );
                            if let Some(d) = dev {
                                if button(ui, "Try it").clicked() {
                                    d.pulse(HapticPulse {
                                        left: h.buttons.intensity,
                                        right: h.buttons.intensity,
                                        freq: h.buttons.frequency,
                                        ms: h.buttons.duration_ms,
                                    });
                                }
                            }
                        }
                        ui.separator();
                        toggle_row(
                            ui,
                            "Trigger texture",
                            "A hum that grows as you pull L2 / R2",
                            &mut h.triggers.enabled,
                        );
                        if h.triggers.enabled {
                            let mut pct = (h.triggers.intensity * 100.0).round();
                            if slider_row(ui, "Strength", "", &mut pct, 5.0..=100.0, "%") {
                                h.triggers.intensity = pct / 100.0;
                            }
                            slider_row(
                                ui,
                                "Pitch",
                                "",
                                &mut h.triggers.frequency,
                                20.0..=300.0,
                                " Hz",
                            );
                        }
                        if !bt || !h.enabled {
                            ui.add_space(4.0);
                            muted(ui, "Without streaming, button clicks fall back to a short classic rumble.");
                        }
                    },
                );
                ui.add_space(12.0);
                card(
                    ui,
                    "Rumble",
                    "Game and test rumble. The controller's own rumble emulation plays it, unless haptics are streaming",
                    |ui| {
                    let r = &mut p.rumble;
                    // Eight hardware steps of 12.5 %, shown as the power left.
                    let mut pct = 100.0 - 12.5 * r.power_reduction.min(7) as f32;
                    let moved = row(ui, "Power", "Controller rumble power", |ui| {
                        ui.add(
                            egui::Slider::new(&mut pct, 12.5..=100.0)
                                .step_by(12.5)
                                .fixed_decimals(1)
                                .suffix(" %")
                                .trailing_fill(true),
                        )
                        .changed()
                    });
                    if moved {
                        r.power_reduction = ((100.0 - pct) / 12.5).round().clamp(0.0, 7.0) as u8;
                    }
                    toggle_row(
                        ui,
                        "Improved rumble",
                        "Smoother rumble emulation (firmware 2.24 and later)",
                        &mut r.enhanced,
                    );
                    if bt && p.haptics.enabled {
                        let h = &mut p.haptics;
                        toggle_row(
                            ui,
                            "Rumble as haptics",
                            "Always play rumble on the haptic stream. Off: only while system audio, clicks, trigger texture, or game haptics are streaming",
                            &mut h.rumble_as_haptics,
                        );
                        let mut pct = (h.rumble_intensity * 100.0).round();
                        if slider_row(
                            ui,
                            "Haptic rumble strength",
                            "When rumble plays on the haptic stream",
                            &mut pct,
                            0.0..=200.0,
                            "%",
                        ) {
                            h.rumble_intensity = pct / 100.0;
                        }
                    }
                    if let Some(d) = dev {
                        ui.horizontal(|ui| {
                            if button(ui, "Heavy").clicked() {
                                d.test_rumble(255, 0, 450);
                            }
                            if button(ui, "Light").clicked() {
                                d.test_rumble(0, 255, 450);
                            }
                            if button(ui, "Both").clicked() {
                                d.test_rumble(200, 200, 450);
                            }
                        });
                    }
                });
            }
        });
    }
}
