//! Update check and update from the GitHub releases.
//!
//! Once a day (setting `check_updates`, on by default) the app asks
//! GitHub's API for the latest release. A newer one is announced in the
//! window, in the tray, and on Settings. Nothing installs by itself: an
//! update restarts the app, which unplugs the virtual controllers, so it
//! waits for a click.
//!
//! DSZ never downloads a program itself: an app that fetches an exe and
//! starts it is what antivirus heuristics call a downloader (Kaspersky
//! flagged 0.3.8, which did). "Download" opens the release's `dsz.exe` in
//! the browser, which scans and saves it like any download. DSZ then finds
//! it in the Downloads folder (or is shown where it is), checks it against
//! the SHA-256 GitHub publishes for the file, and on "Install and restart"
//! renames the running exe to `dsz.exe.old` (Windows allows renaming a
//! running exe, not overwriting it), copies the new one into its place,
//! and starts it with `--updated`. The new process waits for this one to
//! quit and deletes the old file.

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
/// How long to watch the Downloads folder after "Download".
const WATCH_FOR: Duration = Duration::from_secs(30 * 60);
const NO_CHECKSUM: &str = "GitHub published no checksum for this download, so it can't be checked. Replace dsz.exe by hand instead.";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    /// The release page, with its notes.
    pub page: String,
    url: String,
    size: Option<u64>,
    sha256: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Checking,
    UpToDate,
    Available(Release),
    /// The browser is downloading it; watching the Downloads folder.
    Waiting(Release),
    /// Downloaded and checked, ready to install.
    Ready(Release, PathBuf),
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
        Some(Status::Available(r) | Status::Waiting(r) | Status::Ready(r, _)) => Some(r),
        _ => None,
    }
}

fn set(s: Status) {
    *state().lock() = Some(s);
}

/// Whether an update is under way (between "Download" and the restart).
fn busy() -> bool {
    matches!(
        status(),
        Some(Status::Waiting(_) | Status::Ready(..) | Status::Restarting)
    )
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
        if busy() {
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
    if busy() || status() == Some(Status::Checking) {
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
    let size = asset["size"].as_u64();
    let sha256 = asset["digest"]
        .as_str()
        .and_then(|d| d.strip_prefix("sha256:"))
        .and_then(hex32);
    Some(Release { version, page, url, size, sha256 })
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

/// Where browsers save downloads: the user's Downloads folder.
fn downloads() -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};
    let mut p = std::ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut p) };
    let dir = if hr == 0 && !p.is_null() {
        let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
        Some(PathBuf::from(s))
    } else {
        std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Downloads"))
    };
    unsafe { CoTaskMemFree(p as _) };
    dir
}

/// Whether `file` is `r`'s exe: an exe with GitHub's SHA-256 for it.
fn verify(r: &Release, file: &Path) -> Result<(), String> {
    let Some(want) = r.sha256 else {
        return Err(NO_CHECKSUM.into());
    };
    let data = std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))?;
    if !data.starts_with(b"MZ") || crate::install::sha256(&data)? != want {
        return Err(format!(
            "{} isn't DSZ {} (it doesn't match GitHub's checksum).",
            file.display(),
            r.version
        ));
    }
    Ok(())
}

/// A finished download of `r` in the Downloads folder: a `dsz*.exe` of the
/// right size and checksum (the browser may have named it `dsz (1).exe`).
fn find_download(r: &Release) -> Option<PathBuf> {
    let entries = std::fs::read_dir(downloads()?).ok()?;
    entries.flatten().map(|e| e.path()).find(|p| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        name.starts_with("dsz")
            && name.ends_with(".exe")
            && std::fs::metadata(p).is_ok_and(|m| r.size.map_or(true, |s| m.len() == s))
            && verify(r, p).is_ok()
    })
}

/// Start getting `r`: the browser downloads it, and this watches the
/// Downloads folder until the file is there and checks out. A copy
/// downloaded earlier is used as it is.
pub fn download(r: Release, wake: impl Fn() + Send + 'static) {
    if busy() {
        return;
    }
    if r.sha256.is_none() {
        set(Status::Failed(NO_CHECKSUM.into()));
        return;
    }
    set(Status::Waiting(r.clone()));
    std::thread::spawn(move || {
        let waiting = || matches!(status(), Some(Status::Waiting(ref w)) if *w == r);
        if let Some(f) = find_download(&r) {
            set(Status::Ready(r, f));
            wake();
            return;
        }
        open_download(&r);
        let until = Instant::now() + WATCH_FOR;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_secs(2));
            // Cancelled, or the file was picked by hand.
            if !waiting() {
                return;
            }
            if let Some(f) = find_download(&r) {
                log::info!("DSZ {} downloaded to {}", r.version, f.display());
                set(Status::Ready(r, f));
                wake();
                return;
            }
        }
        if waiting() {
            set(Status::Available(r));
            wake();
        }
    });
}

/// Have the browser download `r`'s exe.
pub fn open_download(r: &Release) {
    crate::platform::open_url(&r.url);
}

/// The downloaded file, picked by hand (saved somewhere other than
/// Downloads). Checked before it is offered.
pub fn use_file(r: Release, file: PathBuf) -> Result<(), String> {
    verify(&r, &file)?;
    set(Status::Ready(r, file));
    Ok(())
}

/// Stop waiting for the download.
pub fn cancel() {
    if let Some(Status::Waiting(r) | Status::Ready(r, _)) = status() {
        set(Status::Available(r));
    }
}

/// Swap the downloaded `file` in for the running exe, start it, and quit.
pub fn install(r: Release, file: PathBuf, wake: impl Fn() + Send + 'static) {
    if status() == Some(Status::Restarting) {
        return;
    }
    set(Status::Restarting);
    std::thread::spawn(move || {
        match swap_in(&r, &file) {
            Ok(()) => {
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

fn swap_in(r: &Release, file: &Path) -> Result<(), String> {
    // Checked again: the file may have changed since it was found.
    verify(r, file)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    let cant = |e: std::io::Error| {
        format!(
            "DSZ can't replace its own file in {} ({e}). Replace dsz.exe there by hand with {}.",
            exe.parent().unwrap_or(Path::new("")).display(),
            file.display()
        )
    };
    std::fs::rename(&exe, &old).map_err(cant)?;
    if let Err(e) = std::fs::copy(file, &exe) {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        return Err(cant(e));
    }
    if let Err(e) = std::process::Command::new(&exe).arg("--updated").spawn() {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("The new version didn't start ({e}); this one stays."));
    }
    // Used up; left there, the next update's download would be saved as
    // "dsz (1).exe".
    let _ = std::fs::remove_file(file);
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
             "size":9294336,"digest":"sha256:ef4fea4dcb25a386e63ebb4e50f55c89a36a79b4ee02d1652d425325fa4c26c2"}]}"#;
        let r = parse(json).unwrap();
        assert_eq!(r.version, "0.3.8");
        assert!(r.url.ends_with("/v0.3.8/dsz.exe"));
        assert_eq!(r.size, Some(9294336));
        assert_eq!(r.sha256.unwrap()[..2], [0xef, 0x4f]);
        assert!(parse(r#"{"tag_name":"v1","assets":[]}"#).is_none(), "no exe, no update");
    }

    /// Asks the real GitHub: `cargo test -- --ignored asks_github`.
    #[test]
    #[ignore]
    fn asks_github() {
        let r = latest().unwrap();
        assert!(r.sha256.is_some() && r.size.is_some(), "{r:?}");
        println!("latest {} at {}", r.version, r.url);
    }

    #[test]
    fn checks_the_file_against_the_release() {
        let dir = std::env::temp_dir().join("dsz-update-test");
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("dsz.exe");
        std::fs::write(&f, b"MZ fake exe").unwrap();
        let mut r = Release {
            version: "9.9.9".into(),
            page: RELEASES.into(),
            url: String::new(),
            size: Some(11),
            sha256: Some(crate::install::sha256(b"MZ fake exe").unwrap()),
        };
        assert!(verify(&r, &f).is_ok());
        r.sha256 = Some([0; 32]);
        assert!(verify(&r, &f).is_err(), "a wrong checksum is refused");
        r.sha256 = None;
        assert!(verify(&r, &f).is_err(), "no checksum, no install");
        std::fs::write(&f, b"not an exe").unwrap();
        r.sha256 = Some(crate::install::sha256(b"not an exe").unwrap());
        assert!(verify(&r, &f).is_err(), "only an exe");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_the_downloads_folder() {
        assert!(downloads().is_some_and(|d| d.is_absolute()));
    }

    #[test]
    fn old_exe_sits_beside_the_new_one() {
        assert_eq!(
            old_path(Path::new(r"C:\Apps\dsz.exe")),
            PathBuf::from(r"C:\Apps\dsz.exe.old")
        );
    }
}
