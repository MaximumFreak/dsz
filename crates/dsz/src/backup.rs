//! Backup and restore of everything a user sets up: every profile and the
//! app settings, in one JSON file.
//!
//! Restoring replaces the current profiles and settings. The current ones
//! are saved first to `backups\` in the data directory, so a restore can
//! always be undone by restoring that file.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::profile::Profile;
use crate::settings::Settings;

/// Marks a file as a DSZ backup.
pub const FORMAT: &str = "dsz-backup";
/// Newest backup layout this build reads and the one it writes.
pub const VERSION: u32 = 1;
/// Folder in the data directory for the automatic backups taken before a
/// restore.
pub const AUTO_DIR: &str = "backups";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Backup {
    pub format: String,
    pub version: u32,
    /// The app version that wrote it.
    #[serde(default)]
    pub app_version: String,
    /// When it was made, `YYYY-MM-DD HH:MM UTC`.
    #[serde(default)]
    pub created: String,
    pub settings: Settings,
    pub profiles: Vec<Profile>,
}

impl Backup {
    pub fn new(settings: Settings, profiles: Vec<Profile>) -> Backup {
        Backup {
            format: FORMAT.to_string(),
            version: VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            created: format!("{} UTC", utc_stamp(SystemTime::now(), "-", " ", ":")),
            settings,
            profiles,
        }
    }

    /// Read and check a backup file.
    pub fn parse(text: &str) -> Result<Backup, String> {
        let text = text.trim_start_matches(crate::settings::BOM);
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| crate::localized_format!("This file isn't valid JSON ({e}).", "Este arquivo não contém JSON válido ({e}).", "Este archivo no contiene JSON válido ({e})."))?;
        if v.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
            if serde_json::from_value::<Profile>(v.clone()).is_ok() && v.get("name").is_some() {
                return Err(crate::i18n::tr("This is a single profile, not a full backup. Use \"Import profile…\" on the Profiles page.").into());
            }
            return Err(crate::i18n::tr("This file isn't a DSZ backup.").into());
        }
        let b: Backup =
            serde_json::from_value(v).map_err(|e| crate::localized_format!("This backup is damaged ({e}).", "Este backup está danificado ({e}).", "Esta copia de seguridad está dañada ({e})."))?;
        if b.version > VERSION {
            return Err(crate::localized_format!(
                "This backup was made by a newer DSZ ({}). Update DSZ to restore it.", "Este backup foi criado em uma versão mais recente do DSZ ({}). Atualize para restaurá-lo.", "Esta copia se creó con una versión más reciente de DSZ ({}). Actualiza DSZ para restaurarla.",
                if b.app_version.is_empty() { crate::i18n::tr("unknown version") } else { &b.app_version }
            ));
        }
        if b.profiles.is_empty() {
            return Err(crate::i18n::tr("This backup has no profiles in it.").into());
        }
        for (i, p) in b.profiles.iter().enumerate() {
            if p.name.trim().is_empty() {
                return Err(crate::localized_format!("Profile {} in this backup has no name.", "O perfil {} deste backup não tem nome.", "El perfil {} de esta copia no tiene nombre.", i + 1));
            }
            if b.profiles[..i]
                .iter()
                .any(|q| q.name.eq_ignore_ascii_case(&p.name))
            {
                return Err(crate::localized_format!("This backup has two profiles named \"{}\".", "Este backup contém dois perfis chamados \"{}\".", "Esta copia tiene dos perfiles llamados \"{}\".", p.name));
            }
        }
        Ok(b)
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }

    /// One line for the confirmation: when, which version, how much.
    pub fn summary(&self) -> String {
        let count = self.profiles.len();
        let mut s = if count == 1 {
            crate::localized_format!("{} profile", "{} perfil", "{} perfil", count)
        } else {
            crate::localized_format!("{} profiles", "{} perfis", "{} perfiles", count)
        };
        s.push_str(crate::i18n::tr(" and app settings"));
        if !self.created.is_empty() {
            s.push_str(&crate::localized_format!(", made {}", ", criado em {}", ", creada el {}", self.created));
        }
        if !self.app_version.is_empty() {
            s.push_str(&crate::localized_format!(" by DSZ {}", " pelo DSZ {}", " con DSZ {}", self.app_version));
        }
        s
    }
}

/// `YYYY{d}MM{d}DD{sep}HH{t}MM` in UTC (the app keeps no time zone).
fn utc_stamp(t: SystemTime, d: &str, sep: &str, tm: &str) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}{d}{m:02}{d}{day:02}{sep}{:02}{tm}{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// File name offered when saving a backup.
pub fn suggested_name() -> String {
    format!("DSZ-backup-{}.json", utc_stamp(SystemTime::now(), "-", "_", ""))
}

/// Everything as it stands now.
pub fn current(engine: &Engine) -> Backup {
    Backup::new(
        engine.settings.read().clone(),
        engine.profiles.all().iter().map(|p| (**p).clone()).collect(),
    )
}

/// Write a backup of everything to `path`, through a temp file.
pub fn export(engine: &Engine, path: &Path) -> Result<(), String> {
    write(&current(engine), path)
}

fn write(b: &Backup, path: &Path) -> Result<(), String> {
    let text = b.to_json()?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Read and check the backup at `path`.
pub fn read(path: &Path) -> Result<Backup, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Couldn't read the file ({e})."))?;
    Backup::parse(&text)
}

/// Replace every profile and the app settings with `b`. The current ones
/// are saved to `backups\` first; nothing changes if that fails. Returns
/// where they went.
pub fn restore(engine: &Engine, b: &Backup) -> Result<PathBuf, String> {
    let dir = engine.data_dir.join(AUTO_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Couldn't save the current settings first ({e}); nothing was changed."))?;
    // Never over an earlier one: that may hold the setup from before a
    // restore that turned out wrong.
    let stamp = utc_stamp(SystemTime::now(), "-", "_", "");
    let mut safety = dir.join(format!("before-restore-{stamp}.json"));
    for n in 2.. {
        if !safety.exists() {
            break;
        }
        safety = dir.join(format!("before-restore-{stamp}-{n}.json"));
    }
    write(&current(engine), &safety)
        .map_err(|e| format!("Couldn't save the current settings first ({e}); nothing was changed."))?;
    log::info!(
        "backup: current settings saved to {}; restoring {}",
        safety.display(),
        b.summary()
    );

    let mut s = b.settings.clone();
    keep_learned(&mut s, &engine.settings.read());
    // Windows' startup entry lives outside settings.json; follow the
    // backup only when it can be changed.
    let was_auto = engine.settings.read().start_with_windows;
    if s.start_with_windows != was_auto && !crate::platform::set_autostart(s.start_with_windows) {
        log::warn!("backup: couldn't change the startup entry; keeping it as it was");
        s.start_with_windows = was_auto;
    }
    engine.profiles.replace_all(b.profiles.clone());
    if !engine.profiles.exists(&s.default_profile) {
        s.default_profile = engine.profiles.fallback_name();
    }
    *engine.settings.write() = s;
    engine.settings_changed();
    engine.flush();
    Ok(safety)
}

/// What DSZ learned about this PC rather than what the user set: device
/// nodes it hides, the last controller, and the audio defaults chosen for
/// the virtual DualSense. A backup from another PC, or an older one,
/// would bring back node ids and choices that no longer match.
fn keep_learned(s: &mut Settings, now: &Settings) {
    s.last_controller = now.last_controller.clone();
    s.hide_nodes = now.hide_nodes.clone();
    s.helper_nodes = now.helper_nodes.clone();
    s.audio_on_virtual = now.audio_on_virtual;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_keeps_what_was_learned_here() {
        let mut s = Settings {
            udp_port: 7001,
            last_controller: "old".into(),
            ..Default::default()
        };
        s.hide_nodes.insert("old".into(), vec!["HID\\OLD".into()]);
        let mut now = Settings {
            last_controller: "new".into(),
            audio_on_virtual: [true; 6],
            ..Default::default()
        };
        now.helper_nodes.insert("DSZV1".into(), vec!["HID\\NEW".into()]);
        keep_learned(&mut s, &now);
        assert_eq!(s.udp_port, 7001);
        assert_eq!(s.last_controller, "new");
        assert!(s.hide_nodes.is_empty());
        assert_eq!(s.helper_nodes, now.helper_nodes);
        assert_eq!(s.audio_on_virtual, [true; 6]);
    }

    fn sample() -> Backup {
        let s = Settings {
            udp_port: 6970,
            ..Default::default()
        };
        Backup::new(s, Profile::starter_set())
    }

    #[test]
    fn round_trips() {
        let b = sample();
        let back = Backup::parse(&b.to_json().unwrap()).unwrap();
        assert_eq!(back, b);
        assert_eq!(back.settings.udp_port, 6970);
        assert!(b.summary().starts_with("2 profiles and app settings"));
    }

    #[test]
    fn accepts_a_bom_and_missing_optional_fields() {
        let b = sample();
        let mut v = serde_json::to_value(&b).unwrap();
        v.as_object_mut().unwrap().remove("app_version");
        v.as_object_mut().unwrap().remove("created");
        let text = format!("\u{feff}{v}");
        assert_eq!(Backup::parse(&text).unwrap().profiles, b.profiles);
    }

    #[test]
    fn rejects_what_isnt_a_backup() {
        assert!(Backup::parse("not json").unwrap_err().contains("valid JSON"));
        assert!(Backup::parse("{}").unwrap_err().contains("isn't a DSZ backup"));
        let profile = serde_json::to_string(&Profile::starter_set()[0]).unwrap();
        assert!(Backup::parse(&profile).unwrap_err().contains("single profile"));
    }

    #[test]
    fn rejects_bad_backups() {
        let mut b = sample();
        b.version = VERSION + 1;
        assert!(Backup::parse(&b.to_json().unwrap()).unwrap_err().contains("newer DSZ"));

        let mut b = sample();
        b.profiles.clear();
        assert!(Backup::parse(&b.to_json().unwrap()).unwrap_err().contains("no profiles"));

        let mut b = sample();
        b.profiles[1].name = b.profiles[0].name.to_uppercase();
        assert!(Backup::parse(&b.to_json().unwrap()).unwrap_err().contains("two profiles"));

        let mut b = sample();
        b.profiles[0].name = "  ".into();
        assert!(Backup::parse(&b.to_json().unwrap()).unwrap_err().contains("no name"));

        let mut v = serde_json::to_value(sample()).unwrap();
        v["profiles"] = serde_json::json!("oops");
        assert!(Backup::parse(&v.to_string()).unwrap_err().contains("damaged"));
    }

    #[test]
    fn export_then_restore_puts_everything_back() {
        let dir = std::env::temp_dir().join(format!("dsz-backup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let engine = Engine::new(dir.join("data"));
        engine.settings.write().udp_port = 7001;
        engine.profiles.add(Profile::named("Racing"));
        let file = dir.join(suggested_name());
        export(&engine, &file).unwrap();

        // Change things after the backup.
        engine.settings.write().udp_port = 7002;
        engine.settings.write().default_profile = "Gone".into();
        engine.profiles.replace_all(vec![Profile::named("Gone")]);
        engine.flush();

        let b = read(&file).unwrap();
        let saved = restore(&engine, &b).unwrap();
        let saved_again = restore(&engine, &b).unwrap();
        assert_ne!(saved, saved_again, "a second restore keeps the first safety copy");
        assert_eq!(engine.settings.read().udp_port, 7001);
        assert_eq!(engine.profiles.names(), ["Xbox", "DualSense", "Racing"]);
        assert!(engine.profiles.exists(&engine.settings.read().default_profile));
        // What was there before the restore is kept.
        let before = read(&saved).unwrap();
        assert_eq!(before.settings.udp_port, 7002);
        assert_eq!(before.profiles.len(), 1);
        assert!(saved.starts_with(dir.join("data").join(AUTO_DIR)));
        // And it is on disk, not only in memory.
        let disk = crate::settings::Settings::load(&dir.join("data"));
        assert_eq!(disk.udp_port, 7001);
        let store = crate::profile::ProfileStore::load(&dir.join("data").join("profiles"));
        let mut names = store.names();
        names.sort();
        assert_eq!(names, ["DualSense", "Racing", "Xbox"]);
        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_745), (2026, 10, 19));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(20_745 * 86_400 + 13 * 3600 + 5 * 60);
        assert_eq!(utc_stamp(t, "-", " ", ":"), "2026-10-19 13:05");
        assert_eq!(utc_stamp(t, "-", "_", ""), "2026-10-19_1305");
    }
}
