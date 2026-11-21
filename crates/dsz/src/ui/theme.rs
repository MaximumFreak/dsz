//! Colors, fonts, and egui style.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};

pub const BG: Color32 = Color32::from_rgb(0x0B, 0x0E, 0x14);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x0F, 0x13, 0x1B);
pub const CARD: Color32 = Color32::from_rgb(0x15, 0x1A, 0x25);
pub const CARD_HI: Color32 = Color32::from_rgb(0x1B, 0x22, 0x30);
pub const FIELD: Color32 = Color32::from_rgb(0x0F, 0x14, 0x1E);
pub const BORDER: Color32 = Color32::from_rgb(0x24, 0x2C, 0x3C);
pub const TEXT: Color32 = Color32::from_rgb(0xE7, 0xEB, 0xF3);
pub const MUTED: Color32 = Color32::from_rgb(0x8B, 0x95, 0xA9);
pub const FAINT: Color32 = Color32::from_rgb(0x5A, 0x64, 0x78);
pub const ACCENT: Color32 = Color32::from_rgb(0x3B, 0x82, 0xF6);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0x1D, 0x33, 0x5C);
pub const VIOLET: Color32 = Color32::from_rgb(0x8B, 0x5C, 0xF6);
pub const GOOD: Color32 = Color32::from_rgb(0x34, 0xD3, 0x99);
pub const WARN: Color32 = Color32::from_rgb(0xFB, 0xBF, 0x24);
pub const BAD: Color32 = Color32::from_rgb(0xF8, 0x71, 0x71);

pub const R_CARD: u8 = 14;
pub const R_CTRL: u8 = 8;

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn heading(size: f32) -> FontId {
    FontId::new(size, semibold())
}

/// Maps a Windows font file into memory, read-only, for the rest of the run
/// (each is loaded once). egui keeps a font given as bytes twice, so the
/// Chinese fonts read into memory cost about 75 MB; mapped, Windows reads
/// only the parts that are drawn, and can drop them again, as it does for
/// every other program using the same font.
fn load_font(paths: &[&str]) -> Option<&'static [u8]> {
    paths.iter().find_map(|p| {
        let file = std::fs::File::open(p).ok()?;
        // Safety: fonts in the Windows folder are only replaced through
        // Windows Update, which waits for a restart while a font is open.
        let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        Some(&**Box::leak(Box::new(map)))
    })
}

/// Microsoft YaHei UI as the last fallback, added the first time Chinese is
/// picked or the language list is opened. Its ~35 MB of font files are
/// mapped rather than read (see [`load_font`]), so this costs other
/// languages next to nothing. Fonts change on the next frame.
pub fn ensure_cjk(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    use std::sync::atomic::{AtomicBool, Ordering};
    static LOADED: AtomicBool = AtomicBool::new(false);
    if LOADED.swap(true, Ordering::Relaxed) {
        return;
    }
    let Some(regular) = load_font(&[r"C:\Windows\Fonts\msyh.ttc"]) else {
        log::warn!("msyh.ttc not found; Chinese text may not render");
        return;
    };
    // Face 1 of the collection is "Microsoft YaHei UI".
    let face = |bytes: &'static [u8]| FontData { index: 1, ..FontData::from_static(bytes) };
    let fallback = |family| InsertFontFamily { family, priority: FontPriority::Lowest };
    let bold = load_font(&[r"C:\Windows\Fonts\msyhbd.ttc"]);
    let mut families = vec![fallback(FontFamily::Proportional), fallback(FontFamily::Monospace)];
    if bold.is_none() {
        families.push(fallback(semibold()));
    }
    ctx.add_font(FontInsert::new("yahei", face(regular), families));
    if let Some(bytes) = bold {
        ctx.add_font(FontInsert::new("yahei_b", face(bytes), vec![fallback(semibold())]));
    }
    ctx.request_repaint();
}

pub fn install(ctx: &egui::Context, scale: f32) {
    // Only Windows' own fonts: egui's bundled ones (1.4 MB of the exe) were
    // never drawn, except emoji, which Segoe UI Symbol covers.
    let mut fonts = FontDefinitions::empty();
    let mut add = |name: &str, paths: &[&str]| {
        let bytes = load_font(paths)?;
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
        Some(name.to_string())
    };
    let regular = add(
        "segoe",
        &[
            r"C:\Windows\Fonts\segoeui.ttf",
            r"C:\Windows\Fonts\arial.ttf",
            r"C:\Windows\Fonts\tahoma.ttf",
        ],
    );
    let semi = add(
        "segoe_sb",
        &[
            r"C:\Windows\Fonts\seguisb.ttf",
            r"C:\Windows\Fonts\segoeuib.ttf",
        ],
    );
    // Symbols (arrows, shapes, emoji) for icons and glyphs.
    let symbols = add("segoe_sym", &[r"C:\Windows\Fonts\seguisym.ttf"]);
    let mono = add(
        "consolas",
        &[r"C:\Windows\Fonts\consola.ttf", r"C:\Windows\Fonts\cour.ttf"],
    );
    if regular.is_none() {
        log::error!(r"no system font found in C:\Windows\Fonts; text won't show");
    }
    let family = |first: &[&Option<String>]| -> Vec<String> {
        first.iter().filter_map(|f| (*f).clone()).collect()
    };
    fonts
        .families
        .insert(FontFamily::Proportional, family(&[&regular, &symbols]));
    fonts
        .families
        .insert(FontFamily::Monospace, family(&[&mono, &regular, &symbols]));
    fonts
        .families
        .insert(semibold(), family(&[&semi, &symbols, &regular]));
    ctx.set_fonts(fonts);

    // A zoom on top of Windows' own display scale, so it follows the window
    // to a monitor with a different scale.
    ctx.set_zoom_factor(scale.clamp(0.75, 2.0));

    // The design is dark only. `style_mut` styles the current theme, and
    // with Windows in light mode egui switches to its unstyled light theme
    // on the first frame (light dropdowns and popups), so pin dark.
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut(|s| {
        s.text_styles = [
            (
                TextStyle::Small,
                FontId::new(11.5, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(14.0, FontFamily::Proportional),
            ),
            (TextStyle::Heading, FontId::new(22.0, semibold())),
            (
                TextStyle::Monospace,
                FontId::new(12.5, FontFamily::Monospace),
            ),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(10.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size = egui::vec2(36.0, 28.0);
        s.spacing.slider_width = 160.0;
        s.spacing.combo_width = 180.0;
        s.spacing.icon_width = 16.0;
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.interaction.selectable_labels = false;

        let v = &mut s.visuals;
        *v = egui::Visuals::dark();
        v.override_text_color = Some(TEXT);
        v.window_fill = CARD;
        v.panel_fill = BG;
        v.extreme_bg_color = FIELD;
        v.faint_bg_color = CARD_HI;
        v.code_bg_color = FIELD;
        v.window_stroke = Stroke::new(1.0_f32, BORDER);
        v.window_corner_radius = CornerRadius::same(R_CARD);
        v.menu_corner_radius = CornerRadius::same(R_CTRL + 2);
        v.selection.bg_fill = Color32::from_rgb(0x2F, 0x6E, 0xE0);
        v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
        v.hyperlink_color = ACCENT;
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(120),
        };
        v.popup_shadow = v.window_shadow;

        let r = CornerRadius::same(R_CTRL);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = CARD;
        w.noninteractive.weak_bg_fill = CARD;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32, MUTED);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = CARD_HI;
        w.inactive.weak_bg_fill = CARD_HI;
        w.inactive.bg_stroke = Stroke::new(1.0_f32, BORDER);
        w.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = Color32::from_rgb(0x24, 0x2D, 0x40);
        w.hovered.weak_bg_fill = Color32::from_rgb(0x24, 0x2D, 0x40);
        w.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x36, 0x42, 0x5A));
        w.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = ACCENT;
        w.active.weak_bg_fill = Color32::from_rgb(0x2A, 0x36, 0x4E);
        w.active.bg_stroke = Stroke::new(1.0_f32, ACCENT);
        w.active.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        w.open.bg_fill = CARD_HI;
        w.open.weak_bg_fill = CARD_HI;
        w.open.bg_stroke = Stroke::new(1.0_f32, ACCENT);
        w.open.corner_radius = r;
    });
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

pub fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}
