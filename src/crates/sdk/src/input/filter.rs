//! Declarative event filters with explicit modifier semantics.

use std::fmt;

use super::event::{InputEventKind, KeyCode, ModifierFlags, MouseButton, PressStream};

/// Largest number of explicit key codes or buttons in one filter.
pub const MAX_FILTER_ENTRIES: usize = 256;

/// A set of [`InputEventKind`] values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KindSet(u8);

impl KindSet {
    /// The empty set (an invalid filter kind set).
    #[must_use]
    pub const fn empty() -> Self { Self(0) }

    /// Every kind.
    #[must_use]
    pub const fn all() -> Self { Self(31) }

    /// A set with one kind.
    #[must_use]
    pub const fn of(kind: InputEventKind) -> Self { Self(kind.bit()) }

    /// This set plus `kind`.
    #[must_use]
    pub const fn with(self, kind: InputEventKind) -> Self { Self(self.0 | kind.bit()) }

    /// Whether `kind` is in the set.
    #[must_use]
    pub const fn contains(self, kind: InputEventKind) -> bool { self.0 & kind.bit() != 0 }

    /// Whether both sets share a kind.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool { self.0 & other.0 != 0 }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool { self.0 == 0 }
}

/// Which keys or buttons a filter accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector<T> {
    /// Every value.
    Any,
    /// Exactly these values. An empty list matches nothing.
    Only(Vec<T>),
}

impl<T: Ord + Copy> Selector<T> {
    fn matches(&self, value: T) -> bool {
        match self {
            Self::Any => true,
            Self::Only(values) => values.binary_search(&value).is_ok(),
        }
    }

    fn intersects(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Any, Self::Only(v)) | (Self::Only(v), Self::Any) => !v.is_empty(),
            (Self::Any, Self::Any) => true,
            (Self::Only(a), Self::Only(b)) => a.iter().any(|value| b.binary_search(value).is_ok()),
        }
    }

    fn normalized(values: impl IntoIterator<Item = T>) -> Self {
        let mut values: Vec<T> = values.into_iter().collect();
        values.sort_unstable();
        values.dedup();
        Self::Only(values)
    }

    const fn len(&self) -> usize {
        match self {
            Self::Any => 0,
            Self::Only(values) => values.len(),
        }
    }
}

/// How a filter compares the event's modifier mask.
///
/// Caps Lock is ignored by [`ModifierMatch::Exact`] because it is a lock state,
/// not a held modifier. [`ModifierMatch::AllOf`] compares raw bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierMatch {
    /// Any modifier state.
    Any,
    /// The held modifiers equal this mask (Caps Lock ignored).
    Exact(ModifierFlags),
    /// Every modifier in this mask is held; extra modifiers are allowed.
    AllOf(ModifierFlags),
}

impl ModifierMatch {
    fn matches(self, flags: ModifierFlags) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(mask) => flags.normalized() == mask.normalized(),
            Self::AllOf(mask) => flags.contains(mask),
        }
    }

    /// Conservative: whether some modifier state satisfies both matches.
    fn overlaps(self, other: Self) -> bool {
        match (self, other) {
            (Self::Any, _) | (_, Self::Any) | (Self::AllOf(_), Self::AllOf(_)) => true,
            (Self::Exact(a), Self::Exact(b)) => a.normalized() == b.normalized(),
            (Self::Exact(exact), Self::AllOf(all)) | (Self::AllOf(all), Self::Exact(exact)) => {
                exact.normalized().contains(all.normalized())
            }
        }
    }
}

/// Why a filter is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterError {
    /// The kind set is empty, so the filter can never match.
    NoKinds,
    /// A selector used by the kinds is an empty `Only` list.
    EmptySelector,
    /// A selector holds more than [`MAX_FILTER_ENTRIES`] values.
    TooManyEntries,
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoKinds => "filter selects no event kind",
            Self::EmptySelector => "filter selector matches nothing",
            Self::TooManyEntries => "filter selector has too many entries",
        })
    }
}

impl std::error::Error for FilterError {}

/// A declarative event filter.
///
/// Key and flags events consult `keys`; mouse events consult `buttons`. A
/// `FlagsChanged` event for a key named in `keys` always passes the modifier
/// comparison, so a modifier release still reaches a subscriber even though the
/// resulting mask no longer matches. A `FlagsChanged` filter with
/// [`Selector::Any`] keys cannot use that exception and compares the mask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFilter {
    kinds: KindSet,
    keys: Selector<KeyCode>,
    buttons: Selector<MouseButton>,
    modifiers: ModifierMatch,
}

impl InputFilter {
    /// A filter for `kinds` accepting any key, button and modifier state.
    #[must_use]
    pub const fn new(kinds: KindSet) -> Self {
        Self {
            kinds,
            keys: Selector::Any,
            buttons: Selector::Any,
            modifiers: ModifierMatch::Any,
        }
    }

    /// Restricts to exactly these key codes.
    #[must_use]
    pub fn with_keys(mut self, keys: impl IntoIterator<Item = KeyCode>) -> Self {
        self.keys = Selector::normalized(keys);
        self
    }

    /// Restricts to exactly these mouse buttons.
    #[must_use]
    pub fn with_buttons(mut self, buttons: impl IntoIterator<Item = MouseButton>) -> Self {
        self.buttons = Selector::normalized(buttons);
        self
    }

    /// Sets the modifier comparison.
    #[must_use]
    pub const fn with_modifiers(mut self, modifiers: ModifierMatch) -> Self {
        self.modifiers = modifiers;
        self
    }

    /// The kind set.
    #[must_use]
    pub const fn kinds(&self) -> KindSet { self.kinds }

    /// The modifier comparison.
    #[must_use]
    pub const fn modifiers(&self) -> ModifierMatch { self.modifiers }

    /// Checks the filter can match something and is bounded.
    ///
    /// # Errors
    /// Returns a [`FilterError`] for an empty kind set, a selector that matches
    /// nothing, or an oversized selector.
    pub fn validate(&self) -> Result<(), FilterError> {
        if self.kinds.is_empty() {
            return Err(FilterError::NoKinds);
        }
        if self.keys.len() > MAX_FILTER_ENTRIES || self.buttons.len() > MAX_FILTER_ENTRIES {
            return Err(FilterError::TooManyEntries);
        }
        let uses_keys =
            InputEventKind::ALL.iter().any(|k| self.kinds.contains(*k) && !k.is_mouse());
        let uses_buttons = self.kinds.contains(InputEventKind::MouseDown)
            || self.kinds.contains(InputEventKind::MouseUp);
        if (uses_keys && matches!(&self.keys, Selector::Only(v) if v.is_empty()))
            || (uses_buttons && matches!(&self.buttons, Selector::Only(v) if v.is_empty()))
        {
            return Err(FilterError::EmptySelector);
        }
        Ok(())
    }

    /// Whether an event matches this filter.
    #[must_use]
    pub fn matches(&self, kind: InputEventKind, stream: PressStream, flags: ModifierFlags) -> bool {
        if !self.kinds.contains(kind) {
            return false;
        }
        let stream_ok = match stream {
            PressStream::Key(code) if !kind.is_mouse() => self.keys.matches(code),
            PressStream::Button(button) if kind.is_mouse() => self.buttons.matches(button),
            _ => return false,
        };
        if !stream_ok {
            return false;
        }
        let names_key = matches!(&self.keys, Selector::Only(_));
        if kind == InputEventKind::FlagsChanged && names_key {
            return true;
        }
        self.modifiers.matches(flags)
    }

    /// Conservative overlap test: whether some event can match both filters.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        InputEventKind::ALL.iter().any(|&kind| {
            if !self.kinds.contains(kind) || !other.kinds.contains(kind) {
                return false;
            }
            let streams = if kind.is_mouse() {
                self.buttons.intersects(&other.buttons)
            } else {
                self.keys.intersects(&other.keys)
            };
            streams
                && (kind == InputEventKind::FlagsChanged
                    || self.modifiers.overlaps(other.modifiers))
        })
    }
}
