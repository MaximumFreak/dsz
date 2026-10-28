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
use crate::virt::shape;

#[derive(Clone, Copy)]
struct ReachRimSample {
    direction: f32,
    magnitude: f32,
}

#[derive(Clone)]
struct ReachCalibration {
    started: f64,
    maxima: [Option<f32>; 16],
    turns: u8,
    progress: f32,
    last_direction: Option<f32>,
    rim: [Option<ReachRimSample>; 128],
}

impl ReachCalibration {
    fn new(started: f64) -> Self {
        Self { started, maxima: [None; 16], turns: 0, progress: 0.0, last_direction: None, rim: [None; 128] }
    }

    fn observe(&mut self, x: u8, y: u8) {
        ReachEditor::observe(&mut self.maxima, x, y);
        let (fx, fy) = (shape::axis(x), shape::axis(y));
        let magnitude = fx.hypot(fy);
        if magnitude < 0.5 {
            self.last_direction = None;
            self.progress = 0.0;
            return;
        }
        let direction = shape::direction(fx, fy);
        let bin = (direction * 8.0).round() as usize % self.rim.len();
        if self.rim[bin].is_none_or(|sample| magnitude > sample.magnitude) {
            self.rim[bin] = Some(ReachRimSample { direction, magnitude });
        }
        if let Some(last) = self.last_direction {
            let delta = (direction - last + 8.0).rem_euclid(16.0) - 8.0;
            // Reversals cancel partial turns; rocking the stick must not count as a lap.
            if delta.abs() < 8.0 {
                self.progress += delta;
                if self.progress.abs() >= 16.0 - 0.0001 {
                    self.turns = (self.turns + 1).min(3);
                    self.progress -= self.progress.signum() * 16.0;
                }
            } else {
                self.progress = 0.0;
            }
        }
        self.last_direction = Some(direction);
    }

    fn directional_maxima(&self) -> [Option<f32>; 16] {
        std::array::from_fn(|i| {
            self.maxima[i]?;
            let mut before: Option<(f32, f32)> = None;
            let mut after: Option<(f32, f32)> = None;
            // A broad sector's largest radius may belong to its edge, not its centre.
            // Fit the centre between nearby rim samples before blending the 16 gains.
            for sample in self.rim.iter().flatten() {
                let delta = (sample.direction - i as f32 + 8.0).rem_euclid(16.0) - 8.0;
                if delta <= 0.0 && before.is_none_or(|(d, _)| delta > d) {
                    before = Some((delta, sample.magnitude));
                }
                if delta >= 0.0 && after.is_none_or(|(d, _)| delta < d) {
                    after = Some((delta, sample.magnitude));
                }
            }
            match (before, after) {
                (Some((a, ra)), Some((b, rb))) if a >= -1.0 && b <= 1.0 => {
                    let t = if b - a > 0.00001 { -a / (b - a) } else { 0.0 };
                    Some(ra + (rb - ra) * t)
                }
                _ => None,
            }
        })
    }
}

#[derive(Clone, Default)]
struct ReachEditor {
    selected: usize,
    measured: [Option<f32>; 16],
    calibration: Option<ReachCalibration>,
    pending: Option<[Option<f32>; 16]>,
    last_seen: Option<f64>,
}

impl ReachEditor {
    fn observe(maxima: &mut [Option<f32>; 16], x: u8, y: u8) {
        let (fx, fy) = (shape::axis(x), shape::axis(y));
        let magnitude = fx.hypot(fy);
        // Near-center angles are noise, not measurements of the rim.
        if magnitude < 0.5 { return; }
        let sector = shape::direction(fx, fy).round() as usize % 16;
        maxima[sector] = Some(maxima[sector].unwrap_or(0.0).max(magnitude));
    }

    fn tick(&mut self, input: Option<(u8, u8)>, now: f64) {
        // Do not finish a capture using stale samples after leaving the page.
        if self.last_seen.is_some_and(|last| now - last > 0.5) || input.is_none() {
            self.calibration = None;
        }
        self.last_seen = Some(now);
        if let Some((x, y)) = input {
            Self::observe(&mut self.measured, x, y);
            if let Some(c) = &mut self.calibration {
                c.observe(x, y);
                if c.turns >= 3 {
                    let maxima = c.directional_maxima();
                    if now - c.started >= 5.0 || maxima.iter().all(Option::is_some) {
                        self.pending = Some(maxima);
                        self.calibration = None;
                    }
                }
            }
        }
    }

    fn proposal(&self, current: &StickReach) -> StickReach {
        let mut proposed = current.clone();
        if let Some(maxima) = self.pending {
            for (gain, maximum) in proposed.gains.iter_mut().zip(maxima) {
                if let Some(m) = maximum {
                    *gain = (100.0 / m).clamp(50.0, 150.0);
                }
            }
            proposed.enabled = true;
        }
        proposed
    }

    fn reset_all(&mut self, reach: &mut StickReach) {
        reach.gains = [100.0; 16];
        self.measured = [None; 16];
        self.calibration = None;
        self.pending = None;
    }
}

fn reach_preview(ui: &mut egui::Ui, state: &mut ReachEditor, reach: &mut StickReach, x: u8, y: u8, edit: bool) {
    let size = ui.available_width().min(224.0);
    let (r, _) = ui.allocate_exact_size(egui::vec2(size, size), Sense::hover());
    let c = r.center();
    let unit = (size - 20.0) / 3.0;
    let vector = |sector: f32| {
        let angle = sector * std::f32::consts::TAU / 16.0;
        egui::vec2(angle.sin(), -angle.cos())
    };
    let p = ui.painter().clone();
    p.circle_filled(c, unit, FIELD);
    p.circle_stroke(c, unit, Stroke::new(1.0_f32, BORDER));
    for v in [egui::vec2(unit, 0.0), egui::vec2(0.0, unit)] {
        p.line_segment([c - v, c + v], Stroke::new(1.0_f32, with_alpha(FAINT, 80)));
    }
    for i in 0..16 {
        let v = vector(i as f32);
        let dot = c + v * unit * (reach.gains[i] / 100.0).clamp(0.5, 1.5);
        let hit = egui::Rect::from_center_size(dot, egui::vec2(16.0, 16.0));
        let sense = if edit { Sense::click_and_drag() } else { Sense::click() };
        let response = ui.interact(hit, ui.id().with(("reach-point", i)), sense);
        if response.clicked() || response.drag_started() { state.selected = i; }
        if edit && response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                reach.gains[i] = (((pointer - c).dot(v) / unit * 100.0) * 2.0).round().clamp(100.0, 300.0) / 2.0;
            }
        }
        let dot = c + v * unit * (reach.gains[i] / 100.0).clamp(0.5, 1.5);
        p.line_segment([c + v * unit * 0.5, c + v * unit * 1.5], Stroke::new(1.0_f32, with_alpha(BORDER, 120)));
        p.circle_filled(dot, if state.selected == i { 5.0 } else { 3.5 }, if state.selected == i { ACCENT } else { MUTED });
    }
    let maxima = state.pending.as_ref()
        .or_else(|| state.calibration.as_ref().map(|c| &c.maxima))
        .unwrap_or(&state.measured);
    let point = |sector: f32, mag: f32, corrected: bool| {
        let gain = if corrected { shape::reach_gain(reach, sector) } else { 1.0 };
        let v = vector(sector) * mag * gain;
        c + egui::vec2(v.x.clamp(-1.0, 1.0), v.y.clamp(-1.0, 1.0)) * unit
    };
    for i in 0..16 {
        if let Some(mag) = maxima[i] {
            p.circle_filled(point(i as f32, mag, false), 2.5, MUTED);
        }
        if let (Some(a), Some(b)) = (maxima[i], maxima[(i + 1) % 16]) {
            for corrected in [false, true] {
                let points = (0..=8).map(|step| {
                    let t = step as f32 / 8.0;
                    let smooth = (1.0 - (t * std::f32::consts::PI).cos()) * 0.5;
                    point(i as f32 + t, a + (b - a) * smooth, corrected)
                }).collect();
                p.add(egui::Shape::line(points, Stroke::new(1.5_f32, if corrected { ACCENT } else { MUTED })));
            }
        }
    }
    let raw = egui::vec2(shape::axis(x), shape::axis(y));
    let (cx, cy) = shape::reach(reach, x, y);
    let corrected = egui::vec2(shape::axis(cx), shape::axis(cy));
    let dot = c + corrected * unit;
    p.line_segment([c, dot], Stroke::new(1.0_f32, with_alpha(ACCENT, 120)));
    p.circle_filled(c + raw * unit, 6.0, with_alpha(MUTED, 120));
    p.circle_filled(dot, 4.0, ACCENT);
    let raw_percent = raw.length() * 100.0;
    let corrected_percent = corrected.length() * 100.0;
    muted(ui, &crate::localized_format!("Raw {raw_percent:.0}% → corrected {corrected_percent:.0}%", "Bruto {raw_percent:.0}% → corrigido {corrected_percent:.0}%", "Original {raw_percent:.0}% → corregido {corrected_percent:.0}%"));
}

fn reach_editor(ui: &mut egui::Ui, reach: &mut StickReach, state: &mut ReachEditor, input: Option<(u8, u8)>, virtual_on: bool) {
    let now = ui.input(|i| i.time);
    state.tick(input, now);
    ui.add_space(8.0);
    ui.label(RichText::new(crate::i18n::tr("Directional reach")).font(heading(14.0)));
    muted(ui, crate::i18n::tr("Applies only to the virtual controller. Follows axis inversion."));
    if !virtual_on {
        note(ui, crate::i18n::tr("Choose a virtual controller in this profile to use directional reach."), WARN);
    }
    let idle = state.calibration.is_none() && state.pending.is_none();
    ui.add_enabled_ui(idle, |ui| {
        toggle_row(ui, crate::i18n::tr("Enabled"), "", &mut reach.enabled);
    });
    if state.pending.is_some() {
        pill(ui, crate::i18n::tr("Calibration preview"), ACCENT);
    }
    let mut shown = state.proposal(reach);
    let (x, y) = input.unwrap_or((128, 128));
    ui.vertical_centered(|ui| reach_preview(ui, state, &mut shown, x, y, idle));
    row(ui, crate::i18n::tr("Direction"), crate::i18n::tr("0° is up; clockwise"), |ui| {
        egui::ComboBox::from_id_salt("reach-direction")
            .width(85.0)
            .selected_text(format!("{:.1}°", state.selected as f32 * 22.5))
            .show_ui(ui, |ui| {
                for i in 0..16 {
                    ui.selectable_value(&mut state.selected, i, format!("{:.1}°", i as f32 * 22.5));
                }
            });
    });
    ui.add_enabled_ui(idle, |ui| {
        row(ui, crate::i18n::tr("Gain"), crate::i18n::tr("Drag a point to adjust its direction"), |ui| {
            ui.add(egui::DragValue::new(&mut shown.gains[state.selected]).range(50.0..=150.0).speed(0.5).fixed_decimals(1).suffix("%"));
        });
        ui.horizontal_wrapped(|ui| {
            if button(ui, crate::i18n::tr("Reset direction")).clicked() {
                shown.gains[state.selected] = 100.0;
            }
            if button(ui, crate::i18n::tr("Reset all")).clicked() { state.reset_all(&mut shown); }
        });
    });
    if idle { *reach = shown; }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(idle && input.is_some(), |ui| {
            if button(ui, crate::i18n::tr("Calibrate reach")).clicked() {
                state.calibration = Some(ReachCalibration::new(now));
            }
        });
        if state.pending.is_some() {
            let any = state.pending.as_ref().is_some_and(|m| m.iter().any(Option::is_some));
            ui.add_enabled_ui(any, |ui| {
                if primary_button(ui, crate::i18n::tr("Apply")).clicked() {
                    *reach = state.proposal(reach);
                    state.pending = None;
                }
            });
        }
        if (state.calibration.is_some() || state.pending.is_some()) && button(ui, crate::i18n::tr("Discard")).clicked() {
            state.calibration = None;
            state.pending = None;
        }
    });
    if let Some(c) = &state.calibration {
        let count = c.maxima.iter().filter(|m| m.is_some()).count();
        let turns = c.turns;
        note(ui, &crate::localized_format!("Rotate fully: {count}/16 directions · {turns}/3 turns", "Gire até o limite: {count}/16 direções · {turns}/3 voltas", "Gira hasta el tope: {count}/16 direcciones · {turns}/3 vueltas"), ACCENT);
    } else if let Some(maxima) = &state.pending {
        let missing = maxima.iter().filter(|m| m.is_none()).count();
        if missing > 0 {
            note(ui, &crate::localized_format!("{missing} directions unmeasured. Apply keeps their current gains.", "{missing} direções sem amostras. Aplicar mantém os ganhos atuais delas.", "{missing} direcciones sin medir. Aplicar conserva sus ganancias actuales."), WARN);
        } else {
            muted(ui, crate::i18n::tr("All directions measured. Apply or discard the preview."));
        }
    } else if input.is_none() {
        muted(ui, crate::i18n::tr("Connect a controller to calibrate."));
    } else if state.measured.iter().all(Option::is_none) {
        muted(ui, crate::i18n::tr("Calibrate to measure the stick's reach."));
    }
}

/// The touchpad third that "While touching" uses. Anything else falls back to the left third.
fn touch_zone(b: Button) -> Button {
    match b {
        Button::TouchpadCenter | Button::TouchpadRight => b,
        _ => Button::TouchpadLeft,
    }
}

fn touch_zone_combo(ui: &mut egui::Ui, id: &str, b: &mut Button) {
    egui::ComboBox::from_id_salt(id)
        .width(150.0)
        .selected_text(crate::i18n::tr(b.short_name()))
        .show_ui(ui, |ui| {
            for c in [Button::TouchpadLeft, Button::TouchpadCenter, Button::TouchpadRight] {
                ui.selectable_value(b, c, crate::i18n::tr(c.short_name()));
            }
        });
}

fn button_combo(ui: &mut egui::Ui, id: &str, b: &mut Button, edge: bool) {
    egui::ComboBox::from_id_salt(id)
        .width(150.0)
        .selected_text(crate::i18n::tr(b.short_name()))
        .show_ui(ui, |ui| {
            for c in button_choices(edge) {
                ui.selectable_value(b, c, crate::i18n::tr(c.short_name()));
            }
        });
}

fn gyro_bars(ui: &mut egui::Ui, dps: [f32; 3]) {
    for (label, v) in [(crate::i18n::tr("Pitch"), dps[0]), (crate::i18n::tr("Yaw"), dps[1]), (crate::i18n::tr("Roll"), dps[2])] {
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

#[cfg(test)]
mod reach_tests {
    use super::*;

    fn rim_bytes(direction: f32, magnitude: f32) -> (u8, u8) {
        let angle = direction * std::f32::consts::TAU / 16.0;
        ((127.5 + angle.sin() * magnitude * 127.5).round() as u8,
            (127.5 - angle.cos() * magnitude * 127.5).round() as u8)
    }

    #[test]
    fn calibration_fits_the_direction_instead_of_the_largest_sector_edge() {
        let mut c = ReachCalibration::new(0.0);
        for i in 0..=9 {
            let direction = 1.55 + i as f32 * 0.1;
            let magnitude = 1.13 + 0.13 * (direction - 2.0);
            let (x, y) = rim_bytes(direction, magnitude);
            c.observe(x, y);
        }
        let (x, y) = rim_bytes(2.0, 1.13);
        let raw = shape::axis(x).hypot(shape::axis(y));
        assert!(raw / c.maxima[2].unwrap() < 0.97, "the broad maximum reproduces the short output");
        let mut state = ReachEditor::default();
        state.pending = Some(c.directional_maxima());
        let proposed = state.proposal(&StickReach::default());
        let (cx, cy) = shape::reach(&proposed, x, y);
        let corrected = shape::axis(cx).hypot(shape::axis(cy));
        assert!((0.99..=1.01).contains(&corrected), "{corrected}");
        assert!(state.pending.unwrap()[8].is_none(), "unsampled directions are not fitted");
    }

    #[test]
    fn calibration_fits_across_up_and_keeps_the_largest_repeated_sample() {
        let mut c = ReachCalibration::new(0.0);
        for (direction, magnitude) in [(15.9, 0.94), (0.1, 0.96), (0.1, 0.7)] {
            let (x, y) = rim_bytes(direction, magnitude);
            c.observe(x, y);
        }
        assert!((c.directional_maxima()[0].unwrap() - 0.95).abs() < 0.01);
        assert!(c.directional_maxima()[8].is_none());
    }

    #[test]
    fn dragging_a_reach_point_selects_and_limits_its_gain() {
        let ctx = egui::Context::default();
        let mut state = ReachEditor::default();
        let mut reach = StickReach::default();
        let mut center = egui::Pos2::ZERO;
        let mut frame = |time: f64, events: Vec<egui::Event>| {
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 400.0))),
                time: Some(time), events, ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    center = ui.cursor().min + egui::vec2(112.0, 112.0);
                    reach_preview(ui, &mut state, &mut reach, 128, 128, true);
                });
            });
            (center, reach.gains[8])
        };
        let (c, _) = frame(0.0, vec![]);
        let unit = (224.0 - 20.0) / 3.0;
        let down = c + egui::vec2(0.0, unit);
        let press = |pos, pressed| egui::Event::PointerButton {
            pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
        };
        frame(0.02, vec![egui::Event::PointerMoved(down), press(down, true)]);
        let beyond = c + egui::vec2(0.0, unit * 2.0);
        frame(0.04, vec![egui::Event::PointerMoved(beyond)]);
        assert_eq!(frame(0.06, vec![press(beyond, false)]).1, 150.0);
        let high = c + egui::vec2(0.0, unit * 1.5);
        frame(0.08, vec![egui::Event::PointerMoved(high), press(high, true)]);
        frame(0.10, vec![egui::Event::PointerMoved(c)]);
        frame(0.12, vec![press(c, false)]);
        assert_eq!(state.selected, 8);
        assert_eq!(reach.gains[8], 50.0);
        for i in 0..16 {
            if i != 8 { assert_eq!(reach.gains[i], 100.0); }
        }
    }

    #[test]
    fn calibration_ignores_center_noise_and_keeps_sector_maxima() {
        let mut maxima = [None; 16];
        ReachEditor::observe(&mut maxima, 128, 128);
        assert!(maxima.iter().all(Option::is_none));
        for y in [220, 237, 225] { ReachEditor::observe(&mut maxima, 128, y); }
        assert_eq!(maxima.iter().filter(|m| m.is_some()).count(), 1);
        assert_eq!(maxima[8], Some(shape::axis(128).hypot(shape::axis(237))));
        ReachEditor::observe(&mut maxima, 127, 0);
        ReachEditor::observe(&mut maxima, 128, 0);
        assert!(maxima[0].is_some());
        assert!(maxima[15].is_none());
    }

    #[test]
    fn calibration_preserves_unmeasured_gains() {
        let mut state = ReachEditor::default();
        let mut current = StickReach { enabled: false, gains: [95.0; 16] };
        let mut maxima = [None; 16];
        ReachEditor::observe(&mut maxima, 128, 237);
        state.pending = Some(maxima);
        let proposal = state.proposal(&current);
        assert!(proposal.enabled);
        assert!((proposal.gains[8] - 116.44).abs() < 0.1);
        for i in 0..16 {
            if i != 8 { assert_eq!(proposal.gains[i], 95.0); }
        }
        assert!(!current.enabled);
        assert_eq!(current.gains, [95.0; 16], "preview does not change the profile");
        current = proposal;
        state.pending = None;
        assert_eq!(state.proposal(&current), current);
    }

    #[test]
    fn calibration_requires_three_complete_turns_in_either_direction() {
        for sign in [-1.0, 1.0] {
            let mut state = ReachEditor::default();
            state.calibration = Some(ReachCalibration::new(0.0));
            for i in 0..=48 {
                let angle = sign * i as f32 * std::f32::consts::TAU / 16.0;
                let x = (127.5 + angle.sin() * 127.5).round() as u8;
                let y = (127.5 - angle.cos() * 127.5).round() as u8;
                state.tick(Some((x, y)), i as f64 / 10.0);
                if i < 48 {
                    assert!(state.calibration.is_some(), "finished at {i}/48 sectors");
                    assert!(state.pending.is_none());
                }
            }
            assert!(state.calibration.is_none());
            assert!(state.pending.unwrap().iter().all(Option::is_some));
            state.pending = Some([Some(0.5); 16]);
            assert_eq!(state.proposal(&StickReach::default()).gains, [150.0; 16]);
        }
    }

    #[test]
    fn calibration_does_not_count_rocking_or_timeout_as_turns() {
        let mut state = ReachEditor::default();
        state.calibration = Some(ReachCalibration::new(0.0));
        let sectors = [0, 1, 2, 3, 4, 3, 2, 1, 0, 15, 14, 13, 12, 13, 14, 15, 0];
        for tick in 0..200 {
            let angle = sectors[tick % sectors.len()] as f32 * std::f32::consts::TAU / 16.0;
            let x = (127.5 + angle.sin() * 127.5).round() as u8;
            let y = (127.5 - angle.cos() * 127.5).round() as u8;
            state.tick(Some((x, y)), tick as f64 / 10.0);
        }
        assert_eq!(state.calibration.as_ref().unwrap().turns, 0);
        assert!(state.pending.is_none());
    }

    #[test]
    fn reset_all_clears_gains_and_every_calibration_trace() {
        let mut state = ReachEditor::default();
        state.measured = [Some(0.86); 16];
        state.pending = Some([Some(0.86); 16]);
        state.calibration = Some(ReachCalibration::new(0.0));
        let mut reach = StickReach { enabled: true, gains: [116.0; 16] };
        state.reset_all(&mut reach);
        assert_eq!(reach.gains, [100.0; 16]);
        assert!(reach.enabled);
        assert!(state.measured.iter().all(Option::is_none));
        assert!(state.calibration.is_none());
        assert!(state.pending.is_none());
    }

    #[test]
    fn leaving_the_page_or_losing_the_device_cancels_capture() {
        for input in [None, Some((128, 237))] {
            let mut state = ReachEditor::default();
            state.last_seen = Some(0.0);
            state.calibration = Some(ReachCalibration::new(0.0));
            state.tick(input, 6.0);
            assert!(state.calibration.is_none());
            assert!(state.pending.is_none());
        }
    }
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
            crate::i18n::tr("Motion & touch"),
            crate::i18n::tr("Aim with the gyro, drive the cursor with the touchpad, or turn a stick into a mouse, scroll wheel, or keys."),
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
                    crate::i18n::tr("Gyro mouse"),
                    crate::i18n::tr("Tilt and turn the controller to move the cursor or aim"),
                    |ui| {
                        let virtual_on = p.virtual_out.kind != VirtualKind::Off;
                        let g = &mut p.gyro;
                        let mut on = g.mode != GyroMode::Off;
                        if toggle_row(ui, crate::i18n::tr("Enabled"), "", &mut on) {
                            g.mode = if on { GyroMode::Mouse } else { GyroMode::Off };
                        }
                        if g.mode != GyroMode::Off {
                            row(ui, crate::i18n::tr("Output"), "", |ui| {
                                segmented(
                                    ui,
                                    &mut g.mode,
                                    &[
                                        (GyroMode::Mouse, crate::i18n::tr("Mouse")),
                                        (GyroMode::RightStick, crate::i18n::tr("Right stick")),
                                    ],
                                );
                            });
                            if g.mode == GyroMode::RightStick && !virtual_on {
                                note(ui, crate::i18n::tr("Choose a virtual controller in this profile to aim with the right stick."), WARN);
                            }
                            let (mut kind, mut b) = match g.activation {
                                GyroActivation::Always => (0, Button::R1),
                                GyroActivation::WhileHeld(b) => (1, b),
                                GyroActivation::WhileNotHeld(b) => (2, b),
                                GyroActivation::Toggle(b) => (3, b),
                                GyroActivation::WhileTouched(b) => (4, b),
                            };
                            row(ui, crate::i18n::tr("Active"), "", |ui| {
                                if kind == 4 {
                                    b = touch_zone(b);
                                    touch_zone_combo(ui, "gyro-zone", &mut b);
                                } else if kind != 0 {
                                    button_combo(ui, "gyro-btn", &mut b, edge);
                                }
                                egui::ComboBox::from_id_salt("gyro-act")
                                    .width(150.0)
                                    .selected_text(
                                        [
                                            crate::i18n::tr("Always"),
                                            crate::i18n::tr("While holding"),
                                            crate::i18n::tr("Unless holding"),
                                            crate::i18n::tr("Toggle with"),
                                            crate::i18n::tr("While touching"),
                                        ][kind],
                                    )
                                    .show_ui(ui, |ui| {
                                        for (i, l) in [
                                            crate::i18n::tr("Always"),
                                            crate::i18n::tr("While holding"),
                                            crate::i18n::tr("Unless holding"),
                                            crate::i18n::tr("Toggle with"),
                                            crate::i18n::tr("While touching"),
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
                                3 => GyroActivation::Toggle(b),
                                _ => GyroActivation::WhileTouched(touch_zone(b)),
                            };
                            if g.mode == GyroMode::RightStick {
                                slider_row(
                                    ui,
                                    crate::i18n::tr("Full tilt at"),
                                    crate::i18n::tr("Turning speed that pushes the stick all the way"),
                                    &mut g.stick_speed,
                                    20.0..=600.0,
                                    " °/s",
                                );
                                let mut min = (g.stick_min * 100.0).round();
                                if slider_row(
                                    ui,
                                    crate::i18n::tr("Game deadzone"),
                                    crate::i18n::tr("Starts the stick past the game's deadzone"),
                                    &mut min,
                                    0.0..=50.0,
                                    "%",
                                ) {
                                    g.stick_min = min / 100.0;
                                }
                            } else {
                                slider_row(
                                    ui,
                                    crate::i18n::tr("Sensitivity"),
                                    crate::i18n::tr("Pixels per degree"),
                                    &mut g.sensitivity,
                                    1.0..=80.0,
                                    "",
                                );
                            }
                            slider_row(
                                ui,
                                crate::i18n::tr("Vertical ratio"),
                                "",
                                &mut g.vertical_ratio,
                                0.2..=2.0,
                                "×",
                            );
                            row(ui, crate::i18n::tr("Horizontal from"), "", |ui| {
                                segmented(
                                    ui,
                                    &mut g.axis,
                                    &[
                                        (GyroAxis::Yaw, crate::i18n::tr("Turn")),
                                        (GyroAxis::Roll, crate::i18n::tr("Lean")),
                                        (GyroAxis::Combined, crate::i18n::tr("Both")),
                                    ],
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(crate::i18n::tr("Invert")).color(TEXT));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.checkbox(&mut g.invert_y, crate::i18n::tr("Vertical"));
                                        ui.checkbox(&mut g.invert_x, crate::i18n::tr("Horizontal"));
                                    },
                                );
                            });
                            slider_row(
                                ui,
                                crate::i18n::tr("Steadiness"),
                                crate::i18n::tr("Softens tiny hand tremor"),
                                &mut g.deadzone,
                                0.0..=8.0,
                                " °/s",
                            );
                            let mut sm = (g.smoothing * 100.0).round();
                            if slider_row(
                                ui,
                                crate::i18n::tr("Smoothing"),
                                crate::i18n::tr("Only slow motion is smoothed"),
                                &mut sm,
                                0.0..=100.0,
                                "%",
                            ) {
                                g.smoothing = sm / 100.0;
                            }
                            let mut ac = (g.acceleration * 100.0).round();
                            if slider_row(
                                ui,
                                crate::i18n::tr("Acceleration"),
                                crate::i18n::tr("Extra speed for fast flicks"),
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
                                (crate::i18n::tr("Calibrating — keep still"), WARN)
                            } else if live.gyro_stick {
                                (crate::i18n::tr("Moving the right stick"), ACCENT)
                            } else if live.gyro_active {
                                (crate::i18n::tr("Moving the cursor"), ACCENT)
                            } else {
                                (crate::i18n::tr("Live gyro"), MUTED)
                            };
                            pill(ui, t, c);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Some(d) = dev {
                                        if button(ui, crate::i18n::tr("Calibrate"))
                                            .on_hover_text(
                                                crate::i18n::tr("Lay the controller flat and still for 2 seconds"),
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
                        muted(ui, crate::i18n::tr("Calibration also happens on its own whenever the controller rests still for a moment."));
                    },
                );
            } else {
                card(ui, crate::i18n::tr("Touchpad"), "", |ui| {
                    let t = &mut p.touchpad;
                    let mut on = t.mode == TouchMode::Mouse;
                    if toggle_row(
                        ui,
                        crate::i18n::tr("Touchpad as mouse"),
                        crate::i18n::tr("Off leaves the touchpad to the game"),
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
                        slider_row(ui, crate::i18n::tr("Speed"), "", &mut t.sensitivity, 0.1..=4.0, "×");
                        slider_row(ui, crate::i18n::tr("Acceleration"), crate::i18n::tr("Faster swipes move further; 0 is steady"), &mut t.acceleration, 0.0..=2.0, "");
                        toggle_row(
                            ui,
                            crate::i18n::tr("Tap to click"),
                            crate::i18n::tr("One finger is a left click, two a right click"),
                            &mut t.tap_to_click,
                        );
                        toggle_row(
                            ui,
                            crate::i18n::tr("Press to click"),
                            crate::i18n::tr("Press with two fingers for a right click; hold the press and slide to drag"),
                            &mut t.click_to_click,
                        );
                        if t.click_to_click {
                            toggle_row(
                                ui,
                                crate::i18n::tr("Right side is right click"),
                                crate::i18n::tr("Off: a press anywhere is a left click"),
                                &mut t.right_half_right_click,
                            );
                        }
                        toggle_row(ui, crate::i18n::tr("Two-finger scroll"), "", &mut t.two_finger_scroll);
                        if t.two_finger_scroll {
                            slider_row(ui, crate::i18n::tr("Scroll speed"), "", &mut t.scroll_speed, 0.1..=4.0, "×");
                            toggle_row(ui, crate::i18n::tr("Natural scrolling"), crate::i18n::tr("The page follows your fingers"), &mut t.natural_scroll);
                        }
                    }
                });
                ui.add_space(12.0);
                card(
                    ui,
                    crate::i18n::tr("Sticks"),
                    crate::i18n::tr("Without a virtual controller the game still sees the stick; with one, a stick set to mouse, scroll, or keys reads centered"),
                    |ui| {
                        for (left, label) in [(true, crate::i18n::tr("Left stick")), (false, crate::i18n::tr("Right stick"))] {
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
                                            StickMode::Passthrough => crate::i18n::tr("Game only"),
                                            StickMode::Mouse => crate::i18n::tr("Mouse"),
                                            StickMode::Scroll => crate::i18n::tr("Scroll wheel"),
                                            StickMode::Wasd => "W A S D",
                                            StickMode::Arrows => crate::i18n::tr("Arrow keys"),
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Passthrough,
                                                crate::i18n::tr("Game only"),
                                            );
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Mouse,
                                                crate::i18n::tr("Mouse"),
                                            );
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Scroll,
                                                crate::i18n::tr("Scroll wheel"),
                                            );
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Wasd,
                                                "W A S D",
                                            );
                                            ui.selectable_value(
                                                &mut s.mode,
                                                StickMode::Arrows,
                                                crate::i18n::tr("Arrow keys"),
                                            );
                                        });
                                });
                            });
                            if s.mode == StickMode::Passthrough {
                                let vo = &mut p.virtual_out;
                                let invert = if left { [vo.invert[0], vo.invert[1]] } else { [vo.invert[2], vo.invert[3]] };
                                let input = dev.map(|_| (if invert[0] { 255 - x } else { x }, if invert[1] { 255 - y } else { y }));
                                ui.push_id(("reach", left), |ui| {
                                    let id = ui.make_persistent_id((&p.name, dev.map(|d| d.id), invert));
                                    let mut state = ui.data_mut(|d| d.get_temp::<ReachEditor>(id)).unwrap_or_default();
                                    let dz = if left { &mut vo.left_deadzone } else { &mut vo.right_deadzone };
                                    reach_editor(ui, &mut dz.reach, &mut state, input, vo.kind != VirtualKind::Off);
                                    ui.data_mut(|d| d.insert_temp(id, state));
                                });
                            } else {
                                let mut dz = (s.deadzone * 100.0).round();
                                if slider_row(ui, crate::i18n::tr("Deadzone"), "", &mut dz, 0.0..=60.0, "%") {
                                    s.deadzone = dz / 100.0;
                                }
                                match s.mode {
                                    StickMode::Mouse => {
                                        slider_row(
                                            ui,
                                            crate::i18n::tr("Speed"),
                                            "",
                                            &mut s.mouse_speed,
                                            200.0..=4000.0,
                                            " px/s",
                                        );
                                        slider_row(
                                            ui,
                                            crate::i18n::tr("Curve"),
                                            crate::i18n::tr("Higher is finer near the center"),
                                            &mut s.curve,
                                            1.0..=3.5,
                                            "",
                                        );
                                    }
                                    StickMode::Scroll => {
                                        slider_row(
                                            ui,
                                            crate::i18n::tr("Scroll speed"),
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
                                            crate::i18n::tr("Press point"),
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
