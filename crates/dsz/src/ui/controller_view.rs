//! Live vector drawing of a DualSense / DualSense Edge.
//!
//! The shapes are OpenController's (Brunovncs, MIT, see
//! `assets/OpenController-LICENSE`): its `dualsense` and `dualsenseEdge`
//! looks from `assets/pads.json`, SVG paths on a 400 × 280 grid. Each part
//! is flattened and triangulated once, drawn as a mesh, and lit here from
//! the live input.
//!
//! The app's mark (`logo`) is the same silhouette as the app icon
//! (`icon_art`).

use std::collections::HashMap;
use std::sync::OnceLock;

use eframe::egui::{
    self, epaint::Mesh, pos2, vec2, Color32, CornerRadius, Pos2, Rect, Shape, Stroke,
    StrokeKind, Ui,
};
use serde::Deserialize;

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

use icon_art::PADS;

/// Maps design units to the screen: `origin` is where design (`ox`, `oy`)
/// lands, `s` is pixels per unit.
struct Space {
    origin: Pos2,
    ox: f32,
    oy: f32,
    s: f32,
}

impl Space {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.origin.x + (x - self.ox) * self.s,
            self.origin.y + (y - self.oy) * self.s,
        )
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
    /// Bounds: x0, y0, x1, y1.
    bb: [f32; 4],
}

impl Poly {
    fn new(pts: Vec<(f32, f32)>) -> Poly {
        let tris = triangulate(&pts);
        let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for &(x, y) in &pts {
            bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
        }
        Poly { pts, tris, bb }
    }

    fn center(&self) -> (f32, f32) {
        ((self.bb[0] + self.bb[2]) / 2.0, (self.bb[1] + self.bb[3]) / 2.0)
    }

    /// Filled mesh, shaded from `top` to `bottom` over design rows
    /// `shade.0..shade.1`, with an anti-aliased rim of `rim`.
    fn draw(
        &self,
        p: &egui::Painter,
        sp: &Space,
        top: Color32,
        bottom: Color32,
        rim: Stroke,
        shade: (f32, f32),
        dy: f32,
    ) {
        self.draw_grown(p, sp, top, bottom, rim, shade, dy, 0.0);
    }

    /// As `draw`, with the solid edge pushed `grow` physical pixels outward
    /// before it fades: a shape drawn over another along a shared edge
    /// covers that edge instead of letting a hairline of it show.
    #[allow(clippy::too_many_arguments)]
    fn draw_grown(
        &self,
        p: &egui::Painter,
        sp: &Space,
        top: Color32,
        bottom: Color32,
        rim: Stroke,
        shade: (f32, f32),
        dy: f32,
        grow: f32,
    ) {
        let mut mesh = Mesh::default();
        let pts: Vec<Pos2> = self.pts.iter().map(|&(x, y)| sp.p(x, y + dy)).collect();
        let n = pts.len();
        let px = 1.0 / p.ctx().pixels_per_point();
        let normals = outward_normals(&pts);
        for (i, &(_, y)) in self.pts.iter().enumerate() {
            let t = ((y - shade.0) / (shade.1 - shade.0).max(1.0)).clamp(0.0, 1.0);
            let at = pts[i] + normals.get(i).copied().unwrap_or_default() * grow * px;
            mesh.colored_vertex(at, mix(top, bottom, t));
        }
        mesh.indices = self.tris.clone();
        // A mesh has no anti-aliasing of its own: fade each edge out over
        // one physical pixel, as egui's own shapes do.
        if n >= 3 {
            for i in 0..n {
                mesh.colored_vertex(pts[i] + normals[i] * (grow + 1.0) * px, Color32::TRANSPARENT);
            }
            for i in 0..n as u32 {
                let j = (i + 1) % n as u32;
                let (oi, oj) = (n as u32 + i, n as u32 + j);
                mesh.indices.extend_from_slice(&[i, j, oj, i, oj, oi]);
            }
        }
        p.add(Shape::mesh(mesh));
        if rim.width > 0.0 {
            let line: Vec<Pos2> = self.pts.iter().map(|&(x, y)| sp.p(x, y + dy)).collect();
            p.add(Shape::closed_line(line, rim));
        }
    }

    /// One flat color, rim in the same color so the edge is smooth.
    fn fill(&self, p: &egui::Painter, sp: &Space, c: Color32, rim: Color32) {
        let y = (self.bb[1], self.bb[3]);
        self.draw(p, sp, c, c, Stroke::new(1.0_f32, rim), y, 0.0);
    }
}

/// Unit vectors pointing out of a closed outline, one per point (the mean
/// of the two edges' normals).
fn outward_normals(pts: &[Pos2]) -> Vec<egui::Vec2> {
    let n = pts.len();
    if n < 3 {
        return vec![egui::Vec2::ZERO; n];
    }
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum();
    // Positive shoelace area with y down is a clockwise outline, whose
    // outside is the edge direction turned anticlockwise.
    let out = if area > 0.0 { 1.0 } else { -1.0 };
    let normal = |a: Pos2, b: Pos2| {
        let d = (b - a).normalized();
        vec2(d.y, -d.x) * out
    };
    (0..n)
        .map(|i| {
            let prev = pts[(i + n - 1) % n];
            let next = pts[(i + 1) % n];
            let m = normal(prev, pts[i]) + normal(pts[i], next);
            if m.length_sq() > 1e-6 {
                m.normalized()
            } else {
                normal(pts[i], next)
            }
        })
        .collect()
}

use icon_art::flatten_path as flatten;

#[derive(Deserialize)]
struct RawLook {
    body: String,
    bumpers: [String; 2],
    triggers: [String; 2],
    panels: Vec<RawPanel>,
    touchpad: String,
    dots: Vec<[f32; 3]>,
    sticks: [RawStick; 2],
    buttons: Vec<RawButton>,
}

#[derive(Deserialize)]
struct RawPanel {
    style: String,
    d: String,
}

#[derive(Deserialize)]
struct RawStick {
    circle: [f32; 3],
    well: f32,
}

#[derive(Deserialize)]
struct RawButton {
    input: String,
    circle: Option<[f32; 3]>,
    d: Option<String>,
}

enum Part {
    Circle(f32, f32, f32),
    Shape(Poly),
}

struct Stick {
    x: f32,
    y: f32,
    cap: f32,
    well: f32,
}

struct Look {
    body: Poly,
    bumpers: [Poly; 2],
    triggers: [Poly; 2],
    /// The dark center panel.
    plate: Poly,
    /// The lightbar strip round the touchpad.
    lightbar: Poly,
    /// The microphone hole.
    mic: Option<Poly>,
    touchpad: Poly,
    dots: Vec<[f32; 3]>,
    sticks: [Stick; 2],
    buttons: Vec<(Button, Part)>,
    /// Row of the player LEDs, between the lightbar strip and the grille.
    leds_y: f32,
    /// What to fit on screen: x0, y0, x1, y1.
    bounds: [f32; 4],
}

const CURVE_STEPS: usize = 8;

fn button_for(name: &str) -> Option<Button> {
    Some(match name {
        "south" => Button::Cross,
        "east" => Button::Circle,
        "west" => Button::Square,
        "north" => Button::Triangle,
        "up" => Button::DpadUp,
        "down" => Button::DpadDown,
        "left" => Button::DpadLeft,
        "right" => Button::DpadRight,
        "back" => Button::Create,
        "start" => Button::Options,
        "guide" => Button::Ps,
        "misc1" => Button::Mute,
        "lp2" => Button::FnLeft,
        "rp2" => Button::FnRight,
        _ => return None,
    })
}

fn build(raw: RawLook) -> Look {
    let poly = |d: &str| Poly::new(flatten(d, CURVE_STEPS));
    let panel = |style: &str| raw.panels.iter().find(|p| p.style == style).map(|p| poly(&p.d));
    let body = poly(&raw.body);
    let [b0, b1] = &raw.bumpers;
    let [t0, t1] = &raw.triggers;
    let triggers = [poly(t0), poly(t1)];
    let touchpad = poly(&raw.touchpad);
    let lightbar = panel("light").unwrap_or_else(|| poly(&raw.touchpad));
    let grille_top = raw
        .dots
        .iter()
        .map(|d| d[1] - d[2])
        .fold(f32::MAX, f32::min);
    let leds_y = if grille_top < f32::MAX {
        (lightbar.bb[3] + grille_top) / 2.0
    } else {
        lightbar.bb[3] + 2.0
    };
    let mut bounds = body.bb;
    for t in &triggers {
        bounds = [
            bounds[0].min(t.bb[0]),
            bounds[1].min(t.bb[1]),
            bounds[2].max(t.bb[2]),
            bounds[3].max(t.bb[3]),
        ];
    }
    // Room for the drop shadow under the grips.
    bounds = [bounds[0] - 4.0, bounds[1] - 4.0, bounds[2] + 4.0, bounds[3] + 8.0];
    let stick = |s: &RawStick| Stick {
        x: s.circle[0],
        y: s.circle[1],
        cap: s.circle[2],
        well: s.well,
    };
    let buttons = raw
        .buttons
        .iter()
        .filter_map(|b| {
            let btn = button_for(&b.input)?;
            let part = match (&b.circle, &b.d) {
                (Some([x, y, r]), _) => Part::Circle(*x, *y, *r),
                (None, Some(d)) => Part::Shape(poly(d)),
                _ => return None,
            };
            Some((btn, part))
        })
        .collect();
    Look {
        bumpers: [poly(b0), poly(b1)],
        triggers,
        plate: panel("shade").unwrap_or_else(|| poly(&raw.body)),
        lightbar,
        mic: panel("part"),
        touchpad,
        dots: raw.dots.clone(),
        sticks: [stick(&raw.sticks[0]), stick(&raw.sticks[1])],
        buttons,
        leds_y,
        bounds,
        body,
    }
}

fn looks() -> &'static HashMap<String, Look> {
    static L: OnceLock<HashMap<String, Look>> = OnceLock::new();
    L.get_or_init(|| {
        let raw: HashMap<String, RawLook> =
            serde_json::from_str(PADS).expect("assets/pads.json parses");
        raw.into_iter().map(|(k, v)| (k, build(v))).collect()
    })
}

/// Ear-clipping triangulation of a simple polygon (either winding).
fn triangulate(pts: &[(f32, f32)]) -> Vec<u32> {
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
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
    let mut start = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for k in 0..m {
            // Resume near the last ear: neighbours of a clipped ear are
            // the likeliest next ones, which keeps long outlines fast.
            let i = (start + k) % m;
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
                start = i.saturating_sub(1);
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

struct Mark {
    body: Poly,
    plate: Poly,
    lightbar: Poly,
    touchpad: Poly,
}

fn mark() -> &'static Mark {
    static G: OnceLock<Mark> = OnceLock::new();
    G.get_or_init(|| {
        let s = icon_art::silhouette(CURVE_STEPS);
        Mark {
            body: Poly::new(s.body),
            plate: Poly::new(s.plate),
            lightbar: Poly::new(s.lightbar),
            touchpad: Poly::new(s.touchpad),
        }
    })
}

/// The app's mark: the controller silhouette (as in the app icon), fitted
/// to `rect`.
pub fn logo(p: &egui::Painter, rect: Rect) {
    let g = mark();
    let [x0, y0, x1, y1] = g.body.bb;
    let s = (rect.width() / (x1 - x0)).min(rect.height() / (y1 - y0));
    let origin = pos2(
        rect.center().x - (x1 - x0) * s / 2.0,
        rect.center().y - (y1 - y0) * s / 2.0,
    );
    let sp = Space { origin, ox: x0, oy: y0, s };
    let rows = (y0, y1);
    let shell = Color32::from_rgb(0xE4, 0xE8, 0xF0);
    g.body.draw(p, &sp, shell, Color32::from_rgb(0xB4, 0xBC, 0xCB), Stroke::NONE, rows, 0.0);
    // The plate shares the body's lower arch: cover it, or a hairline of
    // shell shows along the bottom.
    g.plate.draw_grown(p, &sp, Color32::from_rgb(0x1B, 0x21, 0x2E), Color32::from_rgb(0x12, 0x16, 0x1F), Stroke::NONE, rows, 0.0, 1.0);
    // A hairline strip would vanish at this size: an outline of it, wider.
    let strip: Vec<Pos2> = g.lightbar.pts.iter().map(|&(x, y)| sp.p(x, y)).collect();
    p.add(Shape::closed_line(strip, Stroke::new(sp.l(5.0).max(1.2), ACCENT)));
    g.touchpad.draw(p, &sp, Color32::WHITE, Color32::from_rgb(0xDD, 0xE1, 0xE8), Stroke::NONE, rows, 0.0);
}

pub fn draw(ui: &mut Ui, size: egui::Vec2, v: &View) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let edge = v.model.is_edge();
    let g = &looks()[if edge { "dualsenseEdge" } else { "dualsense" }];
    let [bx0, by0, bx1, by1] = g.bounds;
    let (w, h) = (bx1 - bx0, by1 - by0);
    let s = (rect.width() / w).min(rect.height() / h);
    let origin = pos2(rect.center().x - w * s / 2.0, rect.center().y - h * s / 2.0);
    let sp = Space { origin, ox: bx0, oy: by0, s };
    let p = ui.painter_at(rect);
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
    let rows = (g.body.bb[1], g.body.bb[3]);
    // Shell: soft white to a cooler grey. Center panel: near black.
    let shell_top = fade(Color32::from_rgb(0xDF, 0xE3, 0xEB));
    let shell_bottom = fade(Color32::from_rgb(0xA3, 0xAB, 0xBB));
    let shell_rim = fade(Color32::from_rgb(0x8C, 0x95, 0xA7));
    let plate_top = fade(Color32::from_rgb(0x1C, 0x22, 0x2E));
    let plate_bottom = fade(Color32::from_rgb(0x12, 0x16, 0x1F));
    let key = fade(Color32::from_rgb(0xEC, 0xEF, 0xF4));
    let key_rim = fade(Color32::from_rgb(0x93, 0x9B, 0xAB));
    let dark_key = fade(Color32::from_rgb(0x2C, 0x33, 0x42));
    let dark_rim = fade(Color32::from_rgb(0x3D, 0x46, 0x58));
    let well = fade(Color32::from_rgb(0x0A, 0x0D, 0x13));
    let label = fade(with_alpha(TEXT, 190));
    let thin = Stroke::new(1.0_f32, dark_rim);

    // Triggers, with pressure fill from the bottom, then bumpers: behind
    // the shell.
    for (t, val, b) in [(&g.triggers[0], i.l2, Button::L2), (&g.triggers[1], i.r2, Button::R2)] {
        let rim = if on(b) { lit } else { dark_rim };
        t.draw(&p, &sp, dark_key, dark_key, Stroke::new(1.0_f32, rim), rows, 0.0);
        if v.connected && val > 0 {
            let f = val as f32 / 255.0;
            let y = t.bb[3] - (t.bb[3] - t.bb[1]) * f;
            let band = p.with_clip_rect(sp.r(t.bb[0] - 1.0, y, t.bb[2] + 1.0, t.bb[3] + 1.0));
            let c = with_alpha(lit, 140 + (f * 115.0) as u8);
            t.fill(&band, &sp, c, c);
        }
    }
    for (bm, b) in [(&g.bumpers[0], Button::L1), (&g.bumpers[1], Button::R1)] {
        let c = if on(b) { lit } else { fade(Color32::from_rgb(0x4A, 0x53, 0x66)) };
        bm.draw(&p, &sp, c, c, thin, rows, 0.0);
    }

    // Shadow, shell, center panel.
    for (dy, a) in [(8.0, 26u8), (4.5, 40)] {
        let sh = Color32::from_black_alpha(if v.connected { a } else { a / 2 });
        g.body.draw(&p, &sp, sh, sh, Stroke::NONE, rows, dy);
    }
    g.body.draw(&p, &sp, shell_top, shell_bottom, Stroke::new(1.0_f32, shell_rim), rows, 0.0);
    g.plate.draw(&p, &sp, plate_top, plate_bottom, Stroke::new(1.0_f32, plate_bottom), rows, 0.0);
    if let Some(m) = &g.mic {
        m.fill(&p, &sp, well, dark_rim);
    }
    // Speaker grille.
    for &[x, y, r] in &g.dots {
        p.circle_filled(sp.p(x, y), sp.l(r), well);
    }

    // Lightbar strip, glowing in the lightbar color.
    let lb = rgb(v.lightbar);
    // A lightbar this dim reads as off; drawing it would look like a gap.
    let lb_on = v.connected && v.lightbar.iter().any(|&c| c > 24);
    if lb_on {
        let rim: Vec<Pos2> = g.lightbar.pts.iter().map(|&(x, y)| sp.p(x, y)).collect();
        for (wd, a) in [(9.0, 22u8), (5.5, 50), (3.0, 110)] {
            p.add(Shape::closed_line(rim.clone(), Stroke::new(sp.l(wd), with_alpha(lb, a))));
        }
        g.lightbar.fill(&p, &sp, lb, lb);
    } else {
        g.lightbar.fill(&p, &sp, dark_key, dark_rim);
    }

    // Touchpad, with live touch points.
    let tp_top = if on(Button::Touchpad) {
        mix(key, lit, 0.35)
    } else {
        fade(Color32::from_rgb(0xE6, 0xE9, 0xEF))
    };
    let tp_bottom = mix(tp_top, shell_bottom, 0.35);
    let tb = g.touchpad.bb;
    g.touchpad.draw(&p, &sp, tp_top, tp_bottom, Stroke::new(1.0_f32, key_rim), (tb[1], tb[3]), 0.0);
    if v.connected {
        let (mx, my) = ((tb[2] - tb[0]) * 0.06, (tb[3] - tb[1]) * 0.08);
        for (k, t) in i.touch.iter().enumerate() {
            if t.active {
                let x = egui::lerp(tb[0] + mx..=tb[2] - mx, t.x as f32 / 1919.0);
                let y = egui::lerp(tb[1] + my..=tb[3] - my, t.y as f32 / 1079.0);
                let c = if k == 0 { ACCENT } else { VIOLET };
                p.circle_filled(sp.p(x, y), sp.l(7.0), with_alpha(c, 70));
                p.circle_filled(sp.p(x, y), sp.l(3.3), c);
            }
        }
    }

    // Player LEDs under the touchpad.
    let cx = g.touchpad.center().0;
    for k in 0..5 {
        let c = sp.p(cx + (k as f32 - 2.0) * 5.0, g.leds_y);
        let lit_led = v.connected && v.player_leds & (1 << k) != 0;
        if lit_led {
            p.circle_filled(c, sp.l(2.6), with_alpha(Color32::WHITE, 45));
        }
        p.circle_filled(c, sp.l(1.1), if lit_led { Color32::WHITE } else { dark_rim });
    }

    // Sticks in their wells.
    for (st, x, y, b) in [
        (&g.sticks[0], i.lx, i.ly, Button::L3),
        (&g.sticks[1], i.rx, i.ry, Button::R3),
    ] {
        let base = sp.p(st.x, st.y);
        let (ox, oy) = if v.connected {
            ((x as f32 - 128.0) / 128.0, (y as f32 - 128.0) / 128.0)
        } else {
            (0.0, 0.0)
        };
        let moved = ox.abs() > 0.08 || oy.abs() > 0.08;
        p.circle_filled(base, sp.l(st.well), with_alpha(Color32::BLACK, if v.connected { 90 } else { 45 }));
        p.circle_stroke(
            base,
            sp.l(st.well),
            Stroke::new(1.0_f32, if moved || on(b) { with_alpha(lit, 150) } else { dark_rim }),
        );
        // The cap leans as far as the opening lets it.
        let reach = (st.well - st.cap).max(st.cap * 0.25) * 0.8;
        let cap = base + vec2(ox, oy) * sp.l(reach);
        let cap_fill = if on(b) { lit } else { fade(Color32::from_rgb(0x2B, 0x32, 0x41)) };
        p.circle_filled(cap + vec2(0.0, sp.l(1.2)), sp.l(st.cap), Color32::from_black_alpha(70));
        p.circle_filled(cap, sp.l(st.cap), cap_fill);
        p.circle_stroke(cap, sp.l(st.cap), thin);
        p.circle_stroke(
            cap,
            sp.l(st.cap * 0.72),
            Stroke::new(sp.l(0.8).max(1.0), fade(Color32::from_rgb(0x1F, 0x25, 0x31))),
        );
        if v.connected && moved {
            p.circle_filled(cap, sp.l(2.0), ACCENT);
        }
    }

    // Buttons.
    let sym = |b: Button| match b {
        Button::Triangle => Color32::from_rgb(0x2F, 0xB8, 0x8C),
        Button::Circle => Color32::from_rgb(0xE8, 0x55, 0x5A),
        Button::Cross => Color32::from_rgb(0x4F, 0x86, 0xF0),
        _ => Color32::from_rgb(0xD8, 0x6C, 0xC4),
    };
    for (b, part) in &g.buttons {
        let b = *b;
        let pressed = on(b);
        let face = matches!(b, Button::Cross | Button::Circle | Button::Square | Button::Triangle);
        let on_shell = face || matches!(b, Button::DpadUp | Button::DpadDown | Button::DpadLeft | Button::DpadRight);
        match part {
            Part::Shape(poly) => {
                let fill = if pressed {
                    lit
                } else if b == Button::Mute && v.connected && v.mute_led {
                    Color32::from_rgb(0xFF, 0x8A, 0x3D)
                } else if on_shell {
                    key
                } else {
                    dark_key
                };
                let rim = if on_shell { key_rim } else { dark_rim };
                poly.draw(&p, &sp, fill, fill, Stroke::new(1.0_f32, rim), rows, 0.0);
            }
            &Part::Circle(x, y, r) => {
                let c = sp.p(x, y);
                if face {
                    p.circle_filled(c, sp.l(r), if pressed { sym(b) } else { key });
                    p.circle_stroke(c, sp.l(r), Stroke::new(1.0_f32, key_rim));
                    face_symbol(&p, c, sp.l(r * 0.4), b, if pressed { Color32::WHITE } else { fade(sym(b)) });
                } else {
                    p.circle_filled(c, sp.l(r), if pressed { lit } else { dark_key });
                    p.circle_stroke(c, sp.l(r), thin);
                    if b == Button::Ps {
                        p.text(
                            c,
                            egui::Align2::CENTER_CENTER,
                            "PS",
                            heading(sp.l(r * 0.75).max(7.0)),
                            label,
                        );
                    }
                }
            }
        }
    }

    // Edge back paddles, seen through the grips.
    if edge {
        for (x, b, name) in [(52.0, Button::PaddleLeft, "LB"), (348.0, Button::PaddleRight, "RB")] {
            let r = sp.r(x - 13.0, 208.0, x + 13.0, 219.0);
            let fill = if on(b) { lit } else { with_alpha(dark_key, 210) };
            p.rect_filled(r, sp.cr(5.0), fill);
            p.rect_stroke(r, sp.cr(5.0), thin, StrokeKind::Inside);
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                name,
                egui::FontId::proportional(sp.l(5.5).max(7.0)),
                label,
            );
        }
    }
    resp
}

fn face_symbol(p: &egui::Painter, c: Pos2, k: f32, b: Button, color: Color32) {
    let st = Stroke::new((k * 0.34).max(1.2), color);
    match b {
        Button::Triangle => {
            let pts = vec![c + vec2(0.0, -k), c + vec2(k * 0.95, k * 0.7), c + vec2(-k * 0.95, k * 0.7)];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_triangulates_fully() {
        let g = mark();
        for p in [&g.body, &g.plate, &g.lightbar, &g.touchpad] {
            assert_eq!(p.tris.len(), (p.pts.len() - 2) * 3);
        }
    }

    #[test]
    fn both_looks_load_and_triangulate_fully() {
        let l = looks();
        for name in ["dualsense", "dualsenseEdge"] {
            let g = &l[name];
            let mut polys: Vec<&Poly> = vec![&g.body, &g.plate, &g.lightbar, &g.touchpad];
            polys.extend(g.bumpers.iter());
            polys.extend(g.triggers.iter());
            polys.extend(g.mic.iter());
            for (_, part) in &g.buttons {
                if let Part::Shape(p) = part {
                    polys.push(p);
                }
            }
            for p in polys {
                assert!(p.pts.len() >= 3, "{name}: empty outline");
                assert_eq!(p.tris.len(), (p.pts.len() - 2) * 3, "{name}: outline of {} points", p.pts.len());
            }
            let want = if name == "dualsenseEdge" { 14 } else { 12 };
            assert_eq!(g.buttons.len(), want, "{name}: every button mapped");
        }
    }

    #[test]
    fn path_parsing() {
        let pts = flatten("M0 0H10V10h-10Z", 4);
        assert_eq!(pts, vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        let c = flatten("M0 0C0 10 10 10 10 0", 4);
        assert_eq!(c.len(), 5);
        assert_eq!(c[4], (10.0, 0.0));
    }
}
