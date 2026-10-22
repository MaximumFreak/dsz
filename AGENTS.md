# Working on DSZ

Notes for anyone changing DSZ, people and coding agents alike. README.md describes what the app does; this file is about how changes get in.

## Layout

- `crates/ds-proto`: controller protocol only, no Windows code. Input parsing (`input.rs`), the output report (`output.rs`), the Bluetooth haptics and speaker reports (`stream.rs`), CRCs, trigger effects, virtual-pad report formats.
- `crates/dsz`: the app. `device.rs` runs one controller (reader, writer, threads), `pipeline.rs` turns input into mappings and pad state, `composer.rs` builds the output state from the profile, `haptics/` is the Bluetooth audio stream (haptics, speaker, microphone), `virt/` the virtual controllers (`virt/usbip/` is the USB/IP server with the USB descriptors), `profile.rs` and `settings.rs` the saved JSON, `ui/` the egui pages, `i18n.rs` plus `locales/` the translations.
- README.md is the only documentation. New user-facing behavior goes into its feature table; new protocol sources go into its Protocol sources table.

## Build and check

- Windows with the MSVC Rust toolchain. `cargo test --workspace --locked` must pass, with no new compiler warnings.
- The repo isn't rustfmt-formatted. Don't run rustfmt over files; format only the lines you change, like the code around them.
- Debug builds can render a page to a bitmap without opening controllers: `DSZ_SNAPSHOT=<file.bmp>` with `--page <name>`, and `DSZ_DATA_DIR=<folder>` for a separate settings folder. Use this to check UI changes, including in Portuguese.

## Hardware rules

The reports DSZ sends were tuned on real controllers. Treat their bytes as fixed.

- A new feature is off by default. Until the user turns it on, every report DSZ sends (`0x31` output, `0x32` haptics, `0x39` speaker, the USB/IP descriptors and replies) must stay byte for byte what it was. Add a test that pins those bytes.
- Don't add radio traffic for people who don't use a feature: no periodic reports while idle, no extra threads waking the controller.
- Controller packets are untrusted until checked. Never let a packet reach input injection unless its type and CRC are verified. While the speaker stream runs, the controller sends microphone packets that look like input reports; once they were read as touches and took over the PC.
- Test protocol changes with `--probe-virtual` (no input injection) before running the full app. Steam reads the controller's raw HID: close it, or hide the controller from it, before probing the speaker or microphone.
- Say in the PR what you tested on hardware, which controller and firmware, and what you couldn't test.

## Sources

Protocol facts come from public documentation, captures, or code with open licenses, cited in README's Protocol sources table. Don't port code or constants from closed-source controller apps, or from projects copied from them.

## Interface text

- Wrap UI strings in `crate::i18n::tr("...")`. Formatted ones use `crate::localized_format!("English {x}", "Português {x}", ...)`, which keeps both templates at the call site so the compiler checks the arguments.
- Every string also goes into `crates/dsz/locales/pt-BR.json`, keyed by the exact English text. Changing the English text changes the key, so update or remove the old entry too; a missing entry shows English.
- Logs, CLI arguments, profile names, file names and anything saved to JSON stay in English and untranslated.
- UI text is plain and short. Say what a control does, not how it works inside.

## Profiles and settings

- Every saved struct is `#[serde(default)]`. A new field's default must keep today's behavior, so older files load unchanged.
- Old profiles aren't migrated: no serde aliases or conversion code for renamed or removed fields. If a change alters what an existing setting does, say so in the PR so it reaches the release notes.

## Pull requests

- One feature per PR, without unrelated changes or reformatting.
- Comments explain why, not what. Match the naming and style of the surrounding code.
- DSZ is released as one commit per version, so a PR is brought in through the release commit rather than merged, and closed with a link to the release that shipped it.
- The app must never download or start executables by itself (antivirus engines flag that). Updates go through the user's browser.
