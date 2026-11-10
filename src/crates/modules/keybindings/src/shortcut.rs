//! Shortcut spelling: normalization and the Caps, section and ordinary grammars.
//!
//! The grammars are the archived ones. Normalization and the Caps and section
//! parsers are ported from the legacy hotkey module. The ordinary grammar is
//! `global-hotkey` 0.8.0's `HotKey::from_str`, the parser the archived
//! application reached through its global-shortcut plugin. It is used for
//! parsing only: no native manager is ever created here.

use std::fmt;

pub use global_hotkey::hotkey::Code;
use global_hotkey::hotkey::{HotKey, Modifiers};
use stache_sdk::input::ModifierFlags;

/// A physical key plus modifiers for an ordinary Carbon-registrable shortcut.
///
/// The key is a layout-independent `keyboard-types` [`Code`]. Mapping it to a
/// macOS virtual key code is a native concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalChord {
    /// The physical key.
    pub key: Code,
    /// The configured modifiers, in the SDK bit layout.
    pub modifiers: ModifierFlags,
}

/// A macOS virtual key code for a `CapsLock+<key>` binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapsKey(pub u16);

/// The physical ISO section key, the `CapsLock+§` key.
const ISO_SECTION_KEY: u16 = 0x0A;

/// A successfully parsed and routed shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// `CapsLock+<key>`, handled by the Caps Lock owner.
    Caps {
        /// The key held with Caps Lock.
        key: CapsKey,
    },
    /// `[modifiers+]§`. Layout resolution is pending: the physical key and any
    /// layout-required modifiers are resolved later by the native owner.
    Section {
        /// The configured modifiers (Command, Shift, Option, Control only).
        modifiers: ModifierFlags,
    },
    /// Any other shortcut, as a physical chord.
    Ordinary {
        /// The chord.
        chord: PhysicalChord,
    },
}

/// Why one shortcut spelling was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortcutError {
    /// `CapsLock` with no key.
    CapsMissingKey,
    /// `CapsLock` followed by more than one key component.
    CapsUnsupportedShape,
    /// The `CapsLock` key is not supported.
    CapsUnknownKey(String),
    /// `§` must be the last component.
    SectionNotLast,
    /// A component before `§` is not Command, Shift, Option or Control.
    SectionUnknownModifier(String),
    /// The ordinary grammar rejected the spelling.
    Ordinary(String),
}

impl fmt::Display for ShortcutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapsMissingKey => f.write_str("CapsLock shortcut has no key"),
            Self::CapsUnsupportedShape => {
                f.write_str("CapsLock shortcut supports exactly one key and no modifiers")
            }
            Self::CapsUnknownKey(key) => write!(f, "unsupported CapsLock key `{key}`"),
            Self::SectionNotLast => f.write_str("`§` must be the last component"),
            Self::SectionUnknownModifier(modifier) => {
                write!(f, "unknown modifier `{modifier}` before `§`")
            }
            Self::Ordinary(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ShortcutError {}

/// Normalizes aliases per `+`-separated component, exactly as the archive did.
///
/// `Ctrl` becomes `Control`; `Cmd`, `Super` and `Meta` become `Command`; `Alt`
/// and `Opt` become `Option`; a backtick becomes `Backquote`. Matching is
/// case-sensitive and nothing is trimmed here.
#[must_use]
pub fn normalize(shortcut: &str) -> String {
    let mut result = String::with_capacity(shortcut.len() + 8);
    for part in shortcut.split('+') {
        // Legacy separates on output emptiness, so a leading `+` is dropped.
        if !result.is_empty() {
            result.push('+');
        }
        result.push_str(match part {
            "Ctrl" => "Control",
            "Cmd" | "Super" | "Meta" => "Command",
            "Alt" | "Opt" => "Option",
            "`" => "Backquote",
            other => other,
        });
    }
    result
}

/// Normalizes and routes one raw spelling: Caps first, then `§`, then ordinary.
///
/// # Errors
/// Returns a [`ShortcutError`] when the matching grammar rejects the spelling.
pub fn parse(raw: &str) -> Result<Route, ShortcutError> {
    let normalized = normalize(raw);
    if let Some(caps) = parse_caps(&normalized) {
        return caps.map(|key| Route::Caps { key });
    }
    if let Some(section) = parse_section(&normalized) {
        return section.map(|modifiers| Route::Section { modifiers });
    }
    parse_ordinary(&normalized).map(|chord| Route::Ordinary { chord })
}

/// `None` when the spelling is not a Caps shortcut.
pub(crate) fn parse_caps(shortcut: &str) -> Option<Result<CapsKey, ShortcutError>> {
    let mut parts = shortcut.split('+');
    if parts.next() != Some("CapsLock") {
        return None;
    }
    let Some(name) = parts.next() else {
        return Some(Err(ShortcutError::CapsMissingKey));
    };
    if parts.next().is_some() {
        return Some(Err(ShortcutError::CapsUnsupportedShape));
    }
    Some(
        caps_keycode(name)
            .map(CapsKey)
            .ok_or_else(|| ShortcutError::CapsUnknownKey(name.to_owned())),
    )
}

/// `None` when the spelling is not a section shortcut.
fn parse_section(shortcut: &str) -> Option<Result<ModifierFlags, ShortcutError>> {
    let parts: Vec<&str> = shortcut.split('+').collect();
    let (&key, modifiers) = parts.split_last()?;
    if key == "§" {
        let mut flags = ModifierFlags::NONE;
        for modifier in modifiers {
            flags = flags
                | match *modifier {
                    "Command" => ModifierFlags::COMMAND,
                    "Shift" => ModifierFlags::SHIFT,
                    "Option" => ModifierFlags::OPTION,
                    "Control" => ModifierFlags::CONTROL,
                    other => {
                        return Some(Err(ShortcutError::SectionUnknownModifier(other.to_owned())));
                    }
                };
        }
        return Some(Ok(flags));
    }
    parts.contains(&"§").then_some(Err(ShortcutError::SectionNotLast))
}

fn parse_ordinary(shortcut: &str) -> Result<PhysicalChord, ShortcutError> {
    let hotkey: HotKey =
        shortcut.parse().map_err(|error| ShortcutError::Ordinary(format!("{error}")))?;
    let mut modifiers = ModifierFlags::NONE;
    for (from, to) in [
        (Modifiers::SHIFT, ModifierFlags::SHIFT),
        (Modifiers::CONTROL, ModifierFlags::CONTROL),
        (Modifiers::ALT, ModifierFlags::OPTION),
        (Modifiers::SUPER, ModifierFlags::COMMAND),
    ] {
        if hotkey.mods.contains(from) {
            modifiers = modifiers | to;
        }
    }
    Ok(PhysicalChord { key: hotkey.key, modifiers })
}

/// The legacy Caps key table (ANSI virtual key codes), matched case-insensitively.
pub(crate) fn caps_keycode(name: &str) -> Option<u16> {
    Some(match name.to_ascii_uppercase().as_str() {
        "A" => 0,
        "S" => 1,
        "D" => 2,
        "F" => 3,
        "H" => 4,
        "G" => 5,
        "Z" => 6,
        "X" => 7,
        "C" => 8,
        "V" => 9,
        "B" => 11,
        "Q" => 12,
        "W" => 13,
        "E" => 14,
        "R" => 15,
        "Y" => 16,
        "T" => 17,
        "1" | "DIGIT1" => 18,
        "2" | "DIGIT2" => 19,
        "3" | "DIGIT3" => 20,
        "4" | "DIGIT4" => 21,
        "6" | "DIGIT6" => 22,
        "5" | "DIGIT5" => 23,
        "EQUAL" => 24,
        "9" | "DIGIT9" => 25,
        "7" | "DIGIT7" => 26,
        "MINUS" => 27,
        "8" | "DIGIT8" => 28,
        "0" | "DIGIT0" => 29,
        "RIGHTBRACKET" => 30,
        "O" => 31,
        "U" => 32,
        "LEFTBRACKET" => 33,
        "I" => 34,
        "P" => 35,
        "ENTER" | "RETURN" => 36,
        "L" => 37,
        "J" => 38,
        "QUOTE" => 39,
        "K" => 40,
        "SEMICOLON" => 41,
        "BACKSLASH" => 42,
        "COMMA" => 43,
        "SLASH" => 44,
        "N" => 45,
        "M" => 46,
        "PERIOD" => 47,
        "TAB" => 48,
        "SPACE" => 49,
        "BACKQUOTE" | "GRAVE" => 50,
        "§" => ISO_SECTION_KEY,
        "BACKSPACE" | "DELETE" => 51,
        "ESCAPE" => 53,
        "LEFT" | "ARROWLEFT" => 123,
        "RIGHT" | "ARROWRIGHT" => 124,
        "DOWN" | "ARROWDOWN" => 125,
        "UP" | "ARROWUP" => 126,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: ModifierFlags = ModifierFlags::COMMAND;
    const O: ModifierFlags = ModifierFlags::OPTION;
    const K: ModifierFlags = ModifierFlags::CONTROL;
    const S: ModifierFlags = ModifierFlags::SHIFT;

    /// The archived result: `HotKey::from_str` on the legacy-normalized string.
    fn archived(raw: &str) -> Option<PhysicalChord> {
        let hotkey: HotKey = normalize(raw).parse().ok()?;
        let mut modifiers = ModifierFlags::NONE;
        for (from, to) in [
            (Modifiers::SHIFT, S),
            (Modifiers::CONTROL, K),
            (Modifiers::ALT, O),
            (Modifiers::SUPER, C),
        ] {
            if hotkey.mods.contains(from) {
                modifiers = modifiers | to;
            }
        }
        Some(PhysicalChord { key: hotkey.key, modifiers })
    }

    fn ordinary(raw: &str) -> Option<PhysicalChord> {
        match parse(raw) {
            Ok(Route::Ordinary { chord }) => Some(chord),
            _ => None,
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn chord(key: Code, modifiers: ModifierFlags) -> Option<PhysicalChord> {
        Some(PhysicalChord { key, modifiers })
    }

    #[test]
    fn normalization_matches_the_archive() {
        assert_eq!(
            normalize("Ctrl+Cmd+Alt+Opt+Super+Meta+`"),
            "Control+Command+Option+Option+Command+Command+Backquote"
        );
        // Case-sensitive, untrimmed, whole-component only.
        assert_eq!(normalize("ctrl+ Cmd +Alt "), "ctrl+ Cmd +Alt ");
        assert_eq!(normalize("Ctrl+"), "Control+");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("Cmdr+A"), "Cmdr+A");
        // Legacy only separates after nonempty output, so leading `+`s vanish.
        assert_eq!(normalize("+A"), "A");
        assert_eq!(normalize("++Cmd+A"), "Command+A");
        assert_eq!(normalize("A++B"), "A++B");
    }

    #[test]
    fn every_modifier_alias_parses() {
        for (alias, flag) in [
            ("Ctrl", K),
            ("Control", K),
            ("Cmd", C),
            ("Command", C),
            ("Super", C),
            ("Meta", C),
            ("Alt", O),
            ("Opt", O),
            ("Option", O),
            ("Shift", S),
            ("CmdOrCtrl", C),
            ("CommandOrControl", C),
            ("control", K),
            ("COMMAND", C),
        ] {
            let raw = format!("{alias}+A");
            assert_eq!(ordinary(&raw), chord(Code::KeyA, flag), "{raw}");
            assert_eq!(ordinary(&raw), archived(&raw), "{raw}");
        }
        assert_eq!(
            ordinary("Ctrl+Alt+Shift+Cmd+A"),
            chord(Code::KeyA, K | O | S | C)
        );
    }

    #[test]
    fn keys_match_the_archived_parser() {
        let mut keys: Vec<(String, Code)> = Vec::new();
        for letter in 'A'..='Z' {
            let code = ordinary(&format!("Command+{letter}")).expect("letter").key;
            keys.push((letter.to_string(), code));
            keys.push((letter.to_ascii_lowercase().to_string(), code));
            keys.push((format!("Key{letter}"), code));
        }
        for digit in 0..=9 {
            keys.push((
                digit.to_string(),
                ordinary(&digit.to_string()).expect("digit").key,
            ));
            keys.push((
                format!("Digit{digit}"),
                ordinary(&format!("Digit{digit}")).expect("digit").key,
            ));
            keys.push((
                format!("Num{digit}"),
                ordinary(&format!("Num{digit}")).expect("numpad").key,
            ));
            keys.push((
                format!("Numpad{digit}"),
                ordinary(&format!("Numpad{digit}")).expect("numpad").key,
            ));
        }
        for n in 1..=24 {
            keys.push((
                format!("F{n}"),
                ordinary(&format!("Shift+F{n}")).expect("function key").key,
            ));
        }
        // Each spelling must parse in the new path exactly as in the archive.
        for (name, _) in &keys {
            for prefix in ["", "Command+", "Ctrl+Alt+"] {
                let raw = format!("{prefix}{name}");
                assert!(ordinary(&raw).is_some(), "{raw}");
                assert_eq!(ordinary(&raw), archived(&raw), "{raw}");
            }
        }
        for (raw, key) in [
            ("F1", Code::F1),
            ("F12", Code::F12),
            ("F13", Code::F13),
            ("F24", Code::F24),
            ("Up", Code::ArrowUp),
            ("ArrowUp", Code::ArrowUp),
            ("Down", Code::ArrowDown),
            ("Left", Code::ArrowLeft),
            ("ArrowRight", Code::ArrowRight),
            ("Backquote", Code::Backquote),
            ("`", Code::Backquote),
            ("Minus", Code::Minus),
            ("-", Code::Minus),
            ("Equal", Code::Equal),
            ("=", Code::Equal),
            ("Comma", Code::Comma),
            (",", Code::Comma),
            ("Period", Code::Period),
            (".", Code::Period),
            ("Slash", Code::Slash),
            ("/", Code::Slash),
            ("Semicolon", Code::Semicolon),
            (";", Code::Semicolon),
            ("Quote", Code::Quote),
            ("'", Code::Quote),
            ("BracketLeft", Code::BracketLeft),
            ("[", Code::BracketLeft),
            ("BracketRight", Code::BracketRight),
            ("]", Code::BracketRight),
            ("Backslash", Code::Backslash),
            ("\\", Code::Backslash),
            ("Space", Code::Space),
            ("Tab", Code::Tab),
            ("Enter", Code::Enter),
            ("Esc", Code::Escape),
            ("Escape", Code::Escape),
            ("Backspace", Code::Backspace),
            ("Delete", Code::Delete),
            ("Home", Code::Home),
            ("End", Code::End),
            ("PageUp", Code::PageUp),
            ("PageDown", Code::PageDown),
            ("NumpadAdd", Code::NumpadAdd),
            ("NumpadEnter", Code::NumpadEnter),
            ("VolumeUp", Code::AudioVolumeUp),
            ("MediaPlayPause", Code::MediaPlayPause),
        ] {
            assert_eq!(ordinary(raw), chord(key, ModifierFlags::NONE), "{raw}");
            assert_eq!(ordinary(raw), archived(raw), "{raw}");
            let with = format!("Cmd+Shift+{raw}");
            assert_eq!(ordinary(&with), chord(key, C | S), "{with}");
            assert_eq!(ordinary(&with), archived(&with), "{with}");
        }
    }

    #[test]
    fn sample_shortcuts() {
        for (raw, key, modifiers) in [
            ("Command+Control+R", Code::KeyR, C | K),
            ("Command+Control+W", Code::KeyW, C | K),
            ("Command+Control+Shift+W", Code::KeyW, C | K | S),
            ("Command+Control+T", Code::KeyT, C | K),
            ("Command+Control+S", Code::KeyS, C | K),
            ("Command+Control+F", Code::KeyF, C | K),
            ("Command+H", Code::KeyH, C),
        ] {
            assert_eq!(ordinary(raw), chord(key, modifiers), "{raw}");
            assert_eq!(ordinary(raw), archived(raw), "{raw}");
        }
    }

    #[test]
    fn rejected_forms_match_the_archive() {
        for raw in [
            "",
            "Command+",
            "Command++A",
            "Command+Shift",
            "Shift",
            "Command",
            "Hyper+A",
            "Command+Nope",
            "A+B",
            "Command+A+Shift",
            "Command+A+B",
            "Fn+A",
            "F25",
            "F0",
            "Command+ ",
            " ",
            "Command+ + A",
        ] {
            assert!(parse(raw).is_err(), "{raw:?} must be rejected");
            assert_eq!(archived(raw), None, "{raw:?} must be rejected by the archive");
        }
        // Legacy normalization drops a leading separator, so `+A` is plain `A`.
        assert_eq!(
            parse("+A").ok(),
            archived("+A").map(|chord| Route::Ordinary { chord })
        );
        assert!(archived("+A").is_some());
        // The archive's own quirk is preserved: a single token is not trimmed,
        // components of a combination are.
        assert_eq!(ordinary(" A"), None);
        assert_eq!(ordinary("Command+ A "), chord(Code::KeyA, C));
        assert_eq!(ordinary("Command+ A "), archived("Command+ A "));
        assert!(matches!(parse("Command+Nope"), Err(ShortcutError::Ordinary(_))));
    }

    #[test]
    fn routing_precedence() {
        assert_eq!(parse("CapsLock+S"), Ok(Route::Caps { key: CapsKey(1) }));
        assert_eq!(parse("CapsLock+§"), Ok(Route::Caps { key: CapsKey(0x0A) }));
        assert_eq!(parse("CapsLock+return"), Ok(Route::Caps { key: CapsKey(36) }));
        assert_eq!(parse("Command+§"), Ok(Route::Section { modifiers: C }));
        assert_eq!(parse("Cmd+Alt+§"), Ok(Route::Section { modifiers: C | O }));
        assert_eq!(
            parse("Command+Shift+Ctrl+Opt+§"),
            Ok(Route::Section { modifiers: C | S | K | O })
        );
        assert_eq!(parse("§"), Ok(Route::Section { modifiers: ModifierFlags::NONE }));
        // Case-sensitive CapsLock and section modifiers, as archived.
        assert!(matches!(parse("capslock+S"), Err(ShortcutError::Ordinary(_))));
        assert_eq!(
            parse("command+§"),
            Err(ShortcutError::SectionUnknownModifier("command".into()))
        );
        assert_eq!(
            parse("Command++§"),
            Err(ShortcutError::SectionUnknownModifier(String::new()))
        );
        assert_eq!(parse("A+§+B"), Err(ShortcutError::SectionNotLast));
        assert_eq!(
            parse("CapsLock+Command+S"),
            Err(ShortcutError::CapsUnsupportedShape)
        );
    }
}
