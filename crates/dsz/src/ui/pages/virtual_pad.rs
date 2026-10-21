use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::device::Device;
use crate::haptics::game::{TestSignal, TestSource};
use crate::install;
use crate::profile::*;
use crate::ui::theme::*;
use crate::ui::widgets::*;
use crate::ui::App;
use crate::virt;

#[derive(Clone)]
pub struct DriverInfo {
    hidhide: bool,
    usbip: bool,
    usbip_version: Option<[u16; 4]>,
    usbip_blocked: Option<String>,
}

fn deadzone_editor(ui: &mut egui::Ui, id: &str, dz: &mut StickDeadzone) {
    row(ui, crate::i18n::tr("Deadzone"), "", |ui| {
        egui::ComboBox::from_id_salt((id, "shape"))
            .width(130.0)
            .selected_text(match dz.shape {
                DeadzoneShape::None => crate::i18n::tr("None"),
                DeadzoneShape::Radial => crate::i18n::tr("Radial"),
                DeadzoneShape::Axial => crate::i18n::tr("Axial"),
                DeadzoneShape::Curve => crate::i18n::tr("Curve"),
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut dz.shape, DeadzoneShape::None, crate::i18n::tr("None"));
                ui.selectable_value(&mut dz.shape, DeadzoneShape::Radial, crate::i18n::tr("Radial"));
                ui.selectable_value(&mut dz.shape, DeadzoneShape::Axial, crate::i18n::tr("Axial"));
                ui.selectable_value(&mut dz.shape, DeadzoneShape::Curve, crate::i18n::tr("Curve"));
            });
    });
    match dz.shape {
        DeadzoneShape::Radial | DeadzoneShape::Axial => {
            slider_row(ui, crate::i18n::tr("Size"), crate::i18n::tr("Of full deflection"), &mut dz.size, 0..=60, "%");
        }
        DeadzoneShape::Curve => {
            let c = &mut dz.curve;
            slider_row(ui, crate::i18n::tr("Inner zone"), crate::i18n::tr("Reads centered inside this"), &mut c.inner, 0.0..=40.0, "%");
            slider_row(ui, crate::i18n::tr("Outer zone"), crate::i18n::tr("Reads full past this"), &mut c.outer, 50.0..=100.0, "%");
            slider_row(
                ui,
                crate::i18n::tr("Response"),
                crate::i18n::tr("Below 0 is gentler near center, above 0 is quicker"),
                &mut c.response,
                -6.0..=6.0,
                "",
            );
            slider_row(
                ui,
                crate::i18n::tr("Lift"),
                crate::i18n::tr("Output just past the inner zone, for games with their own deadzone"),
                &mut c.lift,
                0.0..=30.0,
                "%",
            );
            toggle_row(
                ui,
                crate::i18n::tr("Square corners"),
                crate::i18n::tr("Reach full X and Y in the corners"),
                &mut c.square_corners,
            );
            slider_row(ui, crate::i18n::tr("Max output"), "", &mut c.max_output, 50.0..=100.0, "%");
        }
        DeadzoneShape::None => {}
    }
}

fn range_row(ui: &mut egui::Ui, label: &str, r: &mut [u8; 2]) {
    row(
        ui,
        label,
        crate::i18n::tr("Below the start reads 0, past the end reads full"),
        |ui| {
            ui.add(egui::DragValue::new(&mut r[0]).range(0..=98).suffix("%"));
            ui.label(RichText::new(crate::i18n::tr("to")).color(MUTED));
            ui.add(egui::DragValue::new(&mut r[1]).range(1..=100).suffix("%"));
            if r[1] <= r[0] {
                r[1] = (r[0] + 1).min(100);
            }
        },
    );
}

/// One driver: state, name, what it's for, and an Install button (with the
/// install's progress) when it's missing.
fn driver_row(
    ui: &mut egui::Ui,
    (state, color): (&str, egui::Color32),
    name: &str,
    what: &str,
    install: Option<install::Package>,
) {
    ui.horizontal(|ui| {
        pill(ui, state, color);
        ui.label(RichText::new(name).strong());
        muted(ui, what);
    });
    let Some(pkg) = install else { return };
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let busy = matches!(
            install::status(pkg),
            Some(
                install::Status::Downloading
                    | install::Status::Verifying
                    | install::Status::Installing
            )
        );
        if !busy && primary_button(ui, &crate::localized_format!("Install {}", "Instalar {}", pkg.name())).clicked() {
            let ctx = ui.ctx().clone();
            install::start(pkg, move || ctx.request_repaint());
        }
        match install::status(pkg) {
            Some(install::Status::Downloading) => {
                ui.spinner();
                muted(ui, crate::i18n::tr("Downloading from GitHub…"));
            }
            Some(install::Status::Verifying) => {
                ui.spinner();
                muted(ui, crate::i18n::tr("Checking the download…"));
            }
            Some(install::Status::Installing) => {
                ui.spinner();
                muted(ui, crate::i18n::tr("Finish the installer that opened (Windows asks for admin first)."));
            }
            Some(install::Status::Finished { ok: true }) => muted(
                ui,
                if pkg == install::Package::HidHide {
                    crate::i18n::tr("Installed. Restart Windows if the installer asked to; this page updates on its own.")
                } else {
                    crate::i18n::tr("Installed. This page updates on its own in a few seconds.")
                },
            ),
            Some(install::Status::Finished { ok: false }) => {
                note(ui, crate::i18n::tr("The installer was closed before it finished."), WARN)
            }
            Some(install::Status::Failed(e)) => note(ui, &e, BAD),
            None => {}
        }
    });
}

impl App {
    /// Feed a test signal through the game-audio path of `d`, unless a game
    /// is already using it or a test is running.
    pub(crate) fn start_game_test(&mut self, d: &Arc<Device>, signal: TestSignal) {
        if self.game_test.is_some() || d.game_audio.lock().is_some() {
            return;
        }
        let len = match signal {
            TestSignal::Actuators => 3,
            TestSignal::Speaker => 4,
        };
        let src = Arc::new(TestSource::new(Duration::from_secs(len), signal));
        *d.game_audio.lock() = Some(src.clone());
        self.game_test = Some((d.id, src));
    }

    /// Detach a finished test signal. Called every frame, whatever page is
    /// showing.
    pub(crate) fn finish_game_test(&mut self) {
        if let Some((id, src)) = &self.game_test {
            if src.finished() {
                if let Some(d) = self.engine.device_list().into_iter().find(|d| d.id == *id) {
                    let mut g = d.game_audio.lock();
                    if g.as_ref()
                        .map(|x| Arc::as_ptr(x) as *const () == Arc::as_ptr(src) as *const ())
                        .unwrap_or(false)
                    {
                        *g = None;
                    }
                }
                self.game_test = None;
            }
        }
    }

    fn driver_info(&mut self) -> DriverInfo {
        let stale = self
            .virt_drivers
            .as_ref()
            .map(|(t, _)| t.elapsed() > Duration::from_secs(3))
            .unwrap_or(true);
        if stale {
            let info = DriverInfo {
                hidhide: virt::hidhide::available(),
                usbip: virt::usbip::available(),
                usbip_version: virt::usbip::driver_version(),
                usbip_blocked: virt::usbip::blocked(),
            };
            let mut info = info;
            // Debug snapshots (DSZ_DEMO_MISSING) show the install buttons.
            if cfg!(debug_assertions) && std::env::var_os("DSZ_DEMO_MISSING").is_some() {
                (info.usbip, info.hidhide, info.usbip_version) = (false, false, None);
            }
            self.virt_drivers = Some((Instant::now(), info));
        }
        self.virt_drivers.as_ref().unwrap().1.clone()
    }

    pub(crate) fn page_virtual(
        &mut self,
        ui: &mut egui::Ui,
        p: &mut Profile,
        dev: Option<&Arc<Device>>,
    ) {
        page_header(
            ui,
            crate::i18n::tr("Virtual controller"),
            crate::i18n::tr("Give games an Xbox 360 or DualSense controller driven by this one, with your mappings applied, while the real controller is hidden from them."),
        );
        let drivers = self.driver_info();

        two_columns(ui, |ui, col| {
            if col == 0 {
                card(ui, crate::i18n::tr("Controller games see"), "", |ui| {
                    let vo = &mut p.virtual_out;
                    segmented(
                        ui,
                        &mut vo.kind,
                        &[
                            (VirtualKind::Off, crate::i18n::tr("Off (native)")),
                            (VirtualKind::Xbox360, "Xbox 360"),
                            (VirtualKind::DualSense, "DualSense"),
                            (VirtualKind::DualShock4, "DualShock 4"),
                        ],
                    );
                    ui.add_space(6.0);
                    if let Some(d) = dev {
                        let st = d.virt.status.lock().clone();
                        match (st.active, &st.error) {
                            (Some(k), _) => {
                                ui.horizontal(|ui| {
                                    pill(ui, &crate::localized_format!("{} connected", "{} conectado(s)", crate::i18n::tr(k.label())), GOOD);
                                    muted(ui, &crate::localized_format!("via {}", "via {}", st.backend));
                                    if let Some(slot) = st.xinput_slot {
                                        muted(ui, &crate::localized_format!("· XInput player {}", "· jogador XInput {}", slot + 1));
                                    }
                                });
                                let n = d.virt.reports.load(Ordering::Relaxed);
                                muted(ui, &crate::localized_format!("{n} reports sent", "{n} relatórios enviados"));
                                if st.game_audio_open {
                                    ui.horizontal(|ui| {
                                        pill(ui, crate::i18n::tr("Game audio open"), VIOLET);
                                        muted(
                                            ui,
                                            crate::i18n::tr("haptics are coming from the game"),
                                        );
                                    });
                                }
                            }
                            (None, Some(e)) => note(ui, e, WARN),
                            (None, None) if vo.kind != VirtualKind::Off => muted(ui, crate::i18n::tr("Starting…")),
                            _ => muted(ui, crate::i18n::tr("Games see the physical controller.")),
                        }
                        if let Some((slot, hidden)) = st.parked {
                            ui.horizontal(|ui| {
                                pill(ui, crate::i18n::tr("Xbox 360 kept"), FAINT);
                                let player = slot
                                    .map(|s| crate::localized_format!("XInput player {}", "Jogador XInput {}", s + 1))
                                    .unwrap_or_else(|| crate::i18n::tr("its XInput slot").into());
                                muted(
                                    ui,
                                    &crate::localized_format!(
                                        "idle, holding {player}{}; presses Xbox buttons", "ocioso, mantendo {player}{}; envia botões Xbox",
                                        if hidden { crate::i18n::tr(", hidden from games") } else { "" }
                                    ),
                                );
                            });
                        }
                        if st.active.is_some() {
                            if st.hidden {
                                ui.horizontal(|ui| {
                                    pill(ui, crate::i18n::tr("Real controller hidden"), ACCENT);
                                    muted(ui, "HidHide");
                                });
                                muted(ui, crate::i18n::tr("Apps that already had the controller open keep it until they restart (Steam, an open game)."));
                            } else if let Some(n) = &st.hide_note {
                                note(ui, n, WARN);
                            }
                        }
                        if let Some(exe) = self.engine.fallback_exe.read().clone() {
                            note(
                                ui,
                                &crate::localized_format!(
                                    "{exe} has no DualSense support, so games see the fallback controller from Profiles & games.", "{exe} não tem suporte a DualSense; será usado o controle alternativo de Perfis e jogos."
                                ),
                                VIOLET,
                            );
                        }
                    } else {
                        let idle = self.engine.pads.list();
                        if idle.is_empty() {
                            muted(ui, crate::i18n::tr("Connect a controller to start the virtual one."));
                        }
                        for pad in idle {
                            ui.horizontal(|ui| {
                                pill(ui, &crate::localized_format!("{} connected", "{} conectado(s)", pad.kind().label()), GOOD);
                                muted(
                                    ui,
                                    &crate::localized_format!(
                                        "via {} · waiting for your controller", "via {} · aguardando seu controle",
                                        pad.target.backend()
                                    ),
                                );
                            });
                        }
                    }
                    ui.add_space(6.0);
                    toggle_row(
                        ui,
                        crate::i18n::tr("Hide the real controller"),
                        crate::i18n::tr("So games and Steam only see the virtual one"),
                        &mut vo.hide_physical,
                    );
                    let mut keep = self.engine.settings.read().persistent_virtual;
                    if toggle_row(
                        ui,
                        crate::i18n::tr("Stay connected while the controller is off"),
                        crate::i18n::tr("Plugged in when the app starts and kept while the controller sleeps or drops out, so games keep the same pad and XInput slot"),
                        &mut keep,
                    ) {
                        self.engine.settings.write().persistent_virtual = keep;
                        self.engine.settings_changed();
                    }
                    let mut keep_xbox = self.engine.settings.read().keep_xbox_pad;
                    if toggle_row(
                        ui,
                        crate::i18n::tr("Keep the Xbox 360 controller as player 1"),
                        crate::i18n::tr("While a DualSense or native profile is active it stays plugged in, idle and hidden from games, presses Xbox buttons, and drives Game Bar while it is open. Off: unplugged, with a helper pad for Xbox buttons"),
                        &mut keep_xbox,
                    ) {
                        self.engine.settings.write().keep_xbox_pad = keep_xbox;
                        self.engine.settings_changed();
                    }
                    let mut ctl = self.engine.settings.read().hidhide_control;
                    if toggle_row(
                        ui,
                        crate::i18n::tr("Use HidHide"),
                        crate::i18n::tr("Off: profiles never hide a controller. Every hide is undone on exit"),
                        &mut ctl,
                    ) {
                        self.engine.settings.write().hidhide_control = ctl;
                        self.engine.settings_changed();
                    }
                });
                ui.add_space(12.0);
                card(
                    ui,
                    crate::i18n::tr("Drivers on this PC"),
                    crate::i18n::tr("Two free, open-source drivers. Install either with one click: the official release is downloaded, verified, and its installer opened."),
                    |ui| {
                        let usbip_name = match drivers.usbip_version {
                            Some(v) => format!("usbip-win2 {}.{}.{}.{}", v[0], v[1], v[2], v[3]),
                            None => "usbip-win2".to_string(),
                        };
                        let usbip_state = if drivers.usbip_blocked.is_some() {
                            ("unsafe", WARN)
                        } else if drivers.usbip {
                            (crate::i18n::tr("ready"), GOOD)
                        } else {
                            (crate::i18n::tr("missing"), FAINT)
                        };
                        driver_row(
                            ui,
                            usbip_state,
                            &usbip_name,
                            crate::i18n::tr("Xbox 360, DualSense, and DualShock 4 controllers"),
                            (!drivers.usbip && drivers.usbip_blocked.is_none())
                                .then_some(install::Package::Usbip),
                        );
                        driver_row(
                            ui,
                            if drivers.hidhide { (crate::i18n::tr("ready"), GOOD) } else { (crate::i18n::tr("missing"), FAINT) },
                            "HidHide",
                            crate::i18n::tr("Hides the real controller, so games see only the virtual one"),
                            (!drivers.hidhide).then_some(install::Package::HidHide),
                        );
                        if let Some(why) = &drivers.usbip_blocked {
                            note(ui, why, WARN);
                        }
                    },
                );
            } else {
                card(
                    ui,
                    crate::i18n::tr("Game feedback"),
                    crate::i18n::tr("What the game sends back to the virtual controller"),
                    |ui| {
                        let vo = &mut p.virtual_out;
                        toggle_row(
                            ui,
                            crate::i18n::tr("Rumble"),
                            crate::i18n::tr("Through the controller's rumble, or on the haptic stream while it runs"),
                            &mut vo.game_rumble,
                        );
                        toggle_row(
                            ui,
                            crate::i18n::tr("Adaptive triggers"),
                            crate::i18n::tr("DualSense mode"),
                            &mut vo.game_triggers,
                        );
                        toggle_row(
                            ui,
                            crate::i18n::tr("Lightbar and LEDs"),
                            crate::i18n::tr("DualSense and DualShock 4 modes"),
                            &mut vo.game_leds,
                        );
                        toggle_row(
                            ui,
                            crate::i18n::tr("Game haptics"),
                            crate::i18n::tr("The game's haptic audio, to the actuators over Bluetooth"),
                            &mut vo.game_haptics,
                        );
                        if vo.game_haptics {
                            slider_row(
                                ui,
                                crate::i18n::tr("Haptics level"),
                                "",
                                &mut vo.game_haptics_level,
                                0..=100,
                                "%",
                            );
                        }
                        toggle_row(
                            ui,
                            crate::i18n::tr("Game speaker"),
                            crate::i18n::tr("The game's controller-speaker sound, to the speaker over Bluetooth"),
                            &mut vo.game_speaker,
                        );
                        if vo.game_speaker {
                            slider_row(
                                ui,
                                crate::i18n::tr("Speaker level"),
                                "",
                                &mut vo.game_speaker_level,
                                0..=100,
                                "%",
                            );
                        }
                        if let Some(d) = dev {
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(crate::i18n::tr("Game speaker sound")).color(MUTED));
                                meter(
                                    ui,
                                    d.speaker_level.load(Ordering::Relaxed) as f32 / 255.0,
                                    VIOLET,
                                    184.0,
                                );
                            });
                            let fb = d.feedback.lock();
                            let g = fb.game.clone();
                            let (h, l) = fb.game_rumble_now();
                            drop(fb);
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(crate::i18n::tr("Game rumble")).color(MUTED));
                                meter(ui, h as f32 / 255.0, ACCENT, 90.0);
                                meter(ui, l as f32 / 255.0, VIOLET, 90.0);
                            });
                            if g.reports > 0 {
                                muted(ui, &crate::localized_format!("{} output report(s) from the game", "{} relatório(s) de saída do jogo", g.reports));
                            }
                            ui.add_space(6.0);
                            let testing = self.game_test.is_some();
                            let label = if testing { crate::i18n::tr("Testing…") } else { crate::i18n::tr("Test game haptics") };
                            if button(ui, label).clicked() {
                                self.start_game_test(d, TestSignal::Actuators);
                            }
                            muted(ui, crate::i18n::tr("Plays a sweep on the left grip, then the right, through the game-haptics path."));
                        }
                    },
                );
                ui.add_space(12.0);
                card(
                    ui,
                    crate::i18n::tr("Sticks and triggers"),
                    crate::i18n::tr("Applied to what the game sees"),
                    |ui| {
                        let vo = &mut p.virtual_out;
                        ui.label(RichText::new(crate::i18n::tr("Left stick")).strong());
                        deadzone_editor(ui, "dz-left", &mut vo.left_deadzone);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut vo.invert[0], crate::i18n::tr("Invert X"));
                            ui.checkbox(&mut vo.invert[1], crate::i18n::tr("Invert Y"));
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new(crate::i18n::tr("Right stick")).strong());
                        deadzone_editor(ui, "dz-right", &mut vo.right_deadzone);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut vo.invert[2], crate::i18n::tr("Invert X"));
                            ui.checkbox(&mut vo.invert[3], crate::i18n::tr("Invert Y"));
                        });
                        ui.add_space(6.0);
                        range_row(ui, crate::i18n::tr("L2 range"), &mut vo.left_trigger_range);
                        range_row(ui, crate::i18n::tr("R2 range"), &mut vo.right_trigger_range);
                        toggle_row(
                        ui,
                        crate::i18n::tr("Touchpad passthrough"),
                        crate::i18n::tr("Keep sending touches to a virtual DualSense while the touchpad drives the mouse"),
                        &mut vo.touch_passthrough,
                    );
                        muted(
                            ui,
                            crate::i18n::tr("Sticks set to mouse, scroll, or keys read centered in the game."),
                        );
                    },
                );
            }
        });
    }
}
