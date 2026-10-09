//! The DualSense silhouette, in code: the outline the overview page draws
//! and the app icon. Shared with `build.rs`, which turns the icon into the
//! `.ico` embedded in the exe, so keep this file free of crate imports.
//!
//! Geometry lives in a 1000 × 660 design space (the controller seen from
//! above, x to the right, y down). Outlines are the right half as cubic
//! Béziers from the top center to the bottom center, mirrored for the left.

pub const DESIGN_W: f32 = 1000.0;
pub const DESIGN_H: f32 = 660.0;

#[derive(Clone, Copy)]
pub enum Seg {
    Line(f32, f32),
    /// Control point 1, control point 2, end point.
    Cubic(f32, f32, f32, f32, f32, f32),
}

use Seg::{Cubic, Line};

/// Right half of the shell, top center to bottom center.
const BODY: [Seg; 8] = [
    Line(660.0, 70.0),
    Cubic(720.0, 52.0, 800.0, 40.0, 880.0, 52.0),
    Cubic(950.0, 64.0, 988.0, 130.0, 994.0, 230.0),
    Cubic(1000.0, 330.0, 978.0, 450.0, 940.0, 548.0),
    Cubic(912.0, 622.0, 872.0, 654.0, 826.0, 650.0),
    Cubic(782.0, 646.0, 754.0, 615.0, 734.0, 568.0),
    Cubic(708.0, 512.0, 668.0, 470.0, 610.0, 462.0),
    Cubic(570.0, 456.0, 530.0, 456.0, 500.0, 456.0),
];

/// Right half of the dark center panel (sticks, PS button), which meets the
/// shell's inner grip edge.
const PLATE: [Seg; 6] = [
    Line(655.0, 70.0),
    Cubic(690.0, 120.0, 695.0, 220.0, 705.0, 285.0),
    Cubic(715.0, 350.0, 762.0, 392.0, 768.0, 455.0),
    Cubic(772.0, 505.0, 754.0, 540.0, 734.0, 568.0),
    Cubic(708.0, 512.0, 668.0, 470.0, 610.0, 462.0),
    Cubic(570.0, 456.0, 530.0, 456.0, 500.0, 456.0),
];

/// The center panel with its bottom pushed below the shell, for clipping.
const PLATE_PAST: [Seg; 6] = [
    Line(655.0, 70.0),
    Cubic(690.0, 120.0, 695.0, 220.0, 705.0, 285.0),
    Cubic(715.0, 350.0, 762.0, 392.0, 768.0, 455.0),
    Cubic(772.0, 505.0, 754.0, 540.0, 734.0, 568.0),
    Line(700.0, 640.0),
    Line(500.0, 640.0),
];

/// Right half of the touchpad: flush with the top edge, narrowing a little
/// toward its rounded bottom corners.
const TOUCHPAD: [Seg; 4] = [
    Line(659.0, 70.0),
    Line(651.0, 240.0),
    Cubic(650.0, 256.0, 642.0, 264.0, 626.0, 264.0),
    Line(500.0, 264.0),
];

/// Lightbar strips beside the touchpad: (top, bottom) of the right one.
pub const LIGHTBAR: [(f32, f32); 2] = [(672.0, 84.0), (662.0, 246.0)];
/// Stick centers (right one) and radius.
pub const STICK: (f32, f32, f32) = (640.0, 372.0, 56.0);

fn flatten(segs: &[Seg], steps: usize) -> Vec<(f32, f32)> {
    let mut pts = vec![(500.0, 70.0)];
    let mut cur = (500.0f32, 70.0f32);
    for s in segs {
        match *s {
            Line(x, y) => {
                cur = (x, y);
                pts.push(cur);
            }
            Cubic(x1, y1, x2, y2, x, y) => {
                for i in 1..=steps {
                    let t = i as f32 / steps as f32;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    pts.push((
                        a * cur.0 + b * x1 + c * x2 + d * x,
                        a * cur.1 + b * y1 + c * y2 + d * y,
                    ));
                }
                cur = (x, y);
            }
        }
    }
    pts
}

/// A closed outline from a right half: the half, then its mirror image back
/// up the left side. No point repeats.
fn mirrored(segs: &[Seg], steps: usize) -> Vec<(f32, f32)> {
    let right = flatten(segs, steps);
    let n = right.len();
    let mut pts = right.clone();
    for &(x, y) in right[1..n - 1].iter().rev() {
        pts.push((DESIGN_W - x, y));
    }
    pts
}

pub fn body_outline(steps: usize) -> Vec<(f32, f32)> {
    mirrored(&BODY, steps)
}

pub fn plate_outline(steps: usize) -> Vec<(f32, f32)> {
    mirrored(&PLATE, steps)
}

pub fn touchpad_outline(steps: usize) -> Vec<(f32, f32)> {
    mirrored(&TOUCHPAD, steps)
}

/// The left lightbar strip as a quad, `w` design units wide.
fn lightbar_quad(right: bool, w: f32) -> Vec<(f32, f32)> {
    let [(xt, yt), (xb, yb)] = LIGHTBAR;
    let m = |x: f32| if right { x } else { DESIGN_W - x };
    vec![
        (m(xt - w / 2.0), yt),
        (m(xt + w / 2.0), yt),
        (m(xb + w / 2.0), yb),
        (m(xb - w / 2.0), yb),
    ]
}

/// `pts` moved `d` outward along the averaged edge normals (fine for the
/// smooth outlines here).
fn grown(pts: &[(f32, f32)], d: f32) -> Vec<(f32, f32)> {
    let n = pts.len();
    let area: f32 = (0..n)
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    let out = if area > 0.0 { 1.0 } else { -1.0 };
    (0..n)
        .map(|i| {
            let (a, b, c) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
            let (tx, ty) = (c.0 - a.0, c.1 - a.1);
            let len = (tx * tx + ty * ty).sqrt().max(1e-6);
            (b.0 + out * ty / len * d, b.1 - out * tx / len * d)
        })
        .collect()
}

fn circle(cx: f32, cy: f32, r: f32) -> Vec<(f32, f32)> {
    (0..48)
        .map(|i| {
            let a = i as f32 / 48.0 * std::f32::consts::TAU;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// Anti-aliased coverage (0..=1 per pixel) of a polygon, even-odd, mapped
/// into a `w` × `h` pixel grid by `p * scale + offset`.
fn coverage(poly: &[(f32, f32)], w: usize, h: usize, scale: f32, off: (f32, f32)) -> Vec<f32> {
    const SUB: usize = 5;
    let pts: Vec<(f32, f32)> = poly
        .iter()
        .map(|&(x, y)| (x * scale + off.0, y * scale + off.1))
        .collect();
    let mut cov = vec![0.0f32; w * h];
    let mut xs: Vec<f32> = Vec::new();
    for row in 0..h {
        for sub in 0..SUB {
            let sy = row as f32 + (sub as f32 + 0.5) / SUB as f32;
            xs.clear();
            for i in 0..pts.len() {
                let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
                if (a.1 <= sy) != (b.1 <= sy) {
                    xs.push(a.0 + (sy - a.1) / (b.1 - a.1) * (b.0 - a.0));
                }
            }
            xs.sort_by(|a, b| a.total_cmp(b));
            for pair in xs.chunks_exact(2) {
                let (x0, x1) = (pair[0].max(0.0), pair[1].min(w as f32));
                if x1 <= x0 {
                    continue;
                }
                let mut px = x0.floor() as usize;
                while (px as f32) < x1 && px < w {
                    let o = (x1.min(px as f32 + 1.0) - x0.max(px as f32)).max(0.0);
                    cov[row * w + px] += o / SUB as f32;
                    px += 1;
                }
            }
        }
    }
    cov
}

/// Square RGBA app icon of `size` pixels: a white DualSense with its dark
/// center panel and blue lightbar, on a transparent background.
pub fn icon_rgba(size: usize) -> Vec<u8> {
    let s = size as f32;
    let scale = s * 0.98 / DESIGN_W;
    // The shell spans y 40..656; center that span.
    let off = (s * 0.01, (s - 616.0 * scale) / 2.0 - 40.0 * scale);
    let detailed = size >= 32;
    // Thin parts get at least about a pixel.
    let px = 1.0 / scale;

    // (outline, color, clip to the shell); `None` color is the shaded shell.
    let body = body_outline(24);
    let rim_px = if size <= 32 { 1.0 } else { 1.4 };
    let mut layers: Vec<(Vec<(f32, f32)>, Option<[f32; 3]>, bool)> = vec![
        // A darker rim keeps the white shell visible on light backgrounds.
        (grown(&body, rim_px * px), Some([84.0, 93.0, 110.0]), false),
        (body.clone(), None, false),
        (mirrored(&PLATE_PAST, 24), Some([27.0, 33.0, 46.0]), true),
        (touchpad_outline(12), Some([250.0, 251.0, 253.0]), false),
    ];
    for right in [false, true] {
        layers.push((
            lightbar_quad(right, 14.0f32.max(px * 1.1)),
            Some([59.0, 130.0, 246.0]),
            false,
        ));
    }
    if detailed {
        let (cx, cy, r) = STICK;
        for x in [cx, DESIGN_W - cx] {
            layers.push((circle(x, cy, r), Some([12.0, 15.0, 22.0]), false));
            layers.push((circle(x, cy, r * 0.62), Some([52.0, 60.0, 78.0]), false));
        }
    }

    let shell = coverage(&body, size, size, scale, off);
    let mut rgba = vec![0.0f32; size * size * 4];
    for (poly, color, clip) in &layers {
        let cov = coverage(poly, size, size, scale, off);
        for (i, &c) in cov.iter().enumerate() {
            let mut a = c.clamp(0.0, 1.0);
            if *clip {
                a = a.min(shell[i].clamp(0.0, 1.0));
            }
            if a <= 0.0 {
                continue;
            }
            let rgb = color.unwrap_or_else(|| {
                // Light at the top, a cooler grey toward the grips.
                let y = (i / size) as f32 / s;
                let t = ((y - 0.2) / 0.6).clamp(0.0, 1.0);
                [242.0 - 50.0 * t, 245.0 - 46.0 * t, 250.0 - 36.0 * t]
            });
            let d = &mut rgba[i * 4..i * 4 + 4];
            // Premultiplied "over".
            for k in 0..3 {
                d[k] = rgb[k] * a + d[k] * (1.0 - a);
            }
            d[3] = a * 255.0 + d[3] * (1.0 - a);
        }
    }
    rgba.chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] / 255.0;
            let un = |c: f32| if a > 0.0 { (c / a).min(255.0) } else { 0.0 };
            [un(p[0]) as u8, un(p[1]) as u8, un(p[2]) as u8, p[3] as u8]
        })
        .collect()
}
