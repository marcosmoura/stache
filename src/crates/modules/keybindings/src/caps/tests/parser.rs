//! KB-04-01: the legacy key table, aliases, case and `CapsLock+<key>` grammar.

use crate::caps::parser::{CapsShortcut, parse_shortcut};
use crate::shortcut::{self, CapsKey, Route, ShortcutError};

/// The archived table (virtual key codes) as an independent oracle.
const TABLE: &[(&str, u16)] = &[
    ("A", 0),
    ("S", 1),
    ("D", 2),
    ("F", 3),
    ("H", 4),
    ("G", 5),
    ("Z", 6),
    ("X", 7),
    ("C", 8),
    ("V", 9),
    ("B", 11),
    ("Q", 12),
    ("W", 13),
    ("E", 14),
    ("R", 15),
    ("Y", 16),
    ("T", 17),
    ("1", 18),
    ("Digit1", 18),
    ("2", 19),
    ("Digit2", 19),
    ("3", 20),
    ("Digit3", 20),
    ("4", 21),
    ("Digit4", 21),
    ("6", 22),
    ("Digit6", 22),
    ("5", 23),
    ("Digit5", 23),
    ("Equal", 24),
    ("9", 25),
    ("Digit9", 25),
    ("7", 26),
    ("Digit7", 26),
    ("Minus", 27),
    ("8", 28),
    ("Digit8", 28),
    ("0", 29),
    ("Digit0", 29),
    ("RightBracket", 30),
    ("O", 31),
    ("U", 32),
    ("LeftBracket", 33),
    ("I", 34),
    ("P", 35),
    ("Enter", 36),
    ("Return", 36),
    ("L", 37),
    ("J", 38),
    ("Quote", 39),
    ("K", 40),
    ("Semicolon", 41),
    ("Backslash", 42),
    ("Comma", 43),
    ("Slash", 44),
    ("N", 45),
    ("M", 46),
    ("Period", 47),
    ("Tab", 48),
    ("Space", 49),
    ("Backquote", 50),
    ("Grave", 50),
    ("§", 10),
    ("Backspace", 51),
    ("Delete", 51),
    ("Escape", 53),
    ("Left", 123),
    ("ArrowLeft", 123),
    ("Right", 124),
    ("ArrowRight", 124),
    ("Down", 125),
    ("ArrowDown", 125),
    ("Up", 126),
    ("ArrowUp", 126),
];

#[test]
fn every_key_and_alias_parses_to_the_archived_code_case_insensitively() {
    for (name, code) in TABLE {
        for spelling in [(*name).to_owned(), name.to_uppercase(), name.to_lowercase()] {
            let shortcut = format!("CapsLock+{spelling}");
            assert_eq!(
                parse_shortcut(&shortcut),
                CapsShortcut::Binding(CapsKey(*code)),
                "{shortcut}"
            );
            // The full router agrees, so there is a single table.
            assert_eq!(
                shortcut::parse(&shortcut),
                Ok(Route::Caps { key: CapsKey(*code) }),
                "{shortcut}"
            );
        }
    }
    assert_eq!(TABLE.len(), 74, "the table has every legacy entry");
}

#[test]
fn only_the_single_key_grammar_is_accepted_and_the_prefix_is_case_sensitive() {
    assert_eq!(parse_shortcut("Command+A"), CapsShortcut::NotCaps);
    assert_eq!(parse_shortcut("A"), CapsShortcut::NotCaps);
    assert_eq!(parse_shortcut(""), CapsShortcut::NotCaps);
    assert_eq!(parse_shortcut("capslock+A"), CapsShortcut::NotCaps);
    assert_eq!(parse_shortcut("CAPSLOCK+A"), CapsShortcut::NotCaps);
    assert_eq!(parse_shortcut("Command+CapsLock+A"), CapsShortcut::NotCaps);
    assert_eq!(
        parse_shortcut("CapsLock"),
        CapsShortcut::Invalid(ShortcutError::CapsMissingKey)
    );
    assert_eq!(
        parse_shortcut("CapsLock+Shift+A"),
        CapsShortcut::Invalid(ShortcutError::CapsUnsupportedShape)
    );
    assert_eq!(
        parse_shortcut("CapsLock+A+"),
        CapsShortcut::Invalid(ShortcutError::CapsUnsupportedShape)
    );
    assert_eq!(
        parse_shortcut("CapsLock+Nope"),
        CapsShortcut::Invalid(ShortcutError::CapsUnknownKey("Nope".to_owned()))
    );
    assert_eq!(
        parse_shortcut("CapsLock+ A"),
        CapsShortcut::Invalid(ShortcutError::CapsUnknownKey(" A".to_owned()))
    );
}
