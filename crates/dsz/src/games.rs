//! Per-game profiles: watch running processes, switch controllers to the
//! game's profile while it runs, and switch back when it exits. The most
//! recently started game wins.
//!
//! Games without a rule are checked once for native DualSense support
//! (`dsdetect`); while one runs, controllers use the DualSense profile.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::engine::Engine;

pub fn run(engine: Arc<Engine>) {
    let mut stack: Vec<String> = Vec::new();
    // Process id -> DualSense support, checked once per process.
    let mut checked: HashMap<u32, Option<(String, String)>> = HashMap::new();
    while !engine.shutdown.load(Ordering::Acquire) {
        let procs = crate::platform::process_list();
        let names: std::collections::HashSet<String> =
            procs.iter().map(|(_, n)| n.clone()).collect();

        let (auto, rules, auto_ds, ds_profile) = {
            let s = engine.settings.read();
            (
                s.auto_profiles,
                s.games.clone(),
                s.auto_dualsense,
                s.dualsense_profile.clone(),
            )
        };
        // DualSense games without a rule. Programs already running when the
        // app starts are checked too, so a game started first still counts.
        let mut ds_games: Vec<(String, String)> = Vec::new();
        if auto && auto_ds {
            checked.retain(|pid, _| procs.iter().any(|(p, _)| p == pid));
            for (pid, name) in &procs {
                if rules
                    .iter()
                    .any(|g| g.enabled && g.exe.trim().eq_ignore_ascii_case(name))
                {
                    continue;
                }
                let hit = checked.entry(*pid).or_insert_with(|| {
                    crate::platform::process_path(*pid).and_then(|path| {
                        engine.dsdetect.check(&path).map(|why| (name.clone(), why))
                    })
                });
                if let Some(h) = hit {
                    ds_games.push(h.clone());
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
        };
        stack.retain(|exe| live(exe));
        for exe in running
            .iter()
            .map(|g| g.exe.trim().to_string())
            .chain(ds_games.iter().map(|(n, _)| n.clone()))
        {
            if !stack.iter().any(|e| e.eq_ignore_ascii_case(&exe)) {
                stack.push(exe);
            }
        }
        let want = if auto {
            stack.last().and_then(|exe| {
                rules
                    .iter()
                    .find(|g| g.enabled && g.exe.trim().eq_ignore_ascii_case(exe))
                    .map(|g| (g.profile.clone(), g.exe.clone()))
                    .or_else(|| {
                        ds_games
                            .iter()
                            .find(|(n, _)| n.eq_ignore_ascii_case(exe))
                            .filter(|_| engine.profiles.exists(&ds_profile))
                            .map(|(n, _)| (ds_profile.clone(), n.clone()))
                    })
            })
        } else {
            None
        };
        // A game the user switched away from keeps its rule off until it closes.
        let want = {
            let mut dismissed = engine.game_dismissed.lock();
            if let Some(d) = dismissed.as_ref() {
                if !stack.iter().any(|e| e.eq_ignore_ascii_case(d)) {
                    *dismissed = None;
                }
            }
            match (&want, dismissed.as_ref()) {
                (Some((_, exe)), Some(d)) if exe.eq_ignore_ascii_case(d) => None,
                _ => want,
            }
        };
        let changed = *engine.game_override.read() != want;
        if changed {
            if let Some((p, exe)) = &want {
                let why = ds_games
                    .iter()
                    .find(|(n, _)| n == exe)
                    .map(|(_, w)| w.clone());
                match why {
                    Some(w) => {
                        log::info!("{exe} supports DualSense ({w}): switching to profile \"{p}\"");
                        engine.toast(format!("{exe} supports DualSense — profile \"{p}\""));
                    }
                    None => {
                        log::info!("{exe} is running: switching to profile \"{p}\"");
                        engine.toast(format!("{exe} detected — profile \"{p}\""));
                    }
                }
            }
            let previous = std::mem::replace(&mut *engine.game_override.write(), want.clone());
            if want.is_none() {
                if let Some((_, exe)) = previous {
                    // The profile each controller had before the game.
                    let back = engine
                        .device_list()
                        .first()
                        .map(|d| engine.assigned_profile(d))
                        .unwrap_or_else(|| engine.settings.read().default_profile.clone());
                    log::info!("{exe} closed: back to profile \"{back}\"");
                    engine.toast(format!("{exe} closed — back to \"{back}\""));
                }
            }
            engine.bump_config();
            for d in engine.device_list() {
                d.identify_light();
            }
        }
        let wait = if rules.is_empty() && !auto_ds {
            3000
        } else {
            1500
        };
        std::thread::sleep(Duration::from_millis(wait));
    }
}
