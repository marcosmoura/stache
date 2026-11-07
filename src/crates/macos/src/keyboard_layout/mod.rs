//! Keyboard-layout awareness for the `§` shortcut.
//!
//! The ordinary `§` key has no fixed physical position: it is the dedicated ISO
//! key on some layouts, `Option+6` on US ANSI and `Shift+3` on German. This module
//! turns "which physical chord types `§`" into a pure, testable question:
//!
//! - [`resolver`] searches key codes and modifier combinations over a
//!   [`KeyLayout`] table with dead-key state reset, picks the fewest required
//!   modifiers, then the lowest modifier bits, then the lowest key code, and falls
//!   back to the ISO key `0x0A` with an honest diagnostic when no layout data is
//!   available. It never touches a native API.
//! - `system` is the one real [`KeyLayout`] source: Text Input Sources (TIS) and
//!   `UCKeyTranslate`. It copies the layout bytes, so the table owns the data for
//!   its whole lifetime. It is main-thread only and never used by default tests.
//! - `observer` subscribes to the selected-keyboard-input-source change
//!   notification and only calls a `Send + Sync` hint closure: it never queries,
//!   registers or waits.
//!
//! Nothing here registers a hotkey; [`crate::hotkeys`] consumes the resolution.

pub(crate) mod observer;
pub mod resolver;
pub(crate) mod system;

pub use resolver::{
    CARBON_COMMAND, CARBON_CONTROL, CARBON_OPTION, CARBON_SHIFT, FallbackReason, ISO_SECTION_KEY,
    LayoutResolution, SectionSource, carbon_modifiers, section_modifiers, select_candidate,
};

/// The identity of one keyboard layout's data.
///
/// Equal identities mean the same layout data was read; a change means every
/// layout-derived registration may be obsolete.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LayoutId(pub u64);

/// A key-translation table for one keyboard layout.
pub trait KeyLayout {
    /// The identity of the underlying layout data.
    fn identity(&self) -> LayoutId;

    /// The first UTF-16 unit typed by `key_code` with `carbon_modifiers` held
    /// (the Carbon masks, for example `0x0200` for Shift), with dead-key state
    /// reset and dead keys suppressed. `None` when the key types nothing.
    fn translate(&self, key_code: u16, carbon_modifiers: u32) -> Option<u16>;
}

/// The result of reading the current layout.
pub enum LayoutRead {
    /// A readable layout.
    Available(Box<dyn KeyLayout>),
    /// No layout data could be read; the reason is shown to the user.
    Unavailable(String),
}

impl LayoutRead {
    /// The layout identity, or `None` when unavailable.
    #[must_use]
    pub fn identity(&self) -> Option<LayoutId> {
        match self {
            Self::Available(layout) => Some(layout.identity()),
            Self::Unavailable(_) => None,
        }
    }
}

impl std::fmt::Debug for LayoutRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available(layout) => write!(f, "Available({:?})", layout.identity()),
            Self::Unavailable(reason) => write!(f, "Unavailable({reason})"),
        }
    }
}

#[cfg(test)]
mod tests;
