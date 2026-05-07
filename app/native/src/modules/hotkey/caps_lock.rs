use std::collections::HashMap;

use crate::config::ShortcutCommands;

pub(super) type CapsBindings = HashMap<CapsKey, CapsBinding>;

#[derive(Debug, Clone)]
pub(super) struct CapsBinding {
    pub raw_shortcut: String,
    #[allow(dead_code)]
    pub commands: ShortcutCommands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct CapsKey(i64);

impl CapsKey {
    #[must_use]
    pub(super) const fn new(keycode: i64) -> Self { Self(keycode) }

    #[allow(dead_code)]
    #[must_use]
    pub(super) const fn keycode(self) -> i64 { self.0 }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CapsShortcut {
    NotCaps,
    Binding(CapsKey),
    Invalid(CapsShortcutError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CapsShortcutError {
    MissingKey,
    UnsupportedShape,
    UnknownKey(String),
}

impl std::fmt::Display for CapsShortcutError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingKey => formatter.write_str("missing key after CapsLock"),
            Self::UnsupportedShape => {
                formatter.write_str("only CapsLock+<single key> is supported")
            }
            Self::UnknownKey(key) => write!(formatter, "unknown CapsLock key: {key}"),
        }
    }
}

pub(super) fn parse_shortcut(shortcut: &str) -> CapsShortcut {
    let mut parts = shortcut.split('+');
    let Some(first) = parts.next() else {
        return CapsShortcut::NotCaps;
    };

    if first != "CapsLock" {
        return CapsShortcut::NotCaps;
    }

    let Some(key_name) = parts.next() else {
        return CapsShortcut::Invalid(CapsShortcutError::MissingKey);
    };

    if parts.next().is_some() {
        return CapsShortcut::Invalid(CapsShortcutError::UnsupportedShape);
    }

    keycode_for_name(key_name).map_or_else(
        || CapsShortcut::Invalid(CapsShortcutError::UnknownKey(key_name.to_string())),
        |keycode| CapsShortcut::Binding(CapsKey::new(keycode)),
    )
}

pub(super) fn start(bindings: CapsBindings) -> bool {
    let count = bindings.len();

    drop(bindings);

    tracing::warn!(
        count,
        "CapsLock keybindings parsed but event tap is not initialized yet"
    );
    false
}

fn keycode_for_name(key_name: &str) -> Option<i64> {
    let normalized = key_name.to_ascii_uppercase();
    match normalized.as_str() {
        "A" => Some(0),
        "S" => Some(1),
        "D" => Some(2),
        "F" => Some(3),
        "H" => Some(4),
        "G" => Some(5),
        "Z" => Some(6),
        "X" => Some(7),
        "C" => Some(8),
        "V" => Some(9),
        "B" => Some(11),
        "Q" => Some(12),
        "W" => Some(13),
        "E" => Some(14),
        "R" => Some(15),
        "Y" => Some(16),
        "T" => Some(17),
        "1" | "DIGIT1" => Some(18),
        "2" | "DIGIT2" => Some(19),
        "3" | "DIGIT3" => Some(20),
        "4" | "DIGIT4" => Some(21),
        "6" | "DIGIT6" => Some(22),
        "5" | "DIGIT5" => Some(23),
        "EQUAL" => Some(24),
        "9" | "DIGIT9" => Some(25),
        "7" | "DIGIT7" => Some(26),
        "MINUS" => Some(27),
        "8" | "DIGIT8" => Some(28),
        "0" | "DIGIT0" => Some(29),
        "RIGHTBRACKET" => Some(30),
        "O" => Some(31),
        "U" => Some(32),
        "LEFTBRACKET" => Some(33),
        "I" => Some(34),
        "P" => Some(35),
        "ENTER" | "RETURN" => Some(36),
        "L" => Some(37),
        "J" => Some(38),
        "QUOTE" => Some(39),
        "K" => Some(40),
        "SEMICOLON" => Some(41),
        "BACKSLASH" => Some(42),
        "COMMA" => Some(43),
        "SLASH" => Some(44),
        "N" => Some(45),
        "M" => Some(46),
        "PERIOD" => Some(47),
        "TAB" => Some(48),
        "SPACE" => Some(49),
        "BACKQUOTE" | "GRAVE" => Some(50),
        "BACKSPACE" | "DELETE" => Some(51),
        "ESCAPE" => Some(53),
        "LEFT" | "ARROWLEFT" => Some(123),
        "RIGHT" | "ARROWRIGHT" => Some(124),
        "DOWN" | "ARROWDOWN" => Some(125),
        "UP" | "ARROWUP" => Some(126),
        _ => None,
    }
}

#[derive(Debug, Default)]
#[allow(dead_code)]
struct CapsState {
    mode: CapsMode,
    active_key: Option<CapsKey>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum CapsMode {
    #[default]
    Idle,
    CapsHeld,
    ChordUsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum CapsInput {
    CapsDown,
    CapsUp,
    KeyDown(CapsKey, bool),
    KeyUp(CapsKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum CapsDecision {
    Pass,
    Suppress,
    Execute(CapsKey),
    SynthesizeCapsTap,
}

impl CapsState {
    #[allow(dead_code)]
    fn handle_input(
        &mut self,
        input: CapsInput,
        has_binding: impl Fn(CapsKey) -> bool,
    ) -> CapsDecision {
        match input {
            CapsInput::CapsDown => {
                self.mode = CapsMode::CapsHeld;
                self.active_key = None;
                CapsDecision::Suppress
            }
            CapsInput::CapsUp => match self.mode {
                CapsMode::CapsHeld => {
                    self.mode = CapsMode::Idle;
                    self.active_key = None;
                    CapsDecision::SynthesizeCapsTap
                }
                CapsMode::ChordUsed => {
                    self.mode = CapsMode::Idle;
                    CapsDecision::Suppress
                }
                CapsMode::Idle => CapsDecision::Pass,
            },
            CapsInput::KeyDown(key, true) if self.active_key == Some(key) => CapsDecision::Suppress,
            CapsInput::KeyDown(key, is_repeat) => match self.mode {
                CapsMode::CapsHeld if has_binding(key) && !is_repeat => {
                    self.mode = CapsMode::ChordUsed;
                    self.active_key = Some(key);
                    CapsDecision::Execute(key)
                }
                CapsMode::ChordUsed if self.active_key == Some(key) => CapsDecision::Suppress,
                _ => CapsDecision::Pass,
            },
            CapsInput::KeyUp(key) => {
                if self.active_key == Some(key) {
                    self.active_key = None;
                    CapsDecision::Suppress
                } else {
                    CapsDecision::Pass
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_caps_letter_binding() {
        assert_eq!(
            parse_shortcut("CapsLock+S"),
            CapsShortcut::Binding(CapsKey::new(1))
        );
    }

    #[test]
    fn parse_caps_digit_binding() {
        assert_eq!(
            parse_shortcut("CapsLock+1"),
            CapsShortcut::Binding(CapsKey::new(18))
        );
    }

    #[test]
    fn parse_caps_special_key_binding() {
        assert_eq!(
            parse_shortcut("CapsLock+Space"),
            CapsShortcut::Binding(CapsKey::new(49))
        );
        assert_eq!(
            parse_shortcut("CapsLock+Backquote"),
            CapsShortcut::Binding(CapsKey::new(50))
        );
    }

    #[test]
    fn parse_non_caps_shortcut() {
        assert_eq!(parse_shortcut("Command+Control+S"), CapsShortcut::NotCaps);
    }

    #[test]
    fn reject_unsupported_caps_shapes() {
        assert_eq!(
            parse_shortcut("CapsLock"),
            CapsShortcut::Invalid(CapsShortcutError::MissingKey)
        );
        assert_eq!(
            parse_shortcut("CapsLock+Command+S"),
            CapsShortcut::Invalid(CapsShortcutError::UnsupportedShape)
        );
        assert_eq!(
            parse_shortcut("CapsLock+S+T"),
            CapsShortcut::Invalid(CapsShortcutError::UnsupportedShape)
        );
    }

    #[test]
    fn reject_unknown_caps_key() {
        assert_eq!(
            parse_shortcut("CapsLock+DefinitelyNotAKey"),
            CapsShortcut::Invalid(CapsShortcutError::UnknownKey("DefinitelyNotAKey".to_string()))
        );
    }

    #[test]
    fn state_machine_synthesizes_caps_tap_for_plain_tap() {
        let mut state = CapsState::default();
        let has_binding = |_: CapsKey| false;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::SynthesizeCapsTap
        );
    }

    #[test]
    fn state_machine_executes_configured_chord_once() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let has_binding = |candidate: CapsKey| candidate == key;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Execute(key)
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, true), has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyUp(key), has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::Suppress
        );
    }

    #[test]
    fn state_machine_suppresses_chord_key_up_after_caps_released() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let has_binding = |candidate: CapsKey| candidate == key;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Execute(key)
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyUp(key), has_binding),
            CapsDecision::Suppress
        );
    }

    #[test]
    fn state_machine_suppresses_chord_repeat_after_caps_released() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let has_binding = |candidate: CapsKey| candidate == key;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Execute(key)
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, true), has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyUp(key), has_binding),
            CapsDecision::Suppress
        );
    }

    #[test]
    fn state_machine_passes_unbound_key_while_caps_held() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let has_binding = |_: CapsKey| false;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyUp(key), has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::SynthesizeCapsTap
        );
    }
}
