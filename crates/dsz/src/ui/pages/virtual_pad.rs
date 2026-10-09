use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::device::Device;
use crate::haptics::game::TestSource;
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
    row(ui, "Deadzone", "", |ui| {
        egui::ComboBox::from_id_salt((id, "shape"))
            .width(130.0)
            .selected_text(match dz.shape {
                DeadzoneShape::None => "None",
                DeadzoneShape::Radial => "Radial",
                DeadzoneShape::Axial => "Axial",
                DeadzoneShape::Curve => "Curve",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut dz.shape, DeadzoneShape::None, "None");
                ui.selectable_value(&mut dz.shape, DeadzoneShape::Radial, "Radial");
                ui.selectable_value(&mut dz.shape, DeadzoneShape::Axial, "Axial");
                ui.selectable_value(&mut dz.shape, DeadzoneShape::Curve, "Curve");
            });
    });
    match dz.shape {
        DeadzoneShape::Radial | DeadzoneShape::Axial => {
            slider_row(ui, "Size", "Of full deflection", &mut dz.size, 0..=60, "%");
        }
        DeadzoneShape::Curve => {
            let c = &mut dz.curve;
            slider_row(ui, "Inner zone", "Reads centered inside this", &mut c.inner, 0.0..=40.0, "%");
            slider_row(ui, "Outer zone", "Reads full past this", &mut c.outer, 50.0..=100.0, "%");
            slider_row(
                ui,
                "Response",
                "Below 0 is gentler near center, above 0 is quicker",
                &mut c.response,
                -6.0..=6.0,
                "",
            );
            slider_row(
                ui,
                "Lift",
                "Output just past the inner zone, for games with their own deadzone",
                &mut c.lift,
                0.0..=30.0,
                "%",
            );
            toggle_row(
                ui,
                "Square corners",
                "Reach full X and Y in the corners",
                &mut c.square_corners,
            );
            slider_row(ui, "Max output", "", &mut c.max_output, 50.0..=100.0, "%");
        }
        DeadzoneShape::None => {}
    }
}

fn range_row(ui: &mut egui::Ui, label: &str, r: &mut [u8; 2]) {
    row(
        ui,
        label,
        "Below the start reads 0, past the end reads full",
        |ui| {
            ui.add(egui::DragValue::new(&mut r[0]).range(0..=98).suffix("%"));
            ui.label(RichText::new("to").color(MUTED));
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
        if !busy && primary_button(ui, &format!("Install {}", pkg.name())).clicked() {
            let ctx = ui.ctx().clone();
            install::start(pkg, move || ctx.request_repaint());
        }
        match install::status(pkg) {
            Some(install::Status::Downloading) => {
                ui.spinner();
                muted(ui, "Downloading from GitHub…");
            }
            Some(install::Status::Verifying) => {
                ui.spinner();
                muted(ui, "Checking the download…");
            }
            Some(install::Status::Installing) => {
                ui.spinner();
                muted(ui, "Finish the installer that opened (Windows asks for admin first).");
            }
            Some(install::Status::Finished { ok: true }) => muted(
                ui,
                if pkg == install::Package::HidHide {
                    "Installed. Restart Windows if the installer asked to; this page updates on its own."
                } else {
                    "Installed. This page updates on its own in a few seconds."
                },
            ),
            Some(install::Status::Finished { ok: false }) => {
                note(ui, "The installer was closed before it finished.", WARN)
            }
            Some(install::Status::Failed(e)) => note(ui, &e, BAD),
            None => {}
        }
    });
}

impl App {
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
            "Virtual controller",
            "Give games an Xbox 360 or DualSense controller driven by this one, with your mappings applied, while the real controller is hidden from them.",
        );
        let drivers = self.driver_info();

        // Finish a game-haptics test.
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

        two_columns(ui, |ui, col| {
            if col == 0 {
                card(ui, "Controller games see", "", |ui| {
                    let vo = &mut p.virtual_out;
                    segmented(
                        ui,
                        &mut vo.kind,
                        &[
                            (VirtualKind::Off, "Off (native)"),
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
                                    pill(ui, &format!("{} connected", k.label()), GOOD);
                                    muted(ui, &format!("via {}", st.backend));
                                    if let Some(slot) = st.xinput_slot {
                                        muted(ui, &format!("· XInput player {}", slot + 1));
                                    }
                                });
                                let n = d.virt.reports.load(Ordering::Relaxed);
                                muted(ui, &format!("{n} reports sent"));
                                if st.game_audio_open {
                                    ui.horizontal(|ui| {
                                        pill(ui, "Game audio open", VIOLET);
                                        muted(
                                            ui,
                                            "haptics are coming from the game",
                                        );
                                    });
                                }
                            }
                            (None, Some(e)) => note(ui, e, WARN),
                            (None, None) if vo.kind != VirtualKind::Off => muted(ui, "Starting…"),
                            _ => muted(ui, "Games see the physical controller."),
                        }
                        if st.active.is_some() {
                            if st.hidden {
                                ui.horizontal(|ui| {
                                    pill(ui, "Real controller hidden", ACCENT);
                                    muted(ui, "HidHide");
                                });
                                muted(ui, "Apps that already had the controller open keep it until they restart (Steam, an open game).");
                            } else if let Some(n) = &st.hide_note {
                                note(ui, n, WARN);
                            }
                        }
                    } else {
                        let idle = self.engine.pads.list();
                        if idle.is_empty() {
                            muted(ui, "Connect a controller to start the virtual one.");
                        }
                        for pad in idle {
                            ui.horizontal(|ui| {
                                pill(ui, &format!("{} connected", pad.kind().label()), GOOD);
                                muted(
                                    ui,
                                    &format!(
                                        "via {} · waiting for your controller",
                                        pad.target.backend()
                                    ),
                                );
                            });
                        }
                    }
                    ui.add_space(6.0);
                    toggle_row(
                        ui,
                        "Hide the real controller",
                        "So games and Steam only see the virtual one",
                        &mut vo.hide_physical,
                    );
                    let mut keep = self.engine.settings.read().persistent_virtual;
                    if toggle_row(
                        ui,
                        "Stay connected while the controller is off",
                        "Plugged in when the app starts and kept while the controller sleeps or drops out, so games keep the same pad and XInput slot",
                        &mut keep,
                    ) {
                        self.engine.settings.write().persistent_virtual = keep;
                        self.engine.settings_changed();
                    }
                    let mut ctl = self.engine.settings.read().hidhide_control;
                    if toggle_row(
                        ui,
                        "Use HidHide",
                        "Off: profiles never hide a controller. Every hide is undone on exit",
                        &mut ctl,
                    ) {
                        self.engine.settings.write().hidhide_control = ctl;
                        self.engine.settings_changed();
                    }
                });
                ui.add_space(12.0);
                card(
                    ui,
                    "Drivers on this PC",
                    "Two free, open-source drivers. Install either with one click: the official release is downloaded, verified, and its installer opened.",
                    |ui| {
                        let usbip_name = match drivers.usbip_version {
                            Some(v) => format!("usbip-win2 {}.{}.{}.{}", v[0], v[1], v[2], v[3]),
                            None => "usbip-win2".to_string(),
                        };
                        let usbip_state = if drivers.usbip_blocked.is_some() {
                            ("unsafe", WARN)
                        } else if drivers.usbip {
                            ("ready", GOOD)
                        } else {
                            ("missing", FAINT)
                        };
                        driver_row(
                            ui,
                            usbip_state,
                            &usbip_name,
                            "Xbox 360, DualSense, and DualShock 4 controllers",
                            (!drivers.usbip && drivers.usbip_blocked.is_none())
                                .then_some(install::Package::Usbip),
                        );
                        driver_row(
                            ui,
                            if drivers.hidhide { ("ready", GOOD) } else { ("missing", FAINT) },
                            "HidHide",
                            "Hides the real controller, so games see only the virtual one",
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
                    "Game feedback",
                    "What the game sends back to the virtual controller",
                    |ui| {
                        let vo = &mut p.virtual_out;
                        toggle_row(
                            ui,
                            "Rumble",
                            "Through the controller's rumble, or on the haptic stream while it runs",
                            &mut vo.game_rumble,
                        );
                        toggle_row(
                            ui,
                            "Adaptive triggers",
                            "DualSense mode",
                            &mut vo.game_triggers,
                        );
                        toggle_row(
                            ui,
                            "Lightbar and LEDs",
                            "DualSense and DualShock 4 modes",
                            &mut vo.game_leds,
                        );
                        toggle_row(
                            ui,
                            "Game haptics",
                            "The game's haptic audio, to the actuators over Bluetooth",
                            &mut vo.game_haptics,
                        );
                        if vo.game_haptics {
                            slider_row(
                                ui,
                                "Haptics level",
                                "",
                                &mut vo.game_haptics_level,
                                0..=100,
                                "%",
                            );
                        }
                        if let Some(d) = dev {
                            let fb = d.feedback.lock();
                            let g = fb.game.clone();
                            let (h, l) = fb.game_rumble_now();
                            drop(fb);
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Game rumble").color(MUTED));
                                meter(ui, h as f32 / 255.0, ACCENT, 90.0);
                                meter(ui, l as f32 / 255.0, VIOLET, 90.0);
                            });
                            if g.reports > 0 {
                                muted(ui, &format!("{} output report(s) from the game", g.reports));
                            }
                            ui.add_space(6.0);
                            let testing = self.game_test.is_some();
                            ui.horizontal(|ui| {
                            let has_real = d.game_audio.lock().is_some() && !testing;
                            let label = if testing { "Testing…" } else { "Test game haptics" };
                            if button(ui, label).clicked() && !testing && !has_real {
                                let src = Arc::new(TestSource::new(Duration::from_secs(3)));
                                *d.game_audio.lock() = Some(src.clone());
                                self.game_test = Some((d.id, src));
                            }
                            muted(ui, "Plays a sweep on the left grip, then the right, through the game-haptics path.");
                        });
                        }
                    },
                );
                ui.add_space(12.0);
                card(
                    ui,
                    "Sticks and triggers",
                    "Applied to what the game sees",
                    |ui| {
                        let vo = &mut p.virtual_out;
                        ui.label(RichText::new("Left stick").strong());
                        deadzone_editor(ui, "dz-left", &mut vo.left_deadzone);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut vo.invert[0], "Invert X");
                            ui.checkbox(&mut vo.invert[1], "Invert Y");
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new("Right stick").strong());
                        deadzone_editor(ui, "dz-right", &mut vo.right_deadzone);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut vo.invert[2], "Invert X");
                            ui.checkbox(&mut vo.invert[3], "Invert Y");
                        });
                        ui.add_space(6.0);
                        range_row(ui, "L2 range", &mut vo.left_trigger_range);
                        range_row(ui, "R2 range", &mut vo.right_trigger_range);
                        toggle_row(
                        ui,
                        "Touchpad passthrough",
                        "Keep sending touches to a virtual DualSense while the touchpad drives the mouse",
                        &mut vo.touch_passthrough,
                    );
                        muted(
                            ui,
                            "Sticks set to mouse, scroll, or keys read centered in the game.",
                        );
                    },
                );
            }
        });
    }
}
