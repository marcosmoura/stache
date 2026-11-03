use super::{CapsKey, CapsShortcut, CapsShortcutError};

pub fn parse_shortcut(shortcut: &str) -> CapsShortcut {
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
        // ISO section key: the dedicated `§` key on ISO/ABNT2 layouts.
        "§" => Some(10),
        "BACKSPACE" | "DELETE" => Some(51),
        "ESCAPE" => Some(53),
        "LEFT" | "ARROWLEFT" => Some(123),
        "RIGHT" | "ARROWRIGHT" => Some(124),
        "DOWN" | "ARROWDOWN" => Some(125),
        "UP" | "ARROWUP" => Some(126),
        _ => None,
    }
}
