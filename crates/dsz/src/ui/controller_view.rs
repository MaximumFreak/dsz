//! Live vector drawing of a DualSense / DualSense Edge.
//!
//! The shell, center panel, and touchpad are the outlines from `icon_art`
//! (the same shapes as the app icon), triangulated once and drawn as meshes
//! with a soft vertical shade; everything that moves is drawn on top.

use std::sync::OnceLock;

use eframe::egui::{
    self, epaint::Mesh, pos2, vec2, Color32, CornerRadius, Pos2, Rect, Shape, Stroke,
    StrokeKind, Ui,
};

use ds_proto::input::Button;
use ds_proto::{InputState, Model};

use super::theme::*;
use crate::icon_art;

pub struct View<'a> {
    pub input: &'a InputState,
    pub lightbar: [u8; 3],
    pub player_leds: u8,
    pub mute_led: bool,
    pub model: Model,
    pub connected: bool,
}

/// Design space: `icon_art`'s 1000 × 660, plus room above for the triggers.
const TOP: f32 = -44.0;
const H: f32 = icon_art::DESIGN_H - TOP;

struct Space {
    origin: Pos2,
    s: f32,
}

impl Space {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.origin.x + x * self.s, self.origin.y + (y - TOP) * self.s)
    }
    fn r(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(self.p(x0, y0), self.p(x1, y1))
    }
    fn l(&self, v: f32) -> f32 {
        v * self.s
    }
    fn cr(&self, v: f32) -> CornerRadius {
        CornerRadius::same(self.l(v).round().clamp(0.0, 255.0) as u8)
    }
}

/// A concave outline and its triangles, in design units.
struct Poly {
    pts: Vec<(f32, f32)>,
    tris: Vec<u32>,
}

impl Poly {
    fn new(pts: Vec<(f32, f32)>) -> Poly {
        let tris = triangulate(&pts);
        Poly { pts, tris }
    }

    /// Filled mesh, shaded from `top` to `bottom` over the design height,
    /// with an anti-aliased rim of `rim`.
    fn draw(&self, p: &egui::Painter, sp: &Space, top: Color32, bottom: Color32, rim: Stroke, dy: f32) {
        let mut mesh = Mesh::default();
        for &(x, y) in &self.pts {
            let t = ((y - 60.0) / 560.0).clamp(0.0, 1.0);
            mesh.colored_vertex(sp.p(x, y + dy), mix(top, bottom, t));
        }
        mesh.indices = self.tris.clone();
        p.add(Shape::mesh(mesh));
        if rim.width > 0.0 {
            let line: Vec<Pos2> = self.pts.iter().map(|&(x, y)| sp.p(x, y + dy)).collect();
            p.add(Shape::closed_line(line, rim));
        }
    }
}

struct Geometry {
    body: Poly,
    plate: Poly,
    touchpad: Poly,
}

fn geometry() -> &'static Geometry {
    static G: OnceLock<Geometry> = OnceLock::new();
    G.get_or_init(|| Geometry {
        body: Poly::new(icon_art::body_outline(14)),
        plate: Poly::new(icon_art::plate_outline(14)),
        touchpad: Poly::new(icon_art::touchpad_outline(8)),
    })
}

/// Ear-clipping triangulation of a simple polygon (either winding).
fn triangulate(pts: &[(f32, f32)]) -> Vec<u32> {
    let n = pts.len();
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    let sign = area.signum();
    let cross = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    };
    let inside = |p: (f32, f32), a, b, c| {
        let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
        d1 * sign >= 0.0 && d2 * sign >= 0.0 && d3 * sign >= 0.0
    };
    let mut idx: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity((n - 2) * 3);
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (pts[ia], pts[ib], pts[ic]);
            if cross(a, b, c) * sign < 0.0 {
                continue;
            }
            let blocked = idx
                .iter()
                .any(|&j| j != ia && j != ib && j != ic && inside(pts[j], a, b, c));
            if !blocked {
                out.extend([ia as u32, ib as u32, ic as u32]);
                idx.remove(i);
                clipped = true;
                break;
            }
        }
        if !clipped {
            // Degenerate leftovers (collinear points): drop one and go on.
            idx.remove(0);
        }
    }
    if idx.len() == 3 {
        out.extend(idx.iter().map(|&i| i as u32));
    }
    out
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        l(a.r(), b.r()),
        l(a.g(), b.g()),
        l(a.b(), b.b()),
        l(a.a(), b.a()),
    )
}

/// The app's mark: the controller silhouette (as in the app icon), fitted
/// to the width of `rect`.
pub fn logo(p: &egui::Painter, rect: Rect) {
    let g = geometry();
    let s = rect.width() / icon_art::DESIGN_W;
    // Center the shell (design y 40..656) vertically.
    let origin = pos2(rect.left(), rect.center().y - (348.0 - TOP) * s);
    let sp = Space { origin, s };
    let shell = Color32::from_rgb(0xE4, 0xE8, 0xF0);
    g.body.draw(p, &sp, shell, Color32::from_rgb(0xB4, 0xBC, 0xCB), Stroke::NONE, 0.0);
    g.plate.draw(p, &sp, Color32::from_rgb(0x1B, 0x21, 0x2E), Color32::from_rgb(0x12, 0x16, 0x1F), Stroke::NONE, 0.0);
    g.touchpad.draw(p, &sp, Color32::WHITE, Color32::from_rgb(0xDD, 0xE1, 0xE8), Stroke::NONE, 0.0);
    let [(xt, yt), (xb, yb)] = icon_art::LIGHTBAR;
    for x in [(xt, xb), (icon_art::DESIGN_W - xt, icon_art::DESIGN_W - xb)] {
        p.line_segment([sp.p(x.0, yt), sp.p(x.1, yb)], Stroke::new(sp.l(22.0).max(1.5), ACCENT));
    }
}

pub fn draw(ui: &mut Ui, size: egui::Vec2, v: &View) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let edge = v.model.is_edge();
    let w = icon_art::DESIGN_W;
    let s = (rect.width() / w).min(rect.height() / H);
    let origin = pos2(rect.center().x - w * s / 2.0, rect.center().y - H * s / 2.0);
    let sp = Space { origin, s };
    let p = ui.painter_at(rect);
    let g = geometry();
    let i = v.input;
    let on = |b: Button| v.connected && i.buttons.has(b);
    let fade = |c: Color32| {
        if v.connected {
            c
        } else {
            with_alpha(c, (c.a() as u32 * 120 / 255) as u8)
        }
    };
    let lit = ACCENT;
    // Shell: soft white to a cooler grey. Center panel: near black.
    let shell_top = fade(Color32::from_rgb(0xD9, 0xDE, 0xE7));
    let shell_bottom = fade(Color32::from_rgb(0xA3, 0xAB, 0xBB));
    let plate_top = fade(Color32::from_rgb(0x1C, 0x22, 0x2E));
    let plate_bottom = fade(Color32::from_rgb(0x12, 0x16, 0x1F));
    let key = fade(Color32::from_rgb(0xEC, 0xEF, 0xF4));
    let key_rim = fade(Color32::from_rgb(0x93, 0x9B, 0xAB));
    let dark_key = fade(Color32::from_rgb(0x2C, 0x33, 0x42));
    let dark_rim = fade(Color32::from_rgb(0x3D, 0x46, 0x58));
    let label = fade(with_alpha(TEXT, 190));

    // Triggers, with pressure fill, then bumpers: behind the shell.
    for (x0, x1, val, name, b) in [
        (160.0, 330.0, i.l2, "L2", Button::L2),
        (670.0, 840.0, i.r2, "R2", Button::R2),
    ] {
        let r = sp.r(x0, -40.0, x1, 40.0);
        let cr = sp.cr(26.0);
        p.rect_filled(r, cr, dark_key);
        if v.connected && val > 0 {
            let f = val as f32 / 255.0;
            let mut fill = r;
            let visible_bottom = sp.p(0.0, 16.0).y;
            fill.set_top(visible_bottom - (visible_bottom - r.top()) * f);
            p.rect_filled(fill, cr, with_alpha(lit, 140 + (f * 115.0) as u8));
        }
        p.rect_stroke(
            r,
            cr,
            Stroke::new(1.0_f32, if on(b) { lit } else { dark_rim }),
            StrokeKind::Inside,
        );
        p.text(
            sp.p((x0 + x1) / 2.0, -15.0),
            egui::Align2::CENTER_CENTER,
            name,
            heading(sp.l(20.0).max(9.0)),
            label,
        );
    }
    for (x0, x1, b) in [(150.0, 345.0, Button::L1), (655.0, 850.0, Button::R1)] {
        let r = sp.r(x0, 14.0, x1, 66.0);
        let cr = sp.cr(22.0);
        p.rect_filled(r, cr, if on(b) { lit } else { fade(Color32::from_rgb(0x4A, 0x53, 0x66)) });
        p.rect_stroke(r, cr, Stroke::new(1.0_f32, dark_rim), StrokeKind::Inside);
    }

    // Shadow, shell, center panel.
    for (dy, a) in [(22.0, 26u8), (12.0, 40)] {
        let sh = Color32::from_black_alpha(if v.connected { a } else { a / 2 });
        g.body.draw(&p, &sp, sh, sh, Stroke::NONE, dy);
    }
    g.body.draw(
        &p,
        &sp,
        shell_top,
        shell_bottom,
        Stroke::new(1.0_f32, fade(Color32::from_rgb(0x8C, 0x95, 0xA7))),
        0.0,
    );
    g.plate.draw(&p, &sp, plate_top, plate_bottom, Stroke::new(1.0_f32, plate_bottom), 0.0);

    // Lightbar strips, glowing in the lightbar color.
    let lb = rgb(v.lightbar);
    // A lightbar this dim reads as off; drawing it would look like a gap.
    let lb_on = v.connected && v.lightbar.iter().any(|&c| c > 24);
    let [(xt, yt), (xb, yb)] = icon_art::LIGHTBAR;
    for right in [false, true] {
        let m = |x: f32| if right { x } else { w - x };
        let seg = [sp.p(m(xt), yt), sp.p(m(xb), yb)];
        if lb_on {
            for (wd, a) in [(34.0, 22u8), (22.0, 50), (14.0, 110)] {
                p.line_segment(seg, Stroke::new(sp.l(wd), with_alpha(lb, a)));
            }
            p.line_segment(seg, Stroke::new(sp.l(8.0).max(1.5), lb));
        } else {
            p.line_segment(seg, Stroke::new(sp.l(8.0).max(1.5), dark_rim));
        }
    }

    // Touchpad, with live touch points.
    let tp_top = if on(Button::Touchpad) {
        mix(key, lit, 0.35)
    } else {
        fade(Color32::from_rgb(0xE6, 0xE9, 0xEF))
    };
    let tp_bottom = mix(tp_top, shell_bottom, 0.35);
    g.touchpad.draw(&p, &sp, tp_top, tp_bottom, Stroke::new(1.0_f32, key_rim), 0.0);
    if v.connected {
        for (k, t) in i.touch.iter().enumerate() {
            if t.active {
                let x = egui::lerp(352.0..=648.0, t.x as f32 / 1919.0);
                let y = egui::lerp(80.0..=254.0, t.y as f32 / 1079.0);
                let c = if k == 0 { ACCENT } else { VIOLET };
                p.circle_filled(sp.p(x, y), sp.l(17.0), with_alpha(c, 70));
                p.circle_filled(sp.p(x, y), sp.l(8.0), c);
            }
        }
    }

    // Player LEDs under the touchpad.
    for k in 0..5 {
        let c = sp.p(500.0 + (k as f32 - 2.0) * 17.0, 286.0);
        let lit_led = v.connected && v.player_leds & (1 << k) != 0;
        if lit_led {
            p.circle_filled(c, sp.l(8.0), with_alpha(Color32::WHITE, 45));
        }
        p.circle_filled(c, sp.l(3.6), if lit_led { Color32::WHITE } else { dark_rim });
    }

    // PS and mute.
    let ps = sp.p(500.0, 384.0);
    p.circle_filled(ps, sp.l(21.0), if on(Button::Ps) { lit } else { dark_key });
    p.circle_stroke(ps, sp.l(21.0), Stroke::new(1.0_f32, dark_rim));
    p.text(
        ps,
        egui::Align2::CENTER_CENTER,
        "PS",
        heading(sp.l(14.0).max(7.0)),
        label,
    );
    let mute = sp.r(478.0, 422.0, 522.0, 436.0);
    let mute_fill = if on(Button::Mute) {
        lit
    } else if v.connected && v.mute_led {
        Color32::from_rgb(0xFF, 0x8A, 0x3D)
    } else {
        dark_key
    };
    p.rect_filled(mute, sp.cr(7.0), mute_fill);

    // Create and Options, beside the touchpad's top corners.
    for (x, b) in [(305.0, Button::Create), (695.0, Button::Options)] {
        let r = sp.r(x - 8.0, 96.0, x + 8.0, 138.0);
        p.rect_filled(r, sp.cr(8.0), if on(b) { lit } else { dark_key });
    }

    // D-pad.
    let (dx, dy) = (200.0, 236.0);
    for (ox, oy, b) in [
        (0.0, -48.0, Button::DpadUp),
        (0.0, 48.0, Button::DpadDown),
        (-48.0, 0.0, Button::DpadLeft),
        (48.0, 0.0, Button::DpadRight),
    ] {
        let c = sp.p(dx + ox, dy + oy);
        let r = Rect::from_center_size(c, vec2(sp.l(44.0), sp.l(44.0)));
        let pressed = on(b);
        p.rect_filled(r, sp.cr(9.0), if pressed { lit } else { key });
        p.rect_stroke(r, sp.cr(9.0), Stroke::new(1.0_f32, key_rim), StrokeKind::Inside);
        // A small arrow pointing outward.
        let dir = vec2(ox, oy) / 48.0;
        let side = vec2(-dir.y, dir.x);
        let k = sp.l(7.0);
        let tip = c + dir * k;
        p.add(Shape::convex_polygon(
            vec![tip, c - dir * k * 0.4 + side * k, c - dir * k * 0.4 - side * k],
            if pressed { Color32::WHITE } else { key_rim },
            Stroke::NONE,
        ));
    }

    // Face buttons.
    let (fx, fy) = (800.0, 236.0);
    let sym = |b: Button| match b {
        Button::Triangle => Color32::from_rgb(0x2F, 0xB8, 0x8C),
        Button::Circle => Color32::from_rgb(0xE8, 0x55, 0x5A),
        Button::Cross => Color32::from_rgb(0x4F, 0x86, 0xF0),
        _ => Color32::from_rgb(0xD8, 0x6C, 0xC4),
    };
    for (ox, oy, b) in [
        (0.0, -50.0, Button::Triangle),
        (50.0, 0.0, Button::Circle),
        (0.0, 50.0, Button::Cross),
        (-50.0, 0.0, Button::Square),
    ] {
        let c = sp.p(fx + ox, fy + oy);
        let pressed = on(b);
        p.circle_filled(c, sp.l(24.0), if pressed { sym(b) } else { key });
        p.circle_stroke(c, sp.l(24.0), Stroke::new(1.0_f32, key_rim));
        let sc = if pressed { Color32::WHITE } else { fade(sym(b)) };
        let st = Stroke::new(sp.l(3.2).max(1.2), sc);
        let k = sp.l(9.5);
        match b {
            Button::Triangle => {
                let pts = vec![
                    c + vec2(0.0, -k),
                    c + vec2(k * 0.95, k * 0.7),
                    c + vec2(-k * 0.95, k * 0.7),
                ];
                p.add(Shape::closed_line(pts, st));
            }
            Button::Circle => {
                p.circle_stroke(c, k * 0.9, st);
            }
            Button::Cross => {
                p.line_segment([c + vec2(-k * 0.8, -k * 0.8), c + vec2(k * 0.8, k * 0.8)], st);
                p.line_segment([c + vec2(-k * 0.8, k * 0.8), c + vec2(k * 0.8, -k * 0.8)], st);
            }
            _ => {
                p.rect_stroke(
                    Rect::from_center_size(c, vec2(k * 1.6, k * 1.6)),
                    CornerRadius::same(1),
                    st,
                    StrokeKind::Middle,
                );
            }
        }
    }

    // Sticks in their wells.
    let (scx, scy, sr) = icon_art::STICK;
    for (cx, x, y, b) in [
        (w - scx, i.lx, i.ly, Button::L3),
        (scx, i.rx, i.ry, Button::R3),
    ] {
        let base = sp.p(cx, scy);
        p.circle_filled(base, sp.l(sr), fade(Color32::from_rgb(0x0A, 0x0D, 0x13)));
        p.circle_stroke(base, sp.l(sr), Stroke::new(1.0_f32, dark_rim));
        let (ox, oy) = if v.connected {
            ((x as f32 - 128.0) / 128.0, (y as f32 - 128.0) / 128.0)
        } else {
            (0.0, 0.0)
        };
        let cap = base + vec2(ox, oy) * sp.l(17.0);
        let cap_fill = if on(b) { lit } else { fade(Color32::from_rgb(0x2B, 0x32, 0x41)) };
        p.circle_filled(cap + vec2(0.0, sp.l(3.0)), sp.l(40.0), Color32::from_black_alpha(70));
        p.circle_filled(cap, sp.l(40.0), cap_fill);
        p.circle_stroke(cap, sp.l(40.0), Stroke::new(1.0_f32, dark_rim));
        p.circle_stroke(cap, sp.l(29.0), Stroke::new(sp.l(2.0).max(1.0), fade(Color32::from_rgb(0x1F, 0x25, 0x31))));
        if v.connected && (ox.abs() > 0.08 || oy.abs() > 0.08) {
            p.circle_filled(cap, sp.l(5.0), ACCENT);
        }
    }

    // Edge extras: Fn buttons under the sticks, back paddles on the grips.
    if edge {
        for (x, b) in [(w - scx, Button::FnLeft), (scx, Button::FnRight)] {
            let r = sp.r(x - 18.0, 444.0, x + 18.0, 456.0);
            p.rect_filled(r, sp.cr(6.0), if on(b) { lit } else { dark_key });
        }
        for (x, b, name) in [
            (195.0, Button::PaddleLeft, "LB"),
            (805.0, Button::PaddleRight, "RB"),
        ] {
            let r = sp.r(x - 34.0, 540.0, x + 34.0, 570.0);
            let fill = if on(b) { lit } else { with_alpha(dark_key, 210) };
            p.rect_filled(r, sp.cr(13.0), fill);
            p.rect_stroke(r, sp.cr(13.0), Stroke::new(1.0_f32, dark_rim), StrokeKind::Inside);
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                name,
                egui::FontId::proportional(sp.l(14.0).max(7.0)),
                label,
            );
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlines_triangulate_fully() {
        for pts in [
            icon_art::body_outline(14),
            icon_art::plate_outline(14),
            icon_art::touchpad_outline(8),
        ] {
            let tris = triangulate(&pts);
            assert_eq!(tris.len(), (pts.len() - 2) * 3);
        }
    }
}
