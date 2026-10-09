//! Debug builds only: `DSZ_SNAPSHOT=<file.bmp>` renders the window for a
//! few frames, saves it as a BMP, and exits. With `DSZ_DATA_DIR` and
//! `--page <name>` this gives a picture of any page without a controller or
//! a second copy of the engine fighting the running app.

use eframe::egui;

pub fn path() -> Option<std::path::PathBuf> {
    std::env::var_os("DSZ_SNAPSHOT").map(Into::into)
}

/// Call once per frame.
pub fn tick(ctx: &egui::Context, frames: u64) {
    let Some(out) = path() else { return };
    if frames == 1 {
        if let Some(dir) = std::env::var_os("DSZ_ICON_DUMP") {
            for size in [16, 20, 24, 32, 48, 64, 256] {
                let f = std::path::Path::new(&dir).join(format!("icon_{size}.rgba"));
                let _ = std::fs::write(f, crate::icon_art::icon_rgba(size));
            }
        }
    }
    ctx.request_repaint();
    if frames == 20 {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
    let shot = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(img) = shot {
        if let Err(e) = std::fs::write(&out, bmp(&img)) {
            eprintln!("snapshot: {e}");
        }
        std::process::exit(0);
    }
}

/// 32-bit top-down BMP.
fn bmp(img: &egui::ColorImage) -> Vec<u8> {
    let [w, h] = img.size;
    let data = (w * h * 4) as u32;
    let mut b = Vec::with_capacity(54 + data as usize);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(54 + data).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(-(h as i32)).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]);
    for p in &img.pixels {
        b.extend_from_slice(&[p.b(), p.g(), p.r(), 255]);
    }
    b
}
