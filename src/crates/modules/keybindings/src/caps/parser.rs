//! The `CapsLock+<key>` grammar.
//!
//! This is the one key table and grammar of [`crate::shortcut`], re-exported
//! under the Caps adapter so there is no second table to drift. Behavior is the
//! archived one: the first component is exactly `CapsLock` (case-sensitive),
//! exactly one key component follows, and key names match case-insensitively.

pub use crate::shortcut::{CapsKey, ShortcutError};

/// A Caps shortcut classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsShortcut {
    /// Not a `CapsLock` shortcut; another grammar owns it.
    NotCaps,
    /// A valid `CapsLock+<key>` binding.
    Binding(CapsKey),
    /// A `CapsLock` shortcut the grammar rejects.
    Invalid(ShortcutError),
}

/// Classifies an already normalized spelling (see [`crate::shortcut::normalize`]).
#[must_use]
pub fn parse_shortcut(normalized: &str) -> CapsShortcut {
    match crate::shortcut::parse_caps(normalized) {
        None => CapsShortcut::NotCaps,
        Some(Ok(key)) => CapsShortcut::Binding(key),
        Some(Err(error)) => CapsShortcut::Invalid(error),
    }
}
