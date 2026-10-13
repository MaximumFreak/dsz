//! The DualSense silhouette for the app icon and the sidebar mark, from
//! OpenController's drawing (Brunovncs, MIT, `assets/OpenController-LICENSE`):
//! the `dualsense` look in `assets/pads.json`, SVG paths on a 400 × 280
//! grid. Shared with `build.rs`, which turns the icon into the `.ico`
//! embedded in the exe, so keep this file free of crate imports (the JSON
//! is read with a few string searches rather than serde).

pub const PADS: &str = include_str!("../assets/pads.json");

/// An SVG path (M, L, H, V, C, Z, absolute or relative) as one outline;
/// each cubic is flattened into `steps` segments. Repeated points and a
/// closing point equal to the first are dropped.
pub fn flatten_path(d: &str, steps: usize) -> Vec<(f32, f32)> {
    let mut toks: Vec<Result<f32, char>> = Vec::new();
    let mut num = String::new();
    let end = |num: &mut String, toks: &mut Vec<Result<f32, char>>| {
        if !num.is_empty() {
            toks.push(Ok(num.parse().unwrap_or(0.0)));
            num.clear();
        }
    };
    for ch in d.chars() {
        match ch {
            '0'..='9' => num.push(ch),
            '.' if num.contains('.') && !num.contains('e') => {
                end(&mut num, &mut toks);
                num.push(ch);
            }
            '-' | '+' if !num.ends_with('e') => {
                end(&mut num, &mut toks);
                num.push(ch);
            }
            '.' | '-' | '+' | 'e' => num.push(ch),
            c if c.is_ascii_alphabetic() => {
                end(&mut num, &mut toks);
                toks.push(Err(c));
            }
            _ => end(&mut num, &mut toks),
        }
    }
    end(&mut num, &mut toks);

    let mut out: Vec<(f32, f32)> = Vec::new();
    let push = |out: &mut Vec<(f32, f32)>, q: (f32, f32)| {
        let new = match out.last() {
            Some(l) => (l.0 - q.0).abs() > 1e-3 || (l.1 - q.1).abs() > 1e-3,
            None => true,
        };
        if new {
            out.push(q);
        }
    };
    let (mut x, mut y) = (0f32, 0f32);
    let mut cmd = 'M';
    let mut args: Vec<f32> = Vec::new();
    for t in toks {
        match t {
            Err(c) => {
                cmd = c;
                args.clear();
            }
            Ok(v) => {
                args.push(v);
                let need = match cmd.to_ascii_uppercase() {
                    'M' | 'L' => 2,
                    'H' | 'V' => 1,
                    'C' => 6,
                    _ => 0,
                };
                if args.len() < need {
                    continue;
                }
                let (ox, oy) = if cmd.is_ascii_lowercase() { (x, y) } else { (0.0, 0.0) };
                match cmd.to_ascii_uppercase() {
                    'M' | 'L' => (x, y) = (args[0] + ox, args[1] + oy),
                    'H' => x = args[0] + ox,
                    'V' => y = args[0] + oy,
                    'C' => {
                        let a = &args;
                        let (x0, y0) = (x, y);
                        let c = [(a[0] + ox, a[1] + oy), (a[2] + ox, a[3] + oy), (a[4] + ox, a[5] + oy)];
                        for k in 1..steps {
                            let t = k as f32 / steps as f32;
                            let m = 1.0 - t;
                            let (b0, b1, b2, b3) = (m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t);
                            push(
                                &mut out,
                                (
                                    b0 * x0 + b1 * c[0].0 + b2 * c[1].0 + b3 * c[2].0,
                                    b0 * y0 + b1 * c[0].1 + b2 * c[1].1 + b3 * c[2].1,
                                ),
                            );
                        }
                        (x, y) = c[2];
                    }
                    _ => {}
                }
                push(&mut out, (x, y));
                // Extra pairs after M are line-tos.
                if cmd == 'M' {
                    cmd = 'L';
                } else if cmd == 'm' {
                    cmd = 'l';
                }
                args.clear();
            }
        }
    }
    if out.len() > 2 && out.first() == out.last() {
        out.pop();
    }
    out
}

/// The parts of the plain DualSense the icon and the mark use, as
/// outlines on the 400 × 280 grid.
pub struct Silhouette {
    pub body: Vec<(f32, f32)>,
    /// The dark center panel.
    pub plate: Vec<(f32, f32)>,
    /// The lightbar strip round the touchpad.
    pub lightbar: Vec<(f32, f32)>,
    pub touchpad: Vec<(f32, f32)>,
    /// Stick caps: x, y, radius.
    pub sticks: [(f32, f32, f32); 2],
}

/// The text right after `pat` in `s`.
fn after<'a>(s: &'a str, pat: &str) -> &'a str {
    let i = s.find(pat).unwrap_or_else(|| panic!("pads.json: no {pat}"));
    &s[i + pat.len()..]
}

/// A JSON string value starting right after its opening quote.
fn string(s: &str) -> &str {
    &s[..s.find('"').expect("pads.json: unterminated string")]
}

pub fn silhouette(steps: usize) -> Silhouette {
    // One look per line; the plain DualSense's line starts with its key.
    let look = PADS
        .lines()
        .find(|l| l.starts_with("\"dualsense\":"))
        .expect("pads.json: no dualsense look");
    let path = |key: &str| flatten_path(string(after(look, key)), steps);
    let circle = |s: &str| {
        let v: Vec<f32> = string_until(after(s, "\"circle\":["), ']')
            .split(',')
            .map(|n| n.trim().parse().unwrap_or(0.0))
            .collect();
        (v[0], v[1], v[2])
    };
    let sticks = after(look, "\"sticks\":[");
    let second = after(sticks, "},{");
    Silhouette {
        body: path("\"body\":\""),
        plate: path("{\"style\":\"shade\",\"d\":\""),
        lightbar: path("{\"style\":\"light\",\"d\":\""),
        touchpad: path("\"touchpad\":\""),
        sticks: [circle(sticks), circle(second)],
    }
}

fn string_until(s: &str, c: char) -> &str {
    &s[..s.find(c).expect("pads.json: unterminated list")]
}

/// Bounds of an outline: x0, y0, x1, y1.
pub fn bounds(pts: &[(f32, f32)]) -> [f32; 4] {
    let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for &(x, y) in pts {
        bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x), bb[3].max(y)];
    }
    bb
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
    let g = silhouette(10);
    let s = size as f32;
    let [x0, y0, x1, y1] = bounds(&g.body);
    let scale = s * 0.98 / (x1 - x0);
    let off = (
        (s - (x1 - x0) * scale) / 2.0 - x0 * scale,
        (s - (y1 - y0) * scale) / 2.0 - y0 * scale,
    );
    let detailed = size >= 32;
    // Thin parts get at least about a pixel.
    let px = 1.0 / scale;

    // (outline, color, clip to the shell); `None` color is the shaded shell.
    let rim_px = if size <= 32 { 1.0 } else { 1.4 };
    let mut layers: Vec<(Vec<(f32, f32)>, Option<[f32; 3]>, bool)> = vec![
        // A darker rim keeps the white shell visible on light backgrounds.
        (grown(&g.body, rim_px * px), Some([84.0, 93.0, 110.0]), false),
        (g.body.clone(), None, false),
        (g.plate.clone(), Some([27.0, 33.0, 46.0]), true),
        // The strip shows round the touchpad; small icons get it a little
        // wider so it stays visible.
        (grown(&g.lightbar, (px * 0.6 - 1.2).max(0.0)), Some([59.0, 130.0, 246.0]), false),
        (g.touchpad.clone(), Some([250.0, 251.0, 253.0]), false),
    ];
    if detailed {
        for &(x, y, r) in &g.sticks {
            layers.push((circle(x, y, r * 1.45), Some([12.0, 15.0, 22.0]), false));
            layers.push((circle(x, y, r), Some([52.0, 60.0, 78.0]), false));
        }
    }

    let shell = coverage(&g.body, size, size, scale, off);
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
                let t = ((y - 0.25) / 0.5).clamp(0.0, 1.0);
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
