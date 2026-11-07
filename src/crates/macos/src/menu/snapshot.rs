//! Pure, `Send` menu data and the legacy rendering rules.
//!
//! Nothing here touches AX or `AppKit`. The reader builds these values; a later
//! presenter turns them into native menus. Rules follow the legacy builder
//! (`menu_builder.rs:266-405`) with the corrected modifier decoding from
//! `AXAttributeConstants.h:995-999`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Unique identity of one menu presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PresentationId(u64);

static NEXT_PRESENTATION: AtomicU64 = AtomicU64::new(1);

impl PresentationId {
    /// Allocates a process-unique identity.
    #[must_use]
    pub fn allocate() -> Self { Self(NEXT_PRESENTATION.fetch_add(1, Ordering::Relaxed)) }
}

/// An opaque handle to one enabled leaf item of one presentation.
///
/// It carries no pointer; the retained AX element lives on the AX owner thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActionToken {
    presentation: PresentationId,
    ordinal: u32,
}

impl ActionToken {
    pub(super) const fn new(presentation: PresentationId, ordinal: u32) -> Self {
        Self { presentation, ordinal }
    }

    /// The presentation this token belongs to.
    #[must_use]
    pub const fn presentation(self) -> PresentationId { self.presentation }
}

/// The check mark state of an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkState {
    /// No mark or an unknown mark character.
    #[default]
    Off,
    /// `✓`.
    On,
    /// `•`.
    Mixed,
}

impl MarkState {
    /// Decodes `AXMenuItemMarkChar`: `✓` on, `•` mixed, anything else off.
    #[must_use]
    pub fn from_mark_char(mark: Option<&str>) -> Self {
        match mark {
            Some("✓") => Self::On,
            Some("•") => Self::Mixed,
            _ => Self::Off,
        }
    }
}

/// The modifier keys displayed with a shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent modifier keys"
)]
pub struct ModifierSet {
    /// Shift (AX bit 0).
    pub shift: bool,
    /// Option (AX bit 1).
    pub option: bool,
    /// Control (AX bit 2).
    pub control: bool,
    /// Command: present unless AX bit 3 (`NoCommand`) is set.
    pub command: bool,
}

impl ModifierSet {
    /// Decodes `AXMenuItemCmdModifiers`. A missing attribute means Command.
    #[must_use]
    pub const fn from_ax(raw: Option<i64>) -> Self {
        let bits = match raw {
            Some(bits) => bits,
            None => 0,
        };
        Self {
            shift: bits & 1 != 0,
            option: bits & 2 != 0,
            control: bits & 4 != 0,
            command: bits & 8 == 0,
        }
    }
}

/// A displayed keyboard shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEquivalent {
    /// Lowercased command character; Unicode is retained.
    pub key: String,
    /// The modifiers to display.
    pub modifiers: ModifierSet,
}

/// What an item is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuItemKind {
    /// A separator line.
    Separator,
    /// A leaf item.
    Item,
    /// An item with a submenu, which is never actionable.
    Submenu(SubmenuSnapshot),
}

/// A submenu. Its title inherits the parent item's title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmenuSnapshot {
    /// The parent item's title.
    pub title: String,
    /// The submenu's items.
    pub items: Vec<MenuItemSnapshot>,
}

/// One menu item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItemSnapshot {
    /// The title; empty for separators.
    pub title: String,
    /// Separator, leaf or submenu.
    pub kind: MenuItemKind,
    /// `AXEnabled`, defaulting to true.
    pub enabled: bool,
    /// The mark state.
    pub mark: MarkState,
    /// The shortcut, when a nonempty command character exists.
    pub key_equivalent: Option<KeyEquivalent>,
    /// Whether the title is bold (the first nonseparator root item).
    pub bold: bool,
    /// Present only for enabled leaf items.
    pub action: Option<ActionToken>,
}

impl MenuItemSnapshot {
    /// A separator.
    #[must_use]
    pub const fn separator() -> Self {
        Self {
            title: String::new(),
            kind: MenuItemKind::Separator,
            enabled: false,
            mark: MarkState::Off,
            key_equivalent: None,
            bold: false,
            action: None,
        }
    }
}

/// The complete, immutable result of one successful menu-bar read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuSnapshot {
    /// The presentation this snapshot belongs to.
    pub presentation: PresentationId,
    /// Root items (the Apple menu is already skipped).
    pub items: Vec<MenuItemSnapshot>,
}

/// Lowercases a command character for native key-equivalent rendering.
#[must_use]
pub fn key_equivalent(cmd_char: Option<&str>, modifiers: Option<i64>) -> Option<KeyEquivalent> {
    let key = cmd_char.filter(|c| !c.is_empty())?;
    Some(KeyEquivalent {
        key: key.to_lowercase(),
        modifiers: ModifierSet::from_ax(modifiers),
    })
}

const _: () = {
    const fn assert_send<T: Send + Sync + 'static>() {}
    assert_send::<MenuSnapshot>();
    assert_send::<MenuItemSnapshot>();
    assert_send::<ActionToken>();
    assert_send::<PresentationId>();
};
