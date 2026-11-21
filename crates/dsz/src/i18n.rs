//! Display-only localization. Profile identifiers, protocol strings, CLI page
//! keys and user-supplied names never depend on the interface language.
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

include!(concat!(env!("OUT_DIR"), "/i18n_table.rs"));

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "pt-BR")]
    PortugueseBrazil,
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "zh-CN")]
    ChineseSimplified,
}

impl Language {
    pub const ALL: [Self; 4] = [Self::English, Self::PortugueseBrazil, Self::Spanish, Self::ChineseSimplified];
    pub fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::PortugueseBrazil => "Português (Brasil)",
            Self::Spanish => "Español",
            Self::ChineseSimplified => "简体中文",
        }
    }
}

static LANGUAGE: AtomicU8 = AtomicU8::new(0);

pub fn set_language(language: Language) {
    LANGUAGE.store(
        match language {
            Language::English => 0,
            Language::PortugueseBrazil => 1,
            Language::Spanish => 2,
            Language::ChineseSimplified => 3,
        },
        Ordering::Relaxed,
    );
}
pub fn language() -> Language {
    match LANGUAGE.load(Ordering::Relaxed) {
        1 => Language::PortugueseBrazil,
        2 => Language::Spanish,
        3 => Language::ChineseSimplified,
        _ => Language::English,
    }
}
pub fn translate(language: Language, english: &str) -> &str {
    let table = match language {
        Language::English => return english,
        Language::PortugueseBrazil => PT_BR,
        Language::Spanish => ES,
        Language::ChineseSimplified => ZH_CN,
    };
    KEYS.binary_search(&english)
        .map(|i| table[i])
        .unwrap_or(english)
}
pub fn tr(english: &str) -> &str {
    translate(language(), english)
}

#[inline(never)]
pub fn fmt(args: std::fmt::Arguments<'_>) -> String {
    std::fmt::format(args)
}

/// Format literals stay at the call site so Rust checks arguments,
/// formatting specifiers and implicit named captures in every language.
#[macro_export]
macro_rules! localized_format {
    ($english:literal, $portuguese:literal, $spanish:literal, $chinese:literal $($args:tt)*) => {
        match $crate::i18n::language() {
            $crate::i18n::Language::PortugueseBrazil => $crate::i18n::fmt(format_args!($portuguese $($args)*)),
            $crate::i18n::Language::Spanish => $crate::i18n::fmt(format_args!($spanish $($args)*)),
            $crate::i18n::Language::ChineseSimplified => $crate::i18n::fmt(format_args!($chinese $($args)*)),
            $crate::i18n::Language::English => $crate::i18n::fmt(format_args!($english $($args)*)),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn catalog(language: Language) -> Option<BTreeMap<String, String>> {
        let json = match language {
            Language::English => return None,
            Language::PortugueseBrazil => include_str!("../locales/pt-BR.json"),
            Language::Spanish => include_str!("../locales/es.json"),
            Language::ChineseSimplified => include_str!("../locales/zh-CN.json"),
        };
        Some(serde_json::from_str(json).expect("bundled translation catalog"))
    }
    #[test]
    fn english_is_preserved_and_missing_strings_fall_back() {
        assert_eq!(translate(Language::English, "Settings"), "Settings");
        assert_eq!(
            translate(Language::PortugueseBrazil, "Settings"),
            "Configurações"
        );
        assert_eq!(
            translate(Language::PortugueseBrazil, "user supplied profile"),
            "user supplied profile"
        );
        assert_eq!(translate(Language::Spanish, "Settings"), "Configuración");
        assert_eq!(translate(Language::Spanish, "user supplied profile"), "user supplied profile");
        assert_eq!(translate(Language::ChineseSimplified, "Settings"), "设置");
        assert_eq!(translate(Language::ChineseSimplified, "user supplied profile"), "user supplied profile");
        let legacy: crate::settings::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.language, Language::English);
    }
    #[test]
    fn language_round_trips_without_renaming_profiles() {
        for (id, language) in [("en", Language::English), ("pt-BR", Language::PortugueseBrazil), ("es", Language::Spanish), ("zh-CN", Language::ChineseSimplified)] {
            let value = format!(r#"{{"language":"{id}","default_profile":"My English Profile"}}"#);
            let s: crate::settings::Settings = serde_json::from_str(&value).unwrap();
            assert_eq!(s.language, language);
            let restored: crate::settings::Settings =
                serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(restored, s);
            assert_eq!(restored.default_profile, "My English Profile");
            assert_eq!(serde_json::to_string(&language).unwrap(), format!("\"{id}\""));
        }
    }
    fn placeholders(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut tail = text;
        while let Some(start) = tail.find('{') {
            tail = &tail[start + 1..];
            if tail.starts_with('{') {
                tail = &tail[1..];
                continue;
            }
            let end = tail.find('}').expect("closed format placeholder");
            out.push(&tail[..end]);
            tail = &tail[end + 1..];
        }
        out.sort_unstable();
        out
    }
    #[test]
    fn catalog_preserves_every_format_argument() {
        let portuguese = catalog(Language::PortugueseBrazil).unwrap();
        let spanish = catalog(Language::Spanish).unwrap();
        assert!(spanish.len() > 650);
        assert_eq!(portuguese.keys().collect::<Vec<_>>(), spanish.keys().collect::<Vec<_>>());
        let chinese = catalog(Language::ChineseSimplified).unwrap();
        assert_eq!(portuguese.keys().collect::<Vec<_>>(), chinese.keys().collect::<Vec<_>>());
        for language in [Language::PortugueseBrazil, Language::Spanish, Language::ChineseSimplified] {
            for (english, translated) in catalog(language).unwrap() {
                assert!(!translated.trim().is_empty(), "{english}");
                assert_eq!(placeholders(&english), placeholders(&translated), "{language:?}: {english}");
                assert_eq!(translate(language, &english), translated, "{language:?}: {english}");
            }
        }
    }

    #[test]
    fn named_keys_and_trigger_presets_have_translations() {
        let mut names: Vec<String> = ds_proto::TriggerPreset::ALL.iter().map(|p| p.name().to_string()).collect();
        for vk in crate::inputsim::key_catalog() {
            let name = crate::inputsim::key_name(vk);
            if name.len() == 1 || name.starts_with("Num ") || matches!(name.as_str(), "Esc" | "Ctrl" | "Alt" | "Win")
                || name.strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok()) {
                continue;
            }
            names.push(name);
        }
        for language in [Language::PortugueseBrazil, Language::Spanish, Language::ChineseSimplified] {
            for name in &names {
                assert!(catalog(language).unwrap().contains_key(name), "{language:?}: {name}");
            }
        }
    }

    #[test]
    fn compiled_format_translations_match_the_catalog() {
        fn literal(source: &str) -> (String, &str) {
            let source = source.trim_start_matches(|c: char| c.is_whitespace() || c == ',');
            assert!(source.starts_with('"'));
            let mut escaped = false;
            for (i, c) in source.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                    continue;
                }
                if c == '\\' {
                    escaped = true;
                    continue;
                }
                if c == '"' {
                    return (
                        serde_json::from_str(&source[..=i]).unwrap(),
                        &source[i + 1..],
                    );
                }
            }
            panic!("unterminated translation literal")
        }
        let files = [
            include_str!("backup.rs"),
            include_str!("profile.rs"),
            include_str!("main.rs"),
            include_str!("update.rs"),
            include_str!("games.rs"),
            include_str!("pipeline.rs"),
            include_str!("virt/mod.rs"),
            include_str!("engine.rs"),
            include_str!("device.rs"),
            include_str!("ui/mod.rs"),
            include_str!("ui/pages/haptics.rs"),
            include_str!("ui/pages/input.rs"),
            include_str!("ui/pages/lighting.rs"),
            include_str!("ui/pages/mappings.rs"),
            include_str!("ui/pages/overview.rs"),
            include_str!("ui/pages/profiles.rs"),
            include_str!("ui/pages/settings.rs"),
            include_str!("ui/pages/triggers.rs"),
            include_str!("ui/pages/virtual_pad.rs"),
        ];
        let mut checked = 0;
        for file in files {
            for call in file.split("crate::i18n::tr(").skip(1) {
                if call.trim_start().starts_with('"') {
                    let (english, _) = literal(call);
                    for language in [Language::PortugueseBrazil, Language::Spanish, Language::ChineseSimplified] {
                        assert!(catalog(language).unwrap().contains_key(&english), "{language:?}: {english}");
                    }
                }
            }
            for call in file.split("crate::localized_format!(").skip(1) {
                let (english, rest) = literal(call);
                let (portuguese, rest) = literal(rest);
                let (spanish, rest) = literal(rest);
                let (chinese, _) = literal(rest);
                assert_eq!(translate(Language::PortugueseBrazil, &english), portuguese, "{english}");
                assert_eq!(translate(Language::Spanish, &english), spanish, "{english}");
                assert_eq!(translate(Language::ChineseSimplified, &english), chinese, "{english}");
                checked += 1;
            }
        }
        assert!(checked >= 20);
    }
}
