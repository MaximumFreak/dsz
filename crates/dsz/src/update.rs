//! Update check and one-click update from the GitHub releases.
//!
//! Once a day (setting `check_updates`, on by default) the app asks
//! GitHub's API for the latest release. A newer one is announced in the
//! window, in the tray, and on Settings. Nothing installs by itself: an
//! update restarts the app, which unplugs the virtual controllers, so it
//! waits for a click on "Update and restart".
//!
//! The update downloads the release's `dsz.exe`, checks it against the
//! SHA-256 GitHub publishes for the file, renames the running exe to
//! `dsz.exe.old` (Windows allows renaming a running exe, not overwriting
//! it), puts the new one in its place, and starts it with `--updated`.
//! The new process waits for this one to quit and deletes the old file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use crate::engine::Engine;

const LATEST: &str = "https://api.github.com/repos/MaximumFreak/dsz/releases/latest";
pub const RELEASES: &str = "https://github.com/MaximumFreak/dsz/releases";
/// The release asset that is the app.
const ASSET: &str = "dsz.exe";
/// First check a little after startup, out of the way of controllers
/// connecting; then once a day.
const FIRST_CHECK: Duration = Duration::from_secs(20);
const EVERY: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    /// The release page, with its notes.
    pub page: String,
    url: String,
    sha256: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Checking,
    UpToDate,
    Available(Release),
    Downloading(Release),
    /// The new exe is in place and starting; this one is quitting.
    Restarting,
    Failed(String),
}

fn state() -> &'static Mutex<Option<Status>> {
    static S: OnceLock<Mutex<Option<Status>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// The last check's result, or `None` before any.
pub fn status() -> Option<Status> {
    state().lock().clone()
}

/// A newer release found by the last check, while not yet installing.
pub fn available() -> Option<Release> {
    match status() {
        Some(Status::Available(r)) => Some(r),
        _ => None,
    }
}

fn set(s: Status) {
    *state().lock() = Some(s);
}

static SHOW_SETTINGS: AtomicBool = AtomicBool::new(false);

/// Ask the window to show Settings (the update notification was clicked).
pub fn request_settings() {
    SHOW_SETTINGS.store(true, Ordering::Release);
}

/// Whether the window should switch to Settings; clears the request.
pub fn take_settings_request() -> bool {
    SHOW_SETTINGS.swap(false, Ordering::AcqRel)
}

/// The background checker: once soon after startup, then daily, while the
/// setting is on.
pub fn run(engine: Arc<Engine>) {
    let mut next = Instant::now() + FIRST_CHECK;
    let mut told: Option<String> = None;
    while !engine.shutdown.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_secs(1));
        if Instant::now() < next || !engine.settings.read().check_updates {
            continue;
        }
        next = Instant::now() + EVERY;
        if matches!(status(), Some(Status::Downloading(_) | Status::Restarting)) {
            continue;
        }
        check();
        if let Some(r) = available() {
            if told.as_deref() != Some(r.version.as_str()) {
                log::info!("DSZ {} is available", r.version);
                engine.toast(format!("DSZ {} is available: Settings > Updates", r.version));
                crate::tray::notify(
                    &format!("DSZ {} is available", r.version),
                    "Open DSZ and go to Settings to update.",
                );
                told = Some(r.version);
            }
        }
    }
}

/// Ask GitHub now. Blocks for the request; the result is in [`status`].
pub fn check() {
    set(Status::Checking);
    match latest() {
        Ok(r) if newer(&r.version, env!("CARGO_PKG_VERSION")) => set(Status::Available(r)),
        Ok(_) => set(Status::UpToDate),
        Err(e) => {
            log::info!("update check: {e}");
            set(Status::Failed(e));
        }
    }
}

/// [`check`] on its own thread; `wake` runs when it is done.
pub fn check_in_background(wake: impl Fn() + Send + 'static) {
    if matches!(
        status(),
        Some(Status::Checking | Status::Downloading(_) | Status::Restarting)
    ) {
        return;
    }
    set(Status::Checking);
    std::thread::spawn(move || {
        check();
        wake();
    });
}

fn latest() -> Result<Release, String> {
    let dir = std::env::temp_dir().join("DSZ");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join("latest-release.json");
    let _ = std::fs::remove_file(&file);
    // A fresh query each time, so a cached copy is never used.
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::install::download(&format!("{LATEST}?t={t}"), &file)
        .map_err(|_| "Couldn't reach GitHub to check for updates.".to_string())?;
    let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&file);
    parse(&text).ok_or_else(|| "GitHub's answer had no release in it.".to_string())
}

fn parse(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = v["tag_name"].as_str()?.trim_start_matches('v').to_string();
    let page = v["html_url"].as_str().unwrap_or(RELEASES).to_string();
    let asset = v["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str() == Some(ASSET))?;
    let url = asset["browser_download_url"].as_str()?.to_string();
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .and_then(hex32);
    Some(Release { version, page, url, sha256 })
}

fn hex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn parts(v: &str) -> Vec<u32> {
    v.split(|c: char| !c.is_ascii_digit())
        .take_while(|p| !p.is_empty())
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// Whether version `a` is newer than `b` ("0.3.10" > "0.3.9").
fn newer(a: &str, b: &str) -> bool {
    parts(a) > parts(b)
}

/// Download `r`, swap it in for the running exe, start it, and quit.
pub fn install(r: Release, wake: impl Fn() + Send + 'static) {
    if matches!(status(), Some(Status::Downloading(_) | Status::Restarting)) {
        return;
    }
    set(Status::Downloading(r.clone()));
    std::thread::spawn(move || {
        match swap_in(&r) {
            Ok(()) => {
                set(Status::Restarting);
                wake();
                crate::tray::quit();
            }
            Err(e) => {
                log::warn!("update to {}: {e}", r.version);
                set(Status::Failed(e));
            }
        }
        wake();
    });
}

/// Download `r`'s exe to the temp folder and check it.
fn fetch(r: &Release) -> Result<PathBuf, String> {
    let Some(want) = r.sha256 else {
        return Err("GitHub published no checksum for this download, so it can't be checked. Download it from the release page instead.".into());
    };
    let dir = std::env::temp_dir().join("DSZ");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("dsz-{}.exe", r.version));
    let _ = std::fs::remove_file(&file);
    log::info!("downloading DSZ {} from {}", r.version, r.url);
    crate::install::download(&r.url, &file)?;
    let data = std::fs::read(&file).map_err(|e| e.to_string())?;
    if crate::install::sha256(&data)? != want || !data.starts_with(b"MZ") {
        let _ = std::fs::remove_file(&file);
        return Err("The download didn't match GitHub's checksum, so it wasn't used.".into());
    }
    Ok(file)
}

fn swap_in(r: &Release) -> Result<(), String> {
    let file = fetch(r)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    let cant = |e: std::io::Error| {
        format!(
            "DSZ can't replace its own file in {} ({e}). Download the new version from the release page instead.",
            exe.parent().unwrap_or(Path::new("")).display()
        )
    };
    std::fs::rename(&exe, &old).map_err(cant)?;
    if let Err(e) = std::fs::copy(&file, &exe) {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        return Err(cant(e));
    }
    let _ = std::fs::remove_file(&file);
    if let Err(e) = std::process::Command::new(&exe).arg("--updated").spawn() {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("The new version didn't start ({e}); this one stays."));
    }
    log::info!("updated to DSZ {}; restarting", r.version);
    Ok(())
}

fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
}

/// After an update: delete the exe it replaced. The old process may still
/// be exiting, so this tries for a few seconds on its own thread.
pub fn clean_up() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = old_path(&exe);
    if !old.exists() {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..30 {
            if std::fs::remove_file(&old).is_ok() || !old.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_by_number() {
        assert!(newer("0.3.8", "0.3.7"));
        assert!(newer("0.3.10", "0.3.9"));
        assert!(newer("0.4.0", "0.3.12"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.3.7", "0.3.7"));
        assert!(!newer("0.3.6", "0.3.7"));
    }

    #[test]
    fn reads_the_release_and_its_checksum() {
        let json = r#"{"tag_name":"v0.3.8","html_url":"https://github.com/MaximumFreak/dsz/releases/tag/v0.3.8",
            "assets":[{"name":"other.zip","browser_download_url":"https://example.invalid/x"},
            {"name":"dsz.exe","browser_download_url":"https://github.com/MaximumFreak/dsz/releases/download/v0.3.8/dsz.exe",
             "digest":"sha256:ef4fea4dcb25a386e63ebb4e50f55c89a36a79b4ee02d1652d425325fa4c26c2"}]}"#;
        let r = parse(json).unwrap();
        assert_eq!(r.version, "0.3.8");
        assert!(r.url.ends_with("/v0.3.8/dsz.exe"));
        assert_eq!(r.sha256.unwrap()[..2], [0xef, 0x4f]);
        assert!(parse(r#"{"tag_name":"v1","assets":[]}"#).is_none(), "no exe, no update");
    }

    /// Asks the real GitHub: `cargo test -- --ignored asks_github`.
    #[test]
    #[ignore]
    fn asks_github() {
        let r = latest().unwrap();
        assert!(r.sha256.is_some(), "{r:?}");
        println!("latest {} at {}", r.version, r.url);
        let f = fetch(&r).unwrap();
        let _ = std::fs::remove_file(f);
        let mut bad = r.clone();
        bad.sha256 = Some([0; 32]);
        assert!(fetch(&bad).is_err(), "a wrong checksum is refused");
    }

    #[test]
    fn old_exe_sits_beside_the_new_one() {
        assert_eq!(
            old_path(Path::new(r"C:\Apps\dsz.exe")),
            PathBuf::from(r"C:\Apps\dsz.exe.old")
        );
    }
}
