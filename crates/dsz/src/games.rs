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

/// Steam, GOG, and Xbox app installs. Other programs keep the profile's
/// virtual controller; a rule covers those.
pub fn is_likely_game(path: &str) -> bool {
    let p = path.to_ascii_lowercase().replace('/', "\\");
    p.contains("\\steamapps\\common\\")
        || p.contains("\\gog games\\")
        || p.contains("\\xboxgames\\")
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
                    let path = crate::platform::process_path(*pid);
                    let dir = path
                        .as_ref()
                        .and_then(|p| p.parent())
                        .map(|d| d.to_string_lossy().to_ascii_lowercase().replace('/', "\\"));
                    Checked {
                        dualsense: path.as_ref().and_then(|p| engine.dsdetect.check(p)),
                        likely: path
                            .as_ref()
                            .is_some_and(|p| is_likely_game(&p.to_string_lossy())),
                        dir,
                    }
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
                            engine.toast(format!("{exe} detected — profile \"{profile}\""));
                        }
                    }
                }
                Some(GameTarget::Fallback { exe }) => {
                    log::info!(
                        "{exe} has no DualSense support: virtual {}",
                        fallback_pad.label()
                    );
                    engine.toast(format!("{exe} — virtual {}", fallback_pad.label()));
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
                        engine.toast(format!("{exe} closed — back to \"{back}\""));
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
