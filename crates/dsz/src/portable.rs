//! Portable mode: settings beside dsz.exe.
//!
//! A `dsz-data` folder next to the exe holds the settings, profiles and
//! log in place of `%APPDATA%\DSZ`; making the folder by hand is enough.
//! Settings switches between the two: it copies everything over (a trial
//! copy first, so a folder that can't be written is caught while nothing
//! has changed), quits, copies again what quitting saved, swaps the
//! folders, and starts DSZ again with `--moved`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::engine::Engine;

pub const FOLDER: &str = "dsz-data";

/// `dsz-data` beside the exe, used when it exists.
pub fn folder() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join(FOLDER))
}

/// The folder used when not portable.
pub fn roaming() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        .join("DSZ")
}

/// Whether `data_dir` is the portable folder.
pub fn is_portable(data_dir: &Path) -> bool {
    folder().is_some_and(|f| f == data_dir)
}

/// `DSZ_DATA_DIR` (test runs) overrides both, so there is nothing to switch.
pub fn switchable() -> bool {
    std::env::var_os("DSZ_DATA_DIR").is_none()
}

fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    dir.with_file_name(name)
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), &dest)?;
        }
    }
    Ok(())
}

/// A fresh copy of `from` at `to`'s side, to be renamed into place.
fn stage(from: &Path, to: &Path) -> io::Result<PathBuf> {
    let part = sibling(to, ".part");
    let _ = std::fs::remove_dir_all(&part);
    copy_dir(from, &part).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&part);
    })?;
    Ok(part)
}

/// The move itself, once DSZ has quit: a fresh copy put in `to`'s place,
/// then `from` removed. On an error `from` is left as it was.
fn relocate(from: &Path, to: &Path, portable: bool) -> io::Result<()> {
    let moved = (|| -> io::Result<()> {
        let part = stage(from, to)?;
        if to.exists() {
            std::fs::remove_dir_all(to)?;
        }
        std::fs::rename(&part, to)
    })();
    match &moved {
        // A leftover portable folder would still be picked up, so it must
        // at least lose its name.
        Ok(()) => {
            if std::fs::remove_dir_all(from).is_err() && !portable {
                let _ = std::fs::rename(from, sibling(from, ".old"));
            }
        }
        Err(_) => {
            let _ = std::fs::remove_dir_all(sibling(to, ".part"));
        }
    }
    moved
}

/// Move the settings beside the exe (`on`) or back to `%APPDATA%`, then
/// restart. An error means nothing changed and DSZ keeps running.
pub fn switch(engine: Arc<Engine>, on: bool) -> Result<(), String> {
    let from = engine.data_dir.clone();
    let to = if on {
        folder().ok_or("DSZ can't tell where its exe is.")?
    } else {
        roaming()
    };
    if from == to {
        return Ok(());
    }
    engine.flush();
    stage(&from, &to).map_err(|e| {
        format!(
            "Couldn't copy the settings to {} ({e}).",
            to.parent().unwrap_or(&to).display()
        )
    })?;
    std::thread::spawn(move || {
        engine.shutdown();
        crate::applog::close();
        let _ = relocate(&from, &to, on);
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("--moved").spawn();
        }
        std::process::exit(0);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_a_full_copy() {
        let root = std::env::temp_dir().join("dsz-portable-test");
        let _ = std::fs::remove_dir_all(&root);
        let (from, to) = (root.join("a"), root.join(FOLDER));
        std::fs::create_dir_all(from.join("profiles")).unwrap();
        std::fs::write(from.join("settings.json"), "{}").unwrap();
        std::fs::write(from.join("profiles").join("p.json"), "[]").unwrap();
        // A stale half copy is replaced, not merged into.
        std::fs::create_dir_all(root.join("dsz-data.part")).unwrap();
        std::fs::write(root.join("dsz-data.part").join("stale"), "").unwrap();
        let part = stage(&from, &to).unwrap();
        assert_eq!(part, root.join("dsz-data.part"));
        assert!(part.join("profiles").join("p.json").is_file());
        assert!(part.join("settings.json").is_file());
        assert!(!part.join("stale").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn moves_there_and_back() {
        let root = std::env::temp_dir().join("dsz-portable-move-test");
        let _ = std::fs::remove_dir_all(&root);
        let (roaming, exe_dir) = (root.join("Roaming").join("DSZ"), root.join("app"));
        let portable = exe_dir.join(FOLDER);
        std::fs::create_dir_all(roaming.join("profiles")).unwrap();
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::write(roaming.join("settings.json"), "1").unwrap();
        std::fs::write(roaming.join("profiles").join("p.json"), "[]").unwrap();
        // The log stays open while DSZ quits.
        let log = std::fs::File::create(roaming.join("dsz.log")).unwrap();

        relocate(&roaming, &portable, true).unwrap();
        assert!(!roaming.exists(), "nothing is left in AppData");
        assert_eq!(std::fs::read_to_string(portable.join("settings.json")).unwrap(), "1");
        assert!(portable.join("profiles").join("p.json").is_file());
        drop(log);

        // And back, over a stale AppData copy.
        std::fs::create_dir_all(&roaming).unwrap();
        std::fs::write(roaming.join("old-only.json"), "").unwrap();
        std::fs::write(portable.join("settings.json"), "2").unwrap();
        relocate(&portable, &roaming, false).unwrap();
        assert!(!portable.exists(), "the portable folder is gone, so it isn't picked up");
        assert_eq!(std::fs::read_to_string(roaming.join("settings.json")).unwrap(), "2");
        assert!(!roaming.join("old-only.json").exists(), "the stale copy was replaced");

        // A target that can't be written leaves everything where it was.
        std::fs::write(root.join("blocker"), "").unwrap();
        assert!(relocate(&roaming, &root.join("blocker").join(FOLDER), true).is_err());
        assert_eq!(std::fs::read_to_string(roaming.join("settings.json")).unwrap(), "2");
        let _ = std::fs::remove_dir_all(&root);
    }
}
