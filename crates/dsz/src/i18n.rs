//! Display-only localization. Profile identifiers, protocol strings, CLI page
//! keys and user-supplied names never depend on the interface language.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    OnceLock,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "pt-BR")]
    PortugueseBrazil,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::English, Self::PortugueseBrazil];
    pub fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::PortugueseBrazil => "Português (Brasil)",
        }
    }
}

static LANGUAGE: AtomicU8 = AtomicU8::new(0);
static PORTUGUESE: OnceLock<BTreeMap<String, String>> = OnceLock::new();

pub fn set_language(language: Language) {
    LANGUAGE.store(
        u8::from(language == Language::PortugueseBrazil),
        Ordering::Relaxed,
    );
}
pub fn language() -> Language {
    if LANGUAGE.load(Ordering::Relaxed) == 1 {
        Language::PortugueseBrazil
    } else {
        Language::English
    }
}
fn catalog() -> &'static BTreeMap<String, String> {
    PORTUGUESE.get_or_init(|| {
        serde_json::from_str(include_str!("../locales/pt-BR.json"))
            .expect("bundled Portuguese catalog")
    })
}
pub fn translate(language: Language, english: &str) -> &str {
    if language == Language::PortugueseBrazil {
        catalog()
            .get(english)
            .map(String::as_str)
            .unwrap_or(english)
    } else {
        english
    }
}
pub fn tr(english: &str) -> &str {
    translate(language(), english)
}

/// Both format literals stay at the call site so Rust checks arguments,
/// formatting specifiers and implicit named captures in both languages.
#[macro_export]
macro_rules! localized_format {
    ($english:literal, $portuguese:literal $($args:tt)*) => {
        if $crate::i18n::language() == $crate::i18n::Language::PortugueseBrazil {
            format!($portuguese $($args)*)
        } else { format!($english $($args)*) }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let legacy: crate::settings::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.language, Language::English);
    }
    #[test]
    fn language_round_trips_without_renaming_profiles() {
        let value = r#"{"language":"pt-BR","default_profile":"My English Profile"}"#;
        let s: crate::settings::Settings = serde_json::from_str(value).unwrap();
        assert_eq!(s.language, Language::PortugueseBrazil);
        let restored: crate::settings::Settings =
            serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(restored, s);
        assert_eq!(restored.default_profile, "My English Profile");
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
        assert!(catalog().len() > 450);
        for (english, portuguese) in catalog() {
            assert!(!portuguese.trim().is_empty(), "{english}");
            assert_eq!(placeholders(english), placeholders(portuguese), "{english}");
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
            include_str!("games.rs"),
            include_str!("pipeline.rs"),
            include_str!("virt/mod.rs"),
            include_str!("engine.rs"),
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
            for call in file.split("crate::localized_format!(").skip(1) {
                let (english, rest) = literal(call);
                let (portuguese, _) = literal(rest);
                assert_eq!(catalog().get(&english), Some(&portuguese), "{english}");
                checked += 1;
            }
        }
        assert!(checked >= 20);
    }
}
