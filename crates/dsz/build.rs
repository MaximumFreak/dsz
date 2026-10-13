// Build the vendored libopus (controller-speaker audio) into the exe, and
// embed the Windows version resource and icon, so Windows shows
// "DSZ - DualSense Zen" (Task Manager, startup apps, tray settings,
// Explorer) instead of the exe name.
use std::path::{Path, PathBuf};

#[path = "src/icon_art.rs"]
#[allow(dead_code)]
mod icon_art;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    build_opus(&manifest.join("..").join("..").join("vendor").join("opus"));
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    println!("cargo:rerun-if-changed=assets/pads.json");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resources(&out);
    }
}

/// Source files listed under `var` in one of libopus's `*_sources.mk`.
fn mk_list(dir: &Path, mk: &str, var: &str) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(dir.join(mk)).expect("libopus source list");
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if !inside {
            if line.split('=').next().map(str::trim) == Some(var) && line.contains('=') {
                inside = true;
            }
            continue;
        }
        let file = line.trim_end_matches('\\').trim();
        if !file.is_empty() {
            out.push(dir.join(file));
        }
        if !line.ends_with('\\') {
            break;
        }
    }
    out
}

/// libopus, float build, portable C (no CPU dispatch): two 10 ms frames
/// per speaker report are cheap enough without SIMD.
fn build_opus(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files = Vec::new();
    for (mk, var) in [
        ("celt_sources.mk", "CELT_SOURCES"),
        ("silk_sources.mk", "SILK_SOURCES"),
        ("silk_sources.mk", "SILK_SOURCES_FLOAT"),
        ("opus_sources.mk", "OPUS_SOURCES"),
        ("opus_sources.mk", "OPUS_SOURCES_FLOAT"),
    ] {
        files.extend(mk_list(dir, mk, var));
    }
    let version = std::fs::read_to_string(dir.join("package_version"))
        .ok()
        .and_then(|t| t.split('"').nth(1).map(str::to_string))
        .unwrap_or_else(|| "unknown".into());
    cc::Build::new()
        .files(files)
        .include(dir.join("include"))
        .include(dir.join("celt"))
        .include(dir.join("silk"))
        .include(dir.join("silk").join("float"))
        .include(dir.join("src"))
        .include(dir)
        .define("OPUS_BUILD", None)
        .define("USE_ALLOCA", None)
        .define("HAVE_LRINT", None)
        .define("HAVE_LRINTF", None)
        .define("PACKAGE_VERSION", format!("\"{version}\"").as_str())
        .opt_level(2)
        .warnings(false)
        .compile("opus");
    println!("cargo:rustc-env=OPUS_VERSION={version}");
}

fn embed_resources(out: &Path) {
    let ico = out.join("dsz.ico");
    if let Err(e) = std::fs::write(&ico, ico_file(&[16, 20, 24, 32, 40, 48, 64, 256])) {
        println!("cargo:warning=could not write the app icon: {e}");
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap())
        .set("FileDescription", "DSZ - DualSense Zen")
        .set("ProductName", "DSZ - DualSense Zen")
        .set("CompanyName", "DSZ")
        .set("InternalName", "dsz")
        .set("OriginalFilename", "dsz.exe")
        .set("LegalCopyright", "MIT License");
    // A missing Windows SDK only costs the version info, not the build.
    if let Err(e) = res.compile() {
        println!(
            "cargo:warning=no version resource embedded ({e}); install the Windows SDK to get one"
        );
    }
}

/// An `.ico` with one 32-bit BMP image per size, drawn by `icon_art`.
fn ico_file(sizes: &[usize]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| bmp_image(s)).collect();
    let mut f = Vec::new();
    // ICONDIR: reserved, type 1 (icon), count.
    f.extend_from_slice(&0u16.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (&s, img) in sizes.iter().zip(&images) {
        // ICONDIRENTRY: 256 is written as 0.
        let dim = if s >= 256 { 0 } else { s as u8 };
        f.extend_from_slice(&[dim, dim, 0, 0]);
        f.extend_from_slice(&1u16.to_le_bytes()); // planes
        f.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        f.extend_from_slice(&(img.len() as u32).to_le_bytes());
        f.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += img.len();
    }
    for img in images {
        f.extend_from_slice(&img);
    }
    f
}

/// BITMAPINFOHEADER, bottom-up BGRA pixels, then an empty AND mask (the
/// alpha channel does the masking).
fn bmp_image(size: usize) -> Vec<u8> {
    let rgba = icon_art::icon_rgba(size);
    let mask_row = size.div_ceil(32) * 4;
    let mut b = Vec::with_capacity(40 + size * size * 4 + mask_row * size);
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(size as i32).to_le_bytes());
    b.extend_from_slice(&(2 * size as i32).to_le_bytes()); // XOR + AND
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&32u16.to_le_bytes());
    b.extend_from_slice(&[0u8; 24]); // BI_RGB, sizes, resolution, palette
    for y in (0..size).rev() {
        for x in 0..size {
            let i = (y * size + x) * 4;
            b.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    b.resize(b.len() + mask_row * size, 0);
    b
}
