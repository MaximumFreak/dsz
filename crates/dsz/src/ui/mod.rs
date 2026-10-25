//! The window: sidebar navigation, top bar, pages, toasts.
//!
//! Each frame clones the profile being edited, lets the page mutate the
//! clone, and publishes it back if anything changed. Controllers pick the
//! change up on their next tick, so every edit is live and autosaved.

mod controller_view;
mod pages;
#[cfg(debug_assertions)]
mod snapshot;
mod theme;
mod widgets;

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, StrokeKind,
};

use crate::composer::Composer;
use crate::device::{Device, Link};
use crate::engine::Engine;
use crate::profile::Profile;
use theme::*;

static CTX: OnceLock<egui::Context> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Page {
    Overview,
    Lighting,
    Triggers,
    Haptics,
    Input,
    Mappings,
    Virtual,
    Profiles,
    Settings,
}

impl Page {
    const ALL: [Page; 9] = [
        Page::Overview,
        Page::Lighting,
        Page::Triggers,
        Page::Haptics,
        Page::Input,
        Page::Mappings,
        Page::Virtual,
        Page::Profiles,
        Page::Settings,
    ];
    fn key(self) -> &'static str {
        match self {
            Page::Overview => "Overview",
            Page::Lighting => "Lighting",
            Page::Triggers => "Adaptive triggers",
            Page::Haptics => "Haptics & audio",
            Page::Input => "Motion & touch",
            Page::Mappings => "Button mapping",
            Page::Virtual => "Virtual controller",
            Page::Profiles => "Profiles & games",
            Page::Settings => "Settings",
        }
    }
    fn label(self) -> &'static str { crate::i18n::tr(self.key()) }
    fn icon(self) -> &'static str {
        match self {
            Page::Overview => "◉",
            Page::Lighting => "☀",
            Page::Triggers => "◢",
            Page::Haptics => "≋",
            Page::Input => "✥",
            Page::Mappings => "⌘",
            Page::Virtual => "🎮",
            Page::Profiles => "☰",
            Page::Settings => "⚙",
        }
    }
    fn live(self) -> bool {
        matches!(
            self,
            Page::Overview
                | Page::Triggers
                | Page::Haptics
                | Page::Input
                | Page::Lighting
                | Page::Virtual
        )
    }
}

pub struct App {
    engine: Arc<Engine>,
    page: Page,
    selected: Option<u64>,
    edit_pin: Option<String>,
    preview: Composer,
    /// Keeps system-audio capture alive while the profile on screen follows loudness.
    audio_meter: Option<Arc<crate::haptics::audio::Sub>>,
    frames: u64,
    hwnd_found: bool,
    // Page state
    capture_key: Option<usize>,
    rename: Option<(String, String)>,
    new_profile: String,
    confirm_delete: Option<String>,
    /// A checked backup waiting for "Restore" to be confirmed.
    restore_pending: Option<(std::path::PathBuf, crate::backup::Backup)>,
    /// "Backup & restore…" on Profiles: when it was clicked, and whether
    /// Settings has scrolled to the card it lights up since.
    pub(crate) focus_backup: Option<(Instant, bool)>,
    audio_devices: Option<(Instant, Vec<String>)>,
    log_level: log::Level,
    processes: Option<(Instant, Vec<String>)>,
    device_name_edit: Option<(String, String)>,
    perf_log: bool,
    perf: (Instant, u32, f64),
    /// Last outer rectangle that was large enough to be a real window.
    window_frame: Option<crate::platform::WindowFrame>,
    window_repair_logged: bool,
    /// Zoom the minimum window size was last set for.
    min_size_zoom: f32,
    /// The window is at its real size, at least the minimum.
    window_sized: bool,
    /// Pages already reported as wider than the window.
    overflow_logged: std::collections::HashSet<Page>,
    virt_drivers: Option<(Instant, pages::virtual_pad::DriverInfo)>,
    game_test: Option<(u64, Arc<crate::haptics::game::TestSource>)>,
}

pub fn run(engine: Arc<Engine>, start_hidden: bool) -> Result<(), String> {
    let icon = egui::IconData {
        rgba: crate::tray::icon_rgba(64),
        width: 64,
        height: 64,
    };
    let (size, min) = crate::platform::initial_window_size([1240.0, 800.0], [760.0, 520.0]);
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(crate::APP_TITLE)
            .with_inner_size(size)
            .with_min_inner_size(min)
            // Starting minimized: never shown until the tray opens it (the
            // vendored eframe skips its first-frame show for this).
            .with_visible(!start_hidden)
            .with_icon(Arc::new(icon)),
        centered: true,
        // Frames are paced by request_repaint_after; vsync makes the NVIDIA GL
        // driver busy-wait in SwapBuffers (a full core for a 60 fps UI).
        vsync: false,
        ..Default::default()
    };
    let e = engine.clone();
    crate::tray::start(engine.clone(), || {
        if let Some(c) = CTX.get() {
            c.request_repaint();
        }
    });
    eframe::run_native(
        crate::APP_TITLE,
        opts,
        Box::new(move |cc| {
            let scale = e.settings.read().ui_scale;
            theme::install(&cc.egui_ctx, scale);
            let _ = CTX.set(cc.egui_ctx.clone());
            Ok(Box::new(App::new(e)))
        }),
    )
    .map_err(|e| e.to_string())
}

impl App {
    fn new(engine: Arc<Engine>) -> Self {
        let args: Vec<String> = std::env::args().collect();
        let page = args
            .iter()
            .position(|a| a == "--page")
            .and_then(|i| args.get(i + 1))
            .and_then(|name| {
                let n = name.to_ascii_lowercase();
                Page::ALL
                    .into_iter()
                    .find(|p| p.key().to_ascii_lowercase().starts_with(&n)
                        || (*p == Page::Input && n == "input"))
            })
            .unwrap_or(Page::Overview);
        let edit_pin = args
            .iter()
            .position(|a| a == "--edit")
            .and_then(|i| args.get(i + 1))
            .cloned();
        App {
            engine,
            page,
            selected: None,
            edit_pin,
            preview: Composer::new(),
            audio_meter: None,
            frames: 0,
            hwnd_found: false,
            capture_key: None,
            rename: None,
            new_profile: String::new(),
            confirm_delete: None,
            restore_pending: None,
            focus_backup: None,
            audio_devices: None,
            log_level: log::Level::Info,
            processes: None,
            device_name_edit: None,
            perf_log: std::env::var_os("DSZ_PERF").is_some(),
            perf: (Instant::now(), 0, 0.0),
            window_frame: None,
            window_repair_logged: false,
            overflow_logged: Default::default(),
            min_size_zoom: 0.0,
            window_sized: false,
            virt_drivers: None,
            game_test: None,
        }
    }

    fn device(&self) -> Option<Arc<Device>> {
        let list = self.engine.device_list();
        self.selected
            .and_then(|id| list.iter().find(|d| d.id == id).cloned())
            .or_else(|| list.first().cloned())
    }

    /// Name of the profile the pages edit.
    fn editing_name(&self, dev: Option<&Arc<Device>>) -> String {
        if let Some(p) = &self.edit_pin {
            if self.engine.profiles.exists(p) {
                return p.clone();
            }
        }
        match dev {
            Some(d) => self.engine.profile_name_for(d),
            None => self.engine.settings.read().default_profile.clone(),
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let (r, _) = ui.allocate_exact_size(egui::vec2(38.0, 30.0), Sense::hover());
            controller_view::logo(ui.painter(), r);
            ui.vertical(|ui| {
                ui.add_space(-2.0);
                // Wordmark: "DSZ" with the Z in the lightbar's accent blue.
                let mut mark = egui::text::LayoutJob::default();
                for (s, c) in [("DS", TEXT), ("Z", ACCENT)] {
                    mark.append(
                        s,
                        0.0,
                        egui::TextFormat {
                            font_id: heading(19.0),
                            color: c,
                            extra_letter_spacing: 1.5,
                            ..Default::default()
                        },
                    );
                }
                ui.label(mark);
                ui.add_space(-7.0);
                ui.label(RichText::new("DualSense Zen").size(12.0).color(MUTED));
            });
        });
        ui.add_space(22.0);
        // Rows give up their spacing on short windows so the footer never
        // lands on them; past that the list scrolls.
        let update = crate::update::available();
        let footer = if update.is_some() { 84.0 } else { 44.0 };
        let room = (ui.available_height() - footer).max(0.0);
        let row_h = (room / Page::ALL.len() as f32).clamp(34.0, 46.0);
        egui::ScrollArea::vertical()
            .id_salt("nav")
            .max_height(room)
            .auto_shrink([false, true])
            .show(ui, |ui| self.nav(ui, row_h));

        // Footer: connection summary.
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(14.0);
            let devs = self.engine.device_list();
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                let (txt, col) = if devs.is_empty() {
                    (crate::i18n::tr("No controller").to_string(), FAINT)
                } else {
                    (crate::localized_format!("{} connected", "{} conectado(s)", "{} conectado(s)", devs.len()), GOOD)
                };
                let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, col);
                ui.label(RichText::new(txt).size(12.5).color(MUTED));
            });
            if let Some(r) = &update {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    let b = egui::Button::new(
                        RichText::new(crate::localized_format!("Update to {}", "Atualizar para {}", "Actualizar a {}", r.version)).size(12.5).color(ACCENT),
                    )
                    .fill(ACCENT_SOFT)
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(CornerRadius::same(R_CTRL));
                    if ui
                        .add(b)
                        .on_hover_text(crate::i18n::tr("A newer DSZ is out. Settings has the update and its notes."))
                        .clicked()
                    {
                        self.page = Page::Settings;
                    }
                });
            }
        });
    }

    fn nav(&mut self, ui: &mut egui::Ui, row_h: f32) {
        ui.spacing_mut().item_spacing.y = 0.0;
        for page in Page::ALL {
            let sel = self.page == page;
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), row_h), Sense::click());
            let rect = rect.shrink2(egui::vec2(10.0, (row_h - 34.0) / 2.0));
            let p = ui.painter();
            if sel {
                p.rect_filled(rect, CornerRadius::same(R_CTRL + 2), ACCENT_SOFT);
                p.rect_filled(
                    egui::Rect::from_min_size(
                        rect.min + egui::vec2(0.0, 9.0),
                        egui::vec2(3.0, rect.height() - 18.0),
                    ),
                    CornerRadius::same(2),
                    ACCENT,
                );
            } else if resp.hovered() {
                p.rect_filled(rect, CornerRadius::same(R_CTRL + 2), CARD);
            }
            let fg = if sel || resp.hovered() { TEXT } else { MUTED };
            p.text(
                rect.left_center() + egui::vec2(18.0, 0.0),
                egui::Align2::CENTER_CENTER,
                page.icon(),
                egui::FontId::proportional(16.0),
                if sel { ACCENT } else { fg },
            );
            p.text(
                rect.left_center() + egui::vec2(36.0, 0.0),
                egui::Align2::LEFT_CENTER,
                page.label(),
                if sel {
                    heading(14.0)
                } else {
                    egui::FontId::proportional(14.0)
                },
                fg,
            );
            if resp.clicked() {
                self.page = page;
                self.capture_key = None;
            }
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, dev: Option<&Arc<Device>>) {
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            let devs = self.engine.device_list();
            if devs.is_empty() {
                widgets::pill(ui, crate::i18n::tr("Waiting for a controller…"), FAINT);
            }
            for d in &devs {
                let sel = dev.map(|x| x.id == d.id).unwrap_or(false);
                let live = d.live.lock().input;
                let link = d.link.lock().clone();
                let name = d.display_name(&self.engine);
                let conn = if d.is_bluetooth() { "BT" } else { "USB" };
                let mut text = format!("{name}  ·  {conn}  ·  {}%", live.battery_percent);
                if let Some(k) = d.virt.active() {
                    text.push_str(&crate::localized_format!("  ·  as {}", "  ·  como {}", "  ·  como {}", k.label()));
                }
                let font = egui::FontId::proportional(13.0);
                let w = ui.fonts(|f| f.layout_no_wrap(text.clone(), font.clone(), TEXT).size().x);
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(w + 38.0, 32.0), Sense::click());
                let p = ui.painter();
                p.rect_filled(
                    rect,
                    CornerRadius::same(16),
                    if sel { CARD_HI } else { CARD },
                );
                p.rect_stroke(
                    rect,
                    CornerRadius::same(16),
                    Stroke::new(1.0_f32, if sel { ACCENT } else { BORDER }),
                    StrokeKind::Inside,
                );
                let dot = match link {
                    Link::Live => GOOD,
                    Link::Quiet => WARN,
                    Link::Opening => MUTED,
                    Link::Error(_) => BAD,
                };
                p.circle_filled(rect.left_center() + egui::vec2(16.0, 0.0), 4.0, dot);
                p.text(
                    rect.left_center() + egui::vec2(28.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    text,
                    font,
                    TEXT,
                );
                if resp.clicked() {
                    self.selected = Some(d.id);
                    self.edit_pin = None;
                }
                resp.on_hover_text(match link {
                    Link::Live => crate::i18n::tr("Connected"),
                    Link::Quiet => crate::i18n::tr("No input recently (asleep, out of range, or interference)"),
                    Link::Opening => crate::i18n::tr("Connecting…"),
                    Link::Error(_) => crate::i18n::tr("Error"),
                });
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let names = self.engine.profiles.names();
                let active = match dev {
                    // What the controller is using, so a game's profile can be
                    // switched away from.
                    Some(d) => self.engine.profile_name_for(d),
                    None => self.engine.settings.read().default_profile.clone(),
                };
                let mut chosen = active.clone();
                egui::ComboBox::from_id_salt("profile-switch")
                    .width(200.0)
                    .selected_text(RichText::new(&active).strong())
                    .show_ui(ui, |ui| {
                        for n in &names {
                            ui.selectable_value(&mut chosen, n.clone(), n);
                        }
                    });
                if chosen != active {
                    match dev {
                        Some(d) => self.engine.set_device_profile(d, &chosen),
                        None => {
                            self.engine.settings.write().default_profile = chosen.clone();
                            self.engine.settings_changed();
                        }
                    }
                    self.edit_pin = None;
                }
                ui.label(RichText::new(crate::i18n::tr("Profile")).color(MUTED));
                if let Some((p, exe)) = &*self.engine.game_override.read() {
                    if self.engine.settings.read().auto_profiles {
                        widgets::pill(ui, &format!("{exe} → {p}"), VIOLET).on_hover_text(
                            crate::i18n::tr("A game rule is active. Its profile is in use until the game closes."),
                        );
                    }
                }
            });
        });
    }

    fn editing_banner(&mut self, ui: &mut egui::Ui, dev: Option<&Arc<Device>>, editing: &str) {
        let in_use = match dev {
            Some(d) => self.engine.profile_name_for(d),
            None => self.engine.settings.read().default_profile.clone(),
        };
        if self.page == Page::Overview || self.page == Page::Settings || self.page == Page::Profiles
        {
            return;
        }
        if editing != in_use {
            ui.horizontal(|ui| {
                widgets::pill(
                    ui,
                    &crate::localized_format!("Editing \"{editing}\" — not active on this controller", "Editando \"{editing}\" — inativo neste controle", "Editando \"{editing}\" — no está activo en este mando"),
                    WARN,
                );
                if widgets::button(ui, crate::i18n::tr("Edit active profile")).clicked() {
                    self.edit_pin = None;
                }
                if let Some(d) = dev {
                    if widgets::button(ui, crate::i18n::tr("Activate")).clicked() {
                        self.engine.set_device_profile(d, editing);
                        self.edit_pin = None;
                    }
                }
            });
            ui.add_space(6.0);
        }
        if let Some(d) = dev {
            let o = d.overrides.lock().clone();
            if o.any() {
                ui.horizontal(|ui| {
                    widgets::pill(ui, crate::i18n::tr("A game mod is overriding some settings (UDP)"), VIOLET);
                    if widgets::button(ui, crate::i18n::tr("Clear overrides")).clicked() {
                        *d.overrides.lock() = Default::default();
                        d.kick();
                    }
                });
                ui.add_space(6.0);
            }
        }
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let list: Vec<crate::engine::Toast> = {
            let mut t = self.engine.toasts.lock();
            t.retain(|x| now.duration_since(x.at) < Duration::from_millis(4200));
            t.iter().rev().take(4).cloned().collect()
        };
        if list.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-20.0, -20.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for t in list {
                    let age = now.duration_since(t.at).as_secs_f32();
                    let fade = ((4.2 - age) / 0.4).clamp(0.0, 1.0);
                    let a = (fade * 255.0) as u8;
                    egui::Frame::new()
                        .fill(with_alpha(CARD_HI, a))
                        .stroke(Stroke::new(
                            1.0_f32,
                            with_alpha(if t.warn { WARN } else { BORDER }, a),
                        ))
                        .corner_radius(CornerRadius::same(R_CTRL + 2))
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: Color32::from_black_alpha((fade * 90.0) as u8),
                        })
                        .show(ui, |ui| {
                            ui.set_max_width(360.0);
                            ui.label(RichText::new(&t.text).color(with_alpha(TEXT, a)));
                        });
                    ui.add_space(8.0);
                }
            });
        repaint_in(ctx, 50);
    }
}

/// egui subtracts its predicted frame time (1/60 s) from a repaint delay,
/// so a plain 16 ms request means "right now" and the UI spins. Add it back.
/// Lay out `add` in a column at most `max` wide, centred in the space left.
fn centered<R>(ui: &mut egui::Ui, max: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let avail = ui.available_rect_before_wrap();
    let w = avail.width().min(max);
    let left = avail.min.x + ((avail.width() - w) / 2.0).floor();
    let rect = egui::Rect::from_min_max(egui::pos2(left, avail.min.y), egui::pos2(left + w, avail.max.y));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)), add)
        .inner
}

fn repaint_in(ctx: &egui::Context, ms: u64) {
    ctx.request_repaint_after(Duration::from_millis(ms + 17));
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        crate::i18n::set_language(self.engine.settings.read().language);
        self.frames += 1;
        if crate::update::take_settings_request() {
            self.page = Page::Settings;
        }
        let t_frame = Instant::now();
        #[cfg(debug_assertions)]
        snapshot::tick(ctx, self.frames);
        if !self.hwnd_found {
            let t = crate::platform::wide(crate::APP_TITLE);
            let h = unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(
                    std::ptr::null(),
                    t.as_ptr(),
                )
            };
            if !h.is_null() {
                crate::tray::set_main_window(h as isize);
                self.hwnd_found = true;
            }
        }

        // The minimum size is in zoomed points, so a larger interface size
        // keeps a usable column; never more than the monitor holds.
        let zoom = ctx.zoom_factor();
        let monitor = ctx.input(|i| i.viewport().monitor_size).unwrap_or(egui::vec2(f32::MAX, f32::MAX));
        let min = egui::vec2(760.0f32.min(monitor.x * 0.9), 520.0f32.min(monitor.y * 0.85));
        if (zoom - self.min_size_zoom).abs() > 0.001 {
            self.min_size_zoom = zoom;
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(min));
        }
        // Windows doesn't grow a window to a new minimum by itself. (The
        // first frames report a placeholder size, hence the upper bound.)
        let now = ctx.input(|i| i.screen_rect().size());
        let short = now.x + 1.0 < min.x || now.y + 1.0 < min.y;
        if now.x < 5000.0 && short {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(now.max(min)));
        }
        self.window_sized = now.x < 5000.0 && !short;

        if ctx.input(|i| i.viewport().close_requested())
            && self.engine.settings.read().close_to_tray
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            crate::tray::hide_main_window();
            self.engine.flush();
        }

        // Minimize to tray: hide the minimized window; the tray icon
        // restores it.
        if ctx.input(|i| i.viewport().minimized) == Some(true)
            && self.engine.settings.read().minimize_to_tray
        {
            let h = crate::tray::main_window();
            let visible = !h.is_null()
                && unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(h) != 0 };
            if visible {
                crate::tray::hide_main_window();
                self.engine.flush();
            }
        }

        // Minimizing reports a 0×0 client size, and that size can get saved as
        // the restored window. Repair it before laying out, or egui keeps a
        // zero screen rect and the window stays an empty frame.
        let care = crate::platform::maintain_window(
            crate::tray::main_window() as isize,
            &mut self.window_frame,
            &mut self.window_repair_logged,
        );
        if !care.draw {
            match care.wake_ms {
                Some(0) => ctx.request_repaint(),
                Some(ms) => repaint_in(ctx, ms),
                None => {}
            }
            return;
        }

        let dev = self.device();
        let name = self.editing_name(dev.as_ref());
        let base = self.engine.profiles.get_or_first(&name);
        let mut profile: Profile = (*base).clone();

        egui::SidePanel::left("nav")
            .exact_width(222.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(SIDEBAR)
                    .stroke(Stroke::new(1.0_f32, BORDER)),
            )
            .show(ctx, |ui| self.sidebar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin {
                left: 28,
                right: 28,
                top: 16,
                bottom: 8,
            }))
            .show(ctx, |ui| {
                // Pages are at most CONTENT wide, centred in a wider window.
                const CONTENT: f32 = 1180.0;
                centered(ui, CONTENT, |ui| self.top_bar(ui, dev.as_ref()));
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .id_salt(format!("{:?}", self.page))
                    .show(ui, |ui| {
                        centered(ui, CONTENT, |ui| {
                        // Overflowing content grows max_rect too, so keep
                        // the column's width from before the page.
                        let column = ui.max_rect().width();
                        self.editing_banner(ui, dev.as_ref(), &profile.name);
                        self.finish_game_test();
                        match self.page {
                            Page::Overview => self.page_overview(ui, &mut profile, dev.as_ref()),
                            Page::Lighting => self.page_lighting(ui, &mut profile, dev.as_ref()),
                            Page::Triggers => self.page_triggers(ui, &mut profile, dev.as_ref()),
                            Page::Haptics => self.page_haptics(ui, &mut profile, dev.as_ref()),
                            Page::Input => self.page_input(ui, &mut profile, dev.as_ref()),
                            Page::Mappings => self.page_mappings(ui, ctx, &mut profile),
                            Page::Virtual => self.page_virtual(ui, &mut profile, dev.as_ref()),
                            Page::Profiles => self.page_profiles(ui, &mut profile, dev.as_ref()),
                            Page::Settings => self.page_settings(ui),
                        }
                        ui.add_space(24.0);
                        // A row that can't wrap widens the whole page and
                        // clips its right edge: say which page, once.
                        let over = ui.min_rect().width() - column;
                        if over > 1.0 && self.window_sized && self.overflow_logged.insert(self.page) {
                            log::warn!(
                                "layout: {:?} is {over:.0} pt wider than its {column:.0} pt column",
                                self.page
                            );
                        }
                        });
                    });
            });

        // Publish edits (renames go through the store, not this path).
        self.sync_audio_meter(&profile);
        let audio_bar = profile.lighting.mode == crate::profile::LightMode::Audio;
        if profile.name == base.name && profile != *base {
            self.engine.profiles.update(profile);
        }

        // Hidden or minimized: schedule nothing. The engine runs on its own
        // threads; the tray wakes the UI when the window comes back.
        let h = crate::tray::main_window();
        let visible = h.is_null()
            || unsafe {
                use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};
                IsWindowVisible(h) != 0 && IsIconic(h) == 0
            };
        if self.perf_log {
            self.perf.1 += 1;
            self.perf.2 += t_frame.elapsed().as_secs_f64() * 1000.0;
            if self.perf.0.elapsed() >= Duration::from_secs(2) {
                let secs = self.perf.0.elapsed().as_secs_f64();
                log::info!(
                    "ui: {:.1} fps, {:.2} ms per update, visible {visible}, causes {:?}",
                    self.perf.1 as f64 / secs,
                    self.perf.2 / self.perf.1.max(1) as f64,
                    ctx.repaint_causes()
                );
                self.perf = (Instant::now(), 0, 0.0);
            }
        }
        if !visible {
            return;
        }
        self.toasts(ctx);
        let fast = (self.page.live() && dev.is_some())
            || (audio_bar && matches!(self.page, Page::Lighting | Page::Overview));
        repaint_in(ctx, if fast { 16 } else { 250 });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.engine.flush();
    }
}

impl App {
    fn sync_audio_meter(&mut self, profile: &Profile) {
        if profile.lighting.mode != crate::profile::LightMode::Audio {
            self.audio_meter = None;
            return;
        }
        let device = profile.haptics.device.as_str();
        let same = self
            .audio_meter
            .as_ref()
            .is_some_and(|s| s.device == device);
        if !same {
            self.audio_meter = Some(self.engine.audio.subscribe_meter(device));
        }
    }
}
