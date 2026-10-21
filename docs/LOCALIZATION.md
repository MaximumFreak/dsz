# Interface languages

DSZ supports English and Brazilian Portuguese. Open **Settings → Language →
Interface language** and choose **Português (Brasil)**. The change applies
immediately, including the tray menu, and is saved in `settings.json` as
`"language": "pt-BR"`. Choose **English** to return to English (`"en"`).

English remains the default. Old settings and backups without a language field
load in English; no existing profiles or device assignments are renamed.

## Contributing translations

`crates/dsz/locales/pt-BR.json` maps the original English display strings to their
Portuguese translations. `i18n::tr` falls back to the original text when a key is
missing. Translation is restricted to display text: CLI page keys (`--page
haptics`, `--page settings`, etc.), profile and protocol identifiers, user names,
file paths and controller data remain unchanged. Raw driver/OS errors, diagnostic
logs and hardware names may still be in their original language.

For static text, use:

```rust
ui.label(crate::i18n::tr("Settings"));
```

For formatted text, keep both literal templates at the call site:

```rust
crate::localized_format!("{n} reports sent", "{n} relatórios enviados")
```

Also keep the template pair in the JSON catalog. Keeping literal format strings
allows Rust to check argument types, named captures and format specifiers in
both languages. Tests verify that compiled format translations match the catalog
and that every placeholder is preserved. Do not translate names of captured
variables or remove an argument. Keep positional arguments in the same order.

The language choice is a display preference and does not touch Bluetooth,
controller profiles, mappings or audio settings. Tests cover legacy settings,
language round trips, English fallback, catalog placeholders and stable English
CLI page keys. Use DSZ's debug snapshot mode with isolated `DSZ_DATA_DIR` folders
to visually check all pages in both languages without opening controllers.
