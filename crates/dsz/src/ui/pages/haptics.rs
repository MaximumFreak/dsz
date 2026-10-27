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

    /// Speaker and headphone sound on a DualShock 4 over Bluetooth.
    fn ds4_audio_card(&mut self, ui: &mut egui::Ui, p: &mut Profile, d: &Arc<Device>) {
        let devices = self.audio_device_list();
        let capture = self.engine.audio.status.lock().clone();
        card(
            ui,
            crate::i18n::tr("DualShock 4 speaker / headphones (experimental)"),
            crate::i18n::tr("Sound on the controller over Bluetooth. Off until you turn on a source."),
            |ui| {
                if !d.ds4_audio_capable() {
                    muted(ui, crate::i18n::tr("Needs a Sony DualShock 4 over Bluetooth. Over USB, Windows already has the controller as a sound device."));
                    return;
                }
                let game_speaker = p.virtual_out.game_speaker;
                let a = &mut p.haptics.ds4_audio;
                toggle_row(
                    ui,
                    crate::i18n::tr("PC audio"),
                    crate::i18n::tr("Play what the PC plays on the controller"),
                    &mut a.pc_audio,
                );
                toggle_row(
                    ui,
                    crate::i18n::tr("Game speaker sound"),
                    crate::i18n::tr("A game's DualSense speaker sound, through the virtual DualSense"),
                    &mut a.game_speaker,
                );
                if a.game_speaker
                    && (d.virt.active() != Some(VirtualKind::DualSense) || !game_speaker)
                {
                    muted(ui, crate::i18n::tr("For game sound, select DualSense on the Virtual controller page, with Game speaker on."));
                }
                if !a.pc_audio && !a.game_speaker {
                    return;
                }
                row(ui, crate::i18n::tr("Output"), crate::i18n::tr("Where the controller plays it"), |ui| {
                    segmented(
                        ui,
                        &mut a.output,
                        &[
                            (Ds4AudioOutput::Speaker, crate::i18n::tr("Speaker")),
                            (Ds4AudioOutput::Headphones, crate::i18n::tr("Headphone jack")),
                        ],
                    );
                });
                slider_row(ui, crate::i18n::tr("Volume"), crate::i18n::tr("0 keeps it off"), &mut a.volume, 0..=100, "%");
                if a.pc_audio {
                    let h = &mut p.haptics;
                    row(
                        ui,
                        crate::i18n::tr("Capture from"),
                        crate::i18n::tr("What the PC plays on this output"),
                        |ui| {
                            let label = if h.device.is_empty() {
                                crate::i18n::tr("Windows default").to_string()
                            } else {
                                h.device.clone()
                            };
                            egui::ComboBox::from_id_salt("ds4-audio-dev")
                                .width(240.0)
                                .selected_text(label)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut h.device, String::new(), crate::i18n::tr("Windows default"));
                                    for dv in &devices {
                                        ui.selectable_value(&mut h.device, dv.clone(), dv);
                                    }
                                });
                        },
                    );
                    if let Some(e) = capture.error.as_ref().filter(|_| !capture.running) {
                        ui.label(RichText::new(e).size(12.0).color(BAD));
                    }
                }
                let streaming = d.stats.stream_hz.load(Ordering::Relaxed);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::tr("Level")).color(MUTED));
                    meter(ui, d.speaker_level.load(Ordering::Relaxed) as f32 / 255.0, VIOLET, 184.0);
                    let (t, c) = if streaming > 0 {
                        (crate::localized_format!("{streaming} pkt/s", "{streaming} pac/s", "{streaming} paq/s"), GOOD)
                    } else {
                        (crate::i18n::tr("off").to_string(), FAINT)
                    };
                    pill(ui, &t, c);
                });
                let under = d.stats.stream_underruns.load(Ordering::Relaxed);
                if under > 0 {
                    muted(
                        ui,
                        &crate::localized_format!("{under} audio gap(s) smoothed over since connect", "{under} falha(s) de áudio suavizada(s) desde a conexão", "{under} corte(s) de audio suavizado(s) desde la conexión"),
                    );
                }
                note(ui, crate::i18n::tr("Not tried on a real DualShock 4 yet. While a source is on, the controller gets audio all the time, which uses more battery."), WARN);
            },
        );
    }

    pub(crate) fn page_haptics(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Profile,
        dev: Option<&Arc<Device>>,
    ) {
        page_header(
            ui,
            crate::i18n::tr("Haptics & audio"),
            crate::i18n::tr("Wireless haptic feedback over Bluetooth: feel game audio, button clicks, and rumble through the voice-coil actuators."),
        );
        let ds4 = dev.map(|d| d.model.is_dualshock4()).unwrap_or(false);
        if ds4 {
            note(
                ui,
                crate::i18n::tr("A DualShock 4 has rumble motors, not the DualSense voice coils. Rumble and button clicks use classic rumble. The haptic stream is DualSense only."),
                WARN,
            );
            ui.add_space(10.0);
            if let Some(d) = dev {
                self.ds4_audio_card(ui, p, d);
                ui.add_space(12.0);
            }
        }
        let bt = dev.map(|d| d.is_bluetooth()).unwrap_or(true) && !ds4;
        if !bt && !ds4 {
            note(
                ui,
                crate::i18n::tr("This controller is on USB. PCM haptics stream over Bluetooth only, so rumble and button clicks use classic rumble here."),
                WARN,
            );
            ui.add_space(10.0);
        }
        let devices = self.audio_device_list();
        let capture = self.engine.audio.status.lock().clone();

        two_columns(ui, |ui, col| {
            if col == 0 {
                card(ui, crate::i18n::tr("Wireless haptics"), "", |ui| {
                    let h = &mut p.haptics;
                    toggle_row(
                        ui,
                        crate::i18n::tr("Stream haptics"),
                        crate::i18n::tr("Over Bluetooth, while there is something to play: system audio, clicks, trigger texture, or game haptics"),
                        &mut h.enabled,
                    );
                    if let Some(d) = dev {
                        let streaming = d.stats.stream_hz.load(Ordering::Relaxed);
                        let lv = [
                            d.haptic_level[0].load(Ordering::Relaxed) as f32 / 255.0,
                            d.haptic_level[1].load(Ordering::Relaxed) as f32 / 255.0,
                        ];
                        ui.horizontal(|ui| {
                            // Room for the two labels and the status pill.
                            let mw = ((ui.available_width() - 170.0) / 2.0).clamp(30.0, 120.0);
                            ui.label(RichText::new(crate::i18n::tr("Left")).color(MUTED));
                            meter(ui, lv[0], ACCENT, mw);
                            ui.add_space(8.0);
                            ui.label(RichText::new(crate::i18n::tr("Right")).color(MUTED));
                            meter(ui, lv[1], ACCENT, mw);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let (t, c) = if !h.enabled || !bt {
                                        (crate::i18n::tr("off").to_string(), FAINT)
                                    } else if streaming > 0 {
                                        (crate::localized_format!("{streaming} pkt/s", "{streaming} pac/s", "{streaming} paq/s"), GOOD)
                                    } else if d.pcm_mode.load(Ordering::Relaxed) {
                                        (crate::i18n::tr("idle").to_string(), MUTED)
                                    } else {
                                        (crate::i18n::tr("standby").to_string(), MUTED)
                                    };
                                    pill(ui, &t, c);
                                },
                            );
                        });
                        let under = d.stats.stream_underruns.load(Ordering::Relaxed);
                        if under > 0 {
                            muted(
                                ui,
                                &crate::localized_format!("{under} audio gap(s) smoothed over since connect", "{under} falha(s) de áudio suavizada(s) desde a conexão", "{under} corte(s) de audio suavizado(s) desde la conexión"),
                            );
                        }
                    }
                    if !h.enabled {
                        return;
                    }
                    ui.add_space(6.0);
                    row(ui, crate::i18n::tr("Haptics source"), crate::i18n::tr("Native game haptics always play; Audio to vibration also turns PC sound into vibration"), |ui| {
                        segmented(
                            ui,
                            &mut h.source,
                            &[
                                (AudioSource::None, crate::i18n::tr("Game / events")),
                                (AudioSource::SystemAudio, crate::i18n::tr("Audio to vibration")),
                            ],
                        );
                    });
                    if h.source == AudioSource::SystemAudio || h.pc_audio_on_speaker {
                        row(
                            ui,
                            crate::i18n::tr("Capture from"),
                            crate::i18n::tr("What the PC plays on this output"),
                            |ui| {
                                let label = if h.device.is_empty() {
                                    crate::i18n::tr("Windows default").to_string()
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
                                            crate::i18n::tr("Windows default"),
                                        );
                                        for d in &devices {
                                            ui.selectable_value(&mut h.device, d.clone(), d);
                                        }
                                    });
                            },
                        );
                        let peak = self.engine.audio.peak();
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(crate::i18n::tr("Input")).color(MUTED));
                            meter(ui, peak, VIOLET, 160.0);
                            if capture.running {
                                ui.label(
                                    RichText::new(crate::localized_format!(
                                        "{} · {} Hz · {} ch", "{} · {} Hz · {} canais", "{} · {} Hz · {} canales",
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
                    }
                    if h.source == AudioSource::SystemAudio {
                        slider_row(ui, crate::i18n::tr("Intensity"), "", &mut h.gain, 0.1..=6.0, "×");
                        slider_row(
                            ui,
                            crate::i18n::tr("Low-pass"),
                            crate::i18n::tr("Higher keeps more detail; lower is punchier"),
                            &mut h.low_pass_hz,
                            100.0..=1400.0,
                            " Hz",
                        );
                        slider_row(
                            ui,
                            crate::i18n::tr("High-pass"),
                            crate::i18n::tr("Removes sub-bass rumble the actuators can't play"),
                            &mut h.high_pass_hz,
                            5.0..=200.0,
                            " Hz",
                        );
                        toggle_row(
                            ui,
                            crate::i18n::tr("Stereo"),
                            crate::i18n::tr("Left audio on the left grip, right on the right"),
                            &mut h.stereo,
                        );
                    }
                    row(
                        ui,
                        crate::i18n::tr("Latency"),
                        crate::i18n::tr("Buffer length: shorter is snappier, longer rides out radio hiccups"),
                        |ui| {
                            segmented(
                                ui,
                                &mut h.latency,
                                &[
                                    (Latency::Short, crate::i18n::tr("Short")),
                                    (Latency::Medium, crate::i18n::tr("Medium")),
                                    (Latency::Long, crate::i18n::tr("Long")),
                                ],
                            );
                        },
                    );
                });
            } else {
                card(
                    ui,
                    crate::i18n::tr("Controller speaker / headset"),
                    crate::i18n::tr("Speaker sound over Bluetooth: a game's own speaker sounds through the virtual DualSense, and PC audio if you want it"),
                    |ui| {
                        let h = &mut p.haptics;
                        slider_row(
                            ui,
                            crate::i18n::tr("Volume"),
                            crate::i18n::tr("0 keeps the speaker off"),
                            &mut h.speaker_volume,
                            0..=100,
                            "%",
                        );
                        let mut pc = h.pc_audio_on_speaker;
                        if toggle_row(
                            ui,
                            crate::i18n::tr("PC audio on speaker / headset"),
                            crate::i18n::tr("Hear PC sound on the controller, independently of the haptics source"),
                            &mut pc,
                        ) {
                            h.pc_audio_on_speaker = pc;
                            if pc {
                                h.enabled = true;
                            }
                        }
                        toggle_row(
                            ui,
                            crate::i18n::tr("Headset when plugged in"),
                            crate::i18n::tr("Sends speaker sound to a headset in the controller's jack instead"),
                            &mut h.headset_when_plugged,
                        );
                        if let Some(d) = dev {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(crate::i18n::tr("Speaker sound")).color(MUTED));
                                meter(
                                    ui,
                                    d.speaker_level.load(Ordering::Relaxed) as f32 / 255.0,
                                    VIOLET,
                                    184.0,
                                );
                            });
                            if bt && h.enabled && h.speaker_volume > 0 {
                                ui.add_space(6.0);
                                let busy = d.game_audio.lock().is_some();
                                if button(ui, crate::i18n::tr("Test speaker")).clicked() && !busy {
                                    d.start_speaker_test();
                                }
                                muted(ui, crate::i18n::tr("Four rising notes, then a steady tone. The controller's microphone listens for clicks."));
                                if let Some(r) = d.speaker_test_result.lock().as_ref() {
                                    muted(ui, r);
                                }
                                if busy && d.mic_tap.lock().is_none() {
                                    muted(ui, crate::i18n::tr("A game is using the virtual DualSense; its speaker sound plays here."));
                                }
                            }
                        }
                        if !bt || !p.haptics.enabled {
                            muted(ui, crate::i18n::tr("Needs Bluetooth and haptics streaming on."));
                        }
                    },
                );
            }
        });
        ui.add_space(12.0);
        card(
            ui,
            crate::i18n::tr("Bluetooth microphone (experimental)"),
            crate::i18n::tr("Controller or headset microphone as a Windows recording device"),
            |ui| {
                let m = &mut p.microphone;
                toggle_row(
                    ui,
                    crate::i18n::tr("Enable microphone"),
                    crate::i18n::tr("Requires Bluetooth and an active virtual DualSense controller"),
                    &mut m.enabled,
                );
                if m.enabled {
                    row(
                        ui,
                        crate::i18n::tr("Microphone source"),
                        crate::i18n::tr("Auto prefers a headset with a microphone in the controller's 3.5 mm jack"),
                        |ui| {
                            segmented(
                                ui,
                                &mut m.source,
                                &[
                                    (MicrophoneSource::Auto, crate::i18n::tr("Auto")),
                                    (MicrophoneSource::BuiltIn, crate::i18n::tr("Built-in")),
                                    (MicrophoneSource::Headset, crate::i18n::tr("Headset")),
                                ],
                            );
                        },
                    );
                    toggle_row(
                        ui,
                        crate::i18n::tr("Mute microphone"),
                        crate::i18n::tr("Send silence to recording apps"),
                        &mut m.muted,
                    );
                    toggle_row(ui, crate::i18n::tr("Mute button"), crate::i18n::tr("The controller's Mute button mutes recording; its LED shows the mute state"), &mut m.mute_button);
                    slider_row(
                        ui,
                        crate::i18n::tr("Microphone gain"),
                        crate::i18n::tr("100% is the original level"),
                        &mut m.gain_percent,
                        0..=200,
                        "%",
                    );
                    if let Some(d) = dev {
                        let ready = bt && d.virt.active() == Some(VirtualKind::DualSense);
                        if !ready {
                            note(ui, crate::i18n::tr("Connect over Bluetooth and select DualSense on the Virtual controller page to expose the microphone to Windows."), WARN);
                        } else {
                            let toggled = d.toggles.lock().mute_engaged;
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(crate::i18n::tr("Microphone level")).color(MUTED));
                                meter(ui, d.microphone.peak() as f32 / 255.0, VIOLET, 184.0);
                                if m.muted || (m.mute_button && toggled) {
                                    pill(ui, crate::i18n::tr("muted"), WARN);
                                } else if !d.virt.microphone_open() {
                                    pill(ui, crate::i18n::tr("no app recording"), FAINT);
                                }
                            });
                            muted(ui, crate::i18n::tr("In Windows, Discord or your game, select the virtual Wireless Controller microphone as the input device. The controller's microphone is on only while an app records from it."));
                        }
                    } else {
                        muted(
                            ui,
                            crate::i18n::tr("Connect a DualSense over Bluetooth to use its microphone."),
                        );
                    }
                }
            },
        );
        ui.add_space(12.0);
        two_columns(ui, |ui, col| {
            if col == 0 {
                card(
                    ui,
                    crate::i18n::tr("Event haptics"),
                    crate::i18n::tr("Generated feedback that does not depend on game audio"),
                    |ui| {
                        let h = &mut p.haptics;
                        toggle_row(
                            ui,
                            crate::i18n::tr("Button clicks"),
                            crate::i18n::tr("A crisp tap under the grip you pressed"),
                            &mut h.buttons.enabled,
                        );
                        if h.buttons.enabled {
                            let mut pct = (h.buttons.intensity * 100.0).round();
                            if slider_row(ui, crate::i18n::tr("Strength"), "", &mut pct, 5.0..=100.0, "%") {
                                h.buttons.intensity = pct / 100.0;
                            }
                            slider_row(
                                ui,
                                crate::i18n::tr("Tone"),
                                "",
                                &mut h.buttons.frequency,
                                60.0..=400.0,
                                " Hz",
                            );
                            slider_row(
                                ui,
                                crate::i18n::tr("Length"),
                                "",
                                &mut h.buttons.duration_ms,
                                15.0..=150.0,
                                " ms",
                            );
                            if let Some(d) = dev {
                                if button(ui, crate::i18n::tr("Try it")).clicked() {
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
                            crate::i18n::tr("Trigger texture"),
                            crate::i18n::tr("A hum that grows as you pull L2 / R2"),
                            &mut h.triggers.enabled,
                        );
                        if h.triggers.enabled {
                            let mut pct = (h.triggers.intensity * 100.0).round();
                            if slider_row(ui, crate::i18n::tr("Strength"), "", &mut pct, 5.0..=100.0, "%") {
                                h.triggers.intensity = pct / 100.0;
                            }
                            slider_row(
                                ui,
                                crate::i18n::tr("Tone"),
                                "",
                                &mut h.triggers.frequency,
                                20.0..=300.0,
                                " Hz",
                            );
                        }
                        if !bt || !h.enabled {
                            ui.add_space(4.0);
                            muted(ui, crate::i18n::tr("Without streaming, button clicks fall back to a short classic rumble."));
                        }
                    },
                );
            } else {
                card(
                    ui,
                    crate::i18n::tr("Rumble"),
                    crate::i18n::tr("Game and test rumble. The controller's own rumble emulation plays it, unless haptics are streaming"),
                    |ui| {
                    let r = &mut p.rumble;
                    // Eight hardware steps of 12.5 %, shown as the power left.
                    let mut pct = 100.0 - 12.5 * r.power_reduction.min(7) as f32;
                    let moved = row(ui, crate::i18n::tr("Power"), crate::i18n::tr("Controller rumble power"), |ui| {
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
                        crate::i18n::tr("Improved rumble"),
                        crate::i18n::tr("Smoother rumble emulation (firmware 2.24 and later)"),
                        &mut r.enhanced,
                    );
                    if bt && p.haptics.enabled {
                        let h = &mut p.haptics;
                        toggle_row(
                            ui,
                            crate::i18n::tr("Rumble as haptics"),
                            crate::i18n::tr("Always play rumble on the haptic stream. Off: only while system audio, clicks, trigger texture, or game haptics are streaming"),
                            &mut h.rumble_as_haptics,
                        );
                        let mut pct = (h.rumble_intensity * 100.0).round();
                        if slider_row(
                            ui,
                            crate::i18n::tr("Haptic rumble strength"),
                            crate::i18n::tr("When rumble plays on the haptic stream"),
                            &mut pct,
                            0.0..=200.0,
                            "%",
                        ) {
                            h.rumble_intensity = pct / 100.0;
                        }
                    }
                    if let Some(d) = dev {
                        ui.horizontal(|ui| {
                            if button(ui, crate::i18n::tr("Heavy")).clicked() {
                                d.test_rumble(255, 0, 450);
                            }
                            if button(ui, crate::i18n::tr("Light")).clicked() {
                                d.test_rumble(0, 255, 450);
                            }
                            if button(ui, crate::i18n::tr("Both")).clicked() {
                                d.test_rumble(200, 200, 450);
                            }
                        });
                    }
                });
            }
        });
    }
}
