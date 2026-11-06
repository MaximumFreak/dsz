//! Per-game profiles: watch running processes, switch controllers to the
//! game's profile while it runs, and switch back when it exits. The most
//! recently started game wins.
//!
//! Games without a rule are checked once for native DualSense support
//! (`dsdetect`); while one runs, controllers use the DualSense profile.
//! A Steam, GOG, or Xbox app game that does not have that support can
//! instead present the fallback virtual controller from settings.

use crate::settings::FallbackPad;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::engine::Engine;

/// What the newest running game asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GameTarget {
    /// Switch to this profile (a rule, or native DualSense support).
    Profile { profile: String, exe: String },
    /// Keep the current profile and present the fallback virtual controller.
    Fallback { exe: String },
}

impl GameTarget {
    fn exe(&self) -> &str {
        match self {
            GameTarget::Profile { exe, .. } | GameTarget::Fallback { exe } => exe,
        }
    }
}

/// One scan of a process that has no game rule.
struct Checked {
    /// Why it counts as a DualSense game, when it does.
    dualsense: Option<String>,
    /// Installed under Steam, GOG, or the Xbox app.
    likely: bool,
    /// Lowercase directory of the exe, for ignoring a launcher beside a
    /// DualSense game (Elden Ring's `start_protected_game.exe`).
    dir: Option<String>,
}

/// A store install with no DualSense support of its own, and not sitting
/// next to one that has it.
fn is_plain_game(likely: bool, dir: Option<&str>, ds_dirs: &HashSet<String>) -> bool {
    likely && !dir.is_some_and(|d| ds_dirs.contains(d))
}

/// Programs that start with a game without being one: the PlayStation PC
/// SDK's overlay, and crash reporters. Some sit beside Sony's pad library,
/// so they look like DualSense games, and starting after the game they
/// would take its place as the newest game. Lowercase exe names.
const HELPERS: &[&str] = &["pspcsdkui.exe", "pspcsdkappmgr.exe"];

/// Parts of crash reporter names: Crashpad (`crashpad_handler.exe`), Unity
/// (`UnityCrashHandler64.exe`), Unreal (`CrashReportClient.exe`), CrashRpt
/// (`CrashSender1403.exe`), BugSplat (`BsSndRpt64.exe`). Lowercase.
const CRASH_REPORTERS: &[&str] = &[
    "crashpad",
    "crashhandler",
    "crash_handler",
    "crashreport",
    "crashsender",
    "bssndrpt",
];

/// Where the PlayStation PC SDK installs its overlay and helpers, under
/// ProgramData or AppData. Lowercase.
const PSPC_SDK_DIR: &str = "\\sony interactive entertainment inc\\pspc_sdk\\";

/// An overlay or crash reporter that runs alongside a game. It never
/// counts as the game; a rule naming it still applies.
pub fn is_helper(name: &str, path: Option<&str>) -> bool {
    let n = name.to_ascii_lowercase();
    HELPERS.contains(&n.as_str())
        || CRASH_REPORTERS.iter().any(|c| n.contains(c))
        || path.is_some_and(|p| {
            p.to_ascii_lowercase()
                .replace('/', "\\")
                .contains(PSPC_SDK_DIR)
        })
}

/// Scan a newly seen process without a rule. `dualsense` runs the
/// DualSense check; helpers are not checked.
fn check_process(
    name: &str,
    path: Option<std::path::PathBuf>,
    dualsense: impl FnOnce(&std::path::Path) -> Option<String>,
) -> Checked {
    let dir = path
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.to_string_lossy().to_ascii_lowercase().replace('/', "\\"));
    let text = path.as_ref().map(|p| p.to_string_lossy().into_owned());
    if is_helper(name, text.as_deref()) {
        // Debug: every Chromium app (Discord, browsers) starts a crashpad_handler.
        log::debug!(
            "{name} runs alongside games (overlay or crash reporter): not counted as a game"
        );
        return Checked {
            dualsense: None,
            likely: false,
            dir,
        };
    }
    Checked {
        dualsense: path.as_deref().and_then(dualsense),
        likely: text.as_deref().is_some_and(is_likely_game),
        dir,
    }
}

/// `top` is the most recently started game still running.
pub fn pick_target(
    auto: bool,
    top: Option<&str>,
    rules: &[(&str, &str)],
    ds_exes: &[&str],
    ds_profile: &str,
    ds_profile_ok: bool,
    plain_exes: &[&str],
) -> Option<GameTarget> {
    if !auto {
        return None;
    }
    let exe = top?;
    if let Some((_, profile)) = rules.iter().find(|(e, _)| e.eq_ignore_ascii_case(exe)) {
        return Some(GameTarget::Profile {
            profile: (*profile).to_string(),
            exe: exe.to_string(),
        });
    }
    if ds_profile_ok && ds_exes.iter().any(|e| e.eq_ignore_ascii_case(exe)) {
        return Some(GameTarget::Profile {
            profile: ds_profile.to_string(),
            exe: exe.to_string(),
        });
    }
    if plain_exes.iter().any(|e| e.eq_ignore_ascii_case(exe)) {
        return Some(GameTarget::Fallback {
            exe: exe.to_string(),
        });
    }
    None
}

/// Store apps that are not games and often run all the time (a wallpaper,
/// a frame-generation overlay, VR runtime, redistributable installers).
/// Folder names under `steamapps\common\`, lowercase.
const NOT_GAMES: &[&str] = &[
    "wallpaper_engine",
    "lossless scaling",
    "steamvr",
    "steamworks shared",
    "steam controller configs",
    "ovr toolkit",
    "ovr advanced settings",
    "fpsvr",
    "soundpad",
    "voicemod",
];

/// Steam, GOG, and Xbox app installs. Other programs keep the profile's
/// virtual controller; a rule covers those.
pub fn is_likely_game(path: &str) -> bool {
    let p = path.to_ascii_lowercase().replace('/', "\\");
    if p.contains("\\_commonredist\\") {
        return false;
    }
    if let Some(i) = p.find("\\steamapps\\common\\") {
        let rest = &p[i + "\\steamapps\\common\\".len()..];
        let folder = rest.split('\\').next().unwrap_or("");
        return !NOT_GAMES.contains(&folder);
    }
    p.contains("\\gog games\\") || p.contains("\\xboxgames\\")
}

pub fn run(engine: Arc<Engine>) {
    let mut stack: Vec<String> = Vec::new();
    // Process id -> scan result, checked once per process.
    let mut checked: HashMap<u32, Checked> = HashMap::new();
    while !engine.shutdown.load(Ordering::Acquire) {
        let procs = crate::platform::process_list();
        let names: std::collections::HashSet<String> =
            procs.iter().map(|(_, n)| n.clone()).collect();

        let (auto, rules, auto_ds, ds_profile, fallback_pad) = {
            let s = engine.settings.read();
            (
                s.auto_profiles,
                s.games.clone(),
                s.auto_dualsense,
                s.dualsense_profile.clone(),
                s.fallback_pad,
            )
        };
        let fallback_on = matches!(fallback_pad, FallbackPad::Xbox360 | FallbackPad::DualShock4);
        // Games without a rule. Programs already running when the app
        // starts are checked too, so a game started first still counts.
        let mut ds_games: Vec<(String, String)> = Vec::new();
        let mut plain_games: Vec<String> = Vec::new();
        if auto && (auto_ds || fallback_on) {
            checked.retain(|pid, _| procs.iter().any(|(p, _)| p == pid));
            for (pid, name) in &procs {
                if rules
                    .iter()
                    .any(|g| g.enabled && g.exe.trim().eq_ignore_ascii_case(name))
                {
                    continue;
                }
                checked.entry(*pid).or_insert_with(|| {
                    check_process(name, crate::platform::process_path(*pid), |p| {
                        engine.dsdetect.check(p)
                    })
                });
            }
            let ds_dirs: HashSet<String> = checked
                .values()
                .filter(|c| c.dualsense.is_some())
                .filter_map(|c| c.dir.clone())
                .collect();
            for (pid, name) in &procs {
                if rules
                    .iter()
                    .any(|g| g.enabled && g.exe.trim().eq_ignore_ascii_case(name))
                {
                    continue;
                }
                let Some(hit) = checked.get(pid) else {
                    continue;
                };
                if let Some(why) = &hit.dualsense {
                    if auto_ds {
                        ds_games.push((name.clone(), why.clone()));
                    }
                } else if fallback_on && is_plain_game(hit.likely, hit.dir.as_deref(), &ds_dirs) {
                    plain_games.push(name.clone());
                }
            }
        }
        let running: Vec<&crate::settings::GameRule> = rules
            .iter()
            .filter(|g| g.enabled && !g.exe.trim().is_empty())
            .filter(|g| names.contains(&g.exe.trim().to_ascii_lowercase()))
            .collect();
        let live = |exe: &str| {
            running
                .iter()
                .any(|g| g.exe.trim().eq_ignore_ascii_case(exe))
                || ds_games.iter().any(|(n, _)| n.eq_ignore_ascii_case(exe))
                || plain_games.iter().any(|n| n.eq_ignore_ascii_case(exe))
        };
        stack.retain(|exe| live(exe));
        for exe in running
            .iter()
            .map(|g| g.exe.trim().to_string())
            .chain(ds_games.iter().map(|(n, _)| n.clone()))
            .chain(plain_games.iter().cloned())
        {
            if !stack.iter().any(|e| e.eq_ignore_ascii_case(&exe)) {
                stack.push(exe);
            }
        }
        let rules_top: Vec<(&str, &str)> = rules
            .iter()
            .filter(|g| g.enabled)
            .map(|g| (g.exe.trim(), g.profile.as_str()))
            .collect();
        let ds_exes: Vec<&str> = ds_games.iter().map(|(n, _)| n.as_str()).collect();
        let plain_exes: Vec<&str> = plain_games.iter().map(String::as_str).collect();
        let target = pick_target(
            auto,
            stack.last().map(String::as_str),
            &rules_top,
            &ds_exes,
            &ds_profile,
            engine.profiles.exists(&ds_profile),
            &plain_exes,
        );
        // A game the user switched away from keeps its rule off until it closes.
        let target = {
            let mut dismissed = engine.game_dismissed.lock();
            if let Some(d) = dismissed.as_ref() {
                if !stack.iter().any(|e| e.eq_ignore_ascii_case(d)) {
                    *dismissed = None;
                }
            }
            match (&target, dismissed.as_ref()) {
                (Some(t), Some(d)) if t.exe().eq_ignore_ascii_case(d) => None,
                _ => target,
            }
        };
        let (next_override, next_fallback) = match &target {
            Some(GameTarget::Profile { profile, exe }) => {
                (Some((profile.clone(), exe.clone())), None)
            }
            Some(GameTarget::Fallback { exe }) => (None, Some(exe.clone())),
            None => (None, None),
        };
        let prev_override = engine.game_override.read().clone();
        let prev_fallback = engine.fallback_exe.read().clone();
        if next_override != prev_override || next_fallback != prev_fallback {
            match &target {
                Some(GameTarget::Profile { profile, exe }) => {
                    let why = ds_games
                        .iter()
                        .find(|(n, _)| n == exe)
                        .map(|(_, w)| w.clone());
                    match why {
                        Some(w) => {
                            log::info!(
                                "{exe} supports DualSense ({w}): switching to profile \"{profile}\""
                            );
                            engine
                                .toast(format!("{exe} supports DualSense — profile \"{profile}\""));
                        }
                        None => {
                            log::info!("{exe} is running: switching to profile \"{profile}\"");
                            engine.toast(crate::localized_format!("{exe} detected — profile \"{profile}\"", "{exe} detectado — perfil \"{profile}\"", "{exe} detectado — perfil \"{profile}\""));
                        }
                    }
                }
                Some(GameTarget::Fallback { exe }) => {
                    log::info!(
                        "{exe} has no DualSense support: virtual {}",
                        fallback_pad.label()
                    );
                    engine.toast(crate::localized_format!("{exe} — virtual {}", "{exe} — virtual {}", "{exe} — mando virtual {}", fallback_pad.label()));
                }
                None => {
                    let exe = prev_override
                        .as_ref()
                        .map(|(_, e)| e.clone())
                        .or(prev_fallback);
                    if let Some(exe) = exe {
                        let back = engine
                            .device_list()
                            .first()
                            .map(|d| engine.assigned_profile(d))
                            .unwrap_or_else(|| engine.settings.read().default_profile.clone());
                        log::info!("{exe} closed: back to profile \"{back}\"");
                        engine.toast(crate::localized_format!("{exe} closed — back to \"{back}\"", "{exe} fechado — voltando para \"{back}\"", "{exe} cerrado — de vuelta a \"{back}\""));
                    }
                }
            }
            *engine.game_override.write() = next_override;
            *engine.fallback_exe.write() = next_fallback;
            engine.bump_config();
            for d in engine.device_list() {
                d.identify_light();
            }
        }
        let wait = if rules.is_empty() && !auto_ds && !fallback_on {
            3000
        } else {
            1500
        };
        std::thread::sleep(Duration::from_millis(wait));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn likely_games_are_store_installs() {
        assert!(is_likely_game(
            r"C:\Program Files (x86)\Steam\steamapps\common\Dead Cells\deadcells.exe"
        ));
        assert!(is_likely_game(r"D:\GOG Games\Hades\Hades.exe"));
        assert!(is_likely_game(r"E:\XboxGames\Forza\Content\forza.exe"));
        assert!(!is_likely_game(r"C:\Program Files (x86)\Steam\steam.exe"));
        assert!(!is_likely_game(r"C:\Windows\System32\svchost.exe"));
        assert!(!is_likely_game(r"C:\Games\portable\game.exe"));
        // Store apps that run in the background are not games.
        assert!(!is_likely_game(
            r"C:\Program Files (x86)\Steam\steamapps\common\wallpaper_engine\wallpaper64.exe"
        ));
        assert!(!is_likely_game(
            r"D:\SteamLibrary\steamapps\common\SteamVR\bin\win64\vrserver.exe"
        ));
        assert!(!is_likely_game(
            r"D:\SteamLibrary\steamapps\common\Lossless Scaling\LosslessScaling.exe"
        ));
        assert!(!is_likely_game(
            r"D:\SteamLibrary\steamapps\common\Hades\_CommonRedist\vcredist\2019\VC_redist.x64.exe"
        ));
    }

    #[test]
    fn launcher_beside_a_dualsense_game_is_not_a_fallback() {
        let mut dirs = HashSet::new();
        dirs.insert(r"c:\steam\steamapps\common\elden ring".into());
        assert!(!is_plain_game(
            true,
            Some(r"c:\steam\steamapps\common\elden ring"),
            &dirs
        ));
        assert!(is_plain_game(
            true,
            Some(r"c:\steam\steamapps\common\dead cells"),
            &dirs
        ));
        assert!(!is_plain_game(
            false,
            Some(r"c:\steam\steamapps\common\dead cells"),
            &dirs
        ));
    }

    #[test]
    fn overlays_and_crash_reporters_are_helpers() {
        for n in [
            "pspcsdkui.exe",
            "PsPcSdkUI.exe",
            "crashpad_handler.exe",
            "sentry_crashpad_handler.exe",
            "UnityCrashHandler64.exe",
            "UnityCrashHandler32.exe",
            "CrashReportClient.exe",
            "CrashSender1403.exe",
            "BsSndRpt64.exe",
        ] {
            assert!(is_helper(n, None), "{n}");
        }
        assert!(is_helper(
            "pspcsdkupdate.exe",
            Some(r"C:\ProgramData\Sony Interactive Entertainment Inc\PSPC_SDK\S22\3.00.00.13\pspcsdkupdate.exe")
        ));
        // Games, their launchers, and other tools stay as they were.
        for (n, p) in [
            ("GhostOfTsushima.exe", r"D:\SteamLibrary\steamapps\common\Ghost of Tsushima DIRECTOR'S CUT\GhostOfTsushima.exe"),
            ("GoWR.exe", r"D:\SteamLibrary\steamapps\common\God of War Ragnarok\GoWR.exe"),
            ("HorizonForbiddenWest.exe", r"D:\SteamLibrary\steamapps\common\Horizon Forbidden West Complete Edition\HorizonForbiddenWest.exe"),
            ("eldenring.exe", r"C:\Steam\steamapps\common\ELDEN RING\Game\eldenring.exe"),
            ("start_protected_game.exe", r"C:\Steam\steamapps\common\ELDEN RING\Game\start_protected_game.exe"),
            ("deadcells.exe", r"C:\Program Files (x86)\Steam\steamapps\common\Dead Cells\deadcells.exe"),
            ("CrashBandicootNSaneTrilogy.exe", r"D:\SteamLibrary\steamapps\common\Crash Bandicoot N. Sane Trilogy\CrashBandicootNSaneTrilogy.exe"),
            ("steam.exe", r"C:\Program Files (x86)\Steam\steam.exe"),
        ] {
            assert!(!is_helper(n, Some(p)), "{n}");
            assert!(!is_helper(n, None), "{n}");
        }
    }

    #[test]
    fn games_are_checked_as_before() {
        let path = r"D:\SteamLibrary\steamapps\common\Ghost of Tsushima DIRECTOR'S CUT\GhostOfTsushima.exe";
        let c = check_process("ghostoftsushima.exe", Some(path.into()), |_| {
            Some("ships libScePad.dll".into())
        });
        assert_eq!(c.dualsense.as_deref(), Some("ships libScePad.dll"));
        assert!(c.likely);
        assert_eq!(
            c.dir.as_deref(),
            Some(r"d:\steamlibrary\steamapps\common\ghost of tsushima director's cut")
        );
        let c = check_process(
            "deadcells.exe",
            Some(r"C:\Program Files (x86)\Steam\steamapps\common\Dead Cells\deadcells.exe".into()),
            |_| None,
        );
        assert_eq!(c.dualsense, None);
        assert!(c.likely);
        let c = check_process("game.exe", Some(r"C:\Games\portable\game.exe".into()), |_| None);
        assert!(!c.likely);
        let c = check_process("elevated.exe", None, |_| panic!("no path to check"));
        assert_eq!((c.dualsense, c.likely, c.dir), (None, false, None));
    }

    #[test]
    fn helpers_beside_a_game_are_not_games() {
        // Beside Sony's pad library, the overlay and crash handler would
        // read as DualSense games, or as store games without it.
        for (n, p) in [
            ("crashpad_handler.exe", r"D:\SteamLibrary\steamapps\common\God of War Ragnarok\crashpad_handler.exe"),
            ("pspcsdkui.exe", r"C:\ProgramData\Sony Interactive Entertainment Inc\PSPC_SDK\S22\3.00.00.13\PsPcSdkUI.exe"),
            ("unitycrashhandler64.exe", r"C:\Steam\steamapps\common\Some Unity Game\UnityCrashHandler64.exe"),
        ] {
            let c = check_process(n, Some(p.into()), |_| panic!("{n} was checked"));
            assert_eq!(c.dualsense, None, "{n}");
            assert!(!c.likely, "{n}");
        }
    }

    #[test]
    fn newest_game_picks_the_pad() {
        let rules = [("eldenring.exe", "Elden")];
        let ds = ["bg3.exe"];
        let plain = ["deadcells.exe"];
        assert_eq!(
            pick_target(
                false,
                Some("deadcells.exe"),
                &rules,
                &ds,
                "DualSense",
                true,
                &plain
            ),
            None
        );
        assert_eq!(
            pick_target(
                true,
                Some("deadcells.exe"),
                &rules,
                &ds,
                "DualSense",
                true,
                &plain
            ),
            Some(GameTarget::Fallback {
                exe: "deadcells.exe".into()
            })
        );
        assert_eq!(
            pick_target(
                true,
                Some("bg3.exe"),
                &rules,
                &ds,
                "DualSense",
                true,
                &plain
            ),
            Some(GameTarget::Profile {
                profile: "DualSense".into(),
                exe: "bg3.exe".into()
            })
        );
        assert_eq!(
            pick_target(
                true,
                Some("BG3.EXE"),
                &rules,
                &ds,
                "DualSense",
                false,
                &plain
            ),
            None,
            "no DualSense profile to switch to"
        );
        assert_eq!(
            pick_target(
                true,
                Some("eldenring.exe"),
                &rules,
                &ds,
                "DualSense",
                true,
                &plain
            ),
            Some(GameTarget::Profile {
                profile: "Elden".into(),
                exe: "eldenring.exe".into()
            })
        );
        assert_eq!(
            pick_target(
                true,
                Some("chrome.exe"),
                &rules,
                &ds,
                "DualSense",
                true,
                &plain
            ),
            None
        );
    }
}
