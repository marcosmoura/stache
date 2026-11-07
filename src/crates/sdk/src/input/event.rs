//! Immutable input values: key codes, buttons, modifier masks and events.

use std::ops::BitOr;
use std::sync::Arc;
use std::time::Duration;

use super::context_snapshot::ContextSnapshot;

/// A hardware-independent virtual key code (macOS `kVK_*` numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyCode(pub u16);

/// An exact mouse-button number.
///
/// Button numbers are compared exactly: button `2` is representable without
/// also matching buttons `3` and above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MouseButton(pub u32);

/// A modifier mask in an SDK-defined bit layout.
///
/// The native owner translates platform flags into this layout. Bits are named
/// constants; callers never depend on platform raw values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ModifierFlags(u32);

impl ModifierFlags {
    /// Caps Lock state.
    pub const CAPS_LOCK: Self = Self(1 << 0);
    /// Command.
    pub const COMMAND: Self = Self(1 << 4);
    /// Control.
    pub const CONTROL: Self = Self(1 << 2);
    /// Function (fn).
    pub const FUNCTION: Self = Self(1 << 5);
    const KNOWN: u32 = 0b11_1111;
    /// No modifier held.
    pub const NONE: Self = Self(0);
    /// Option (Alt).
    pub const OPTION: Self = Self(1 << 3);
    /// Shift.
    pub const SHIFT: Self = Self(1 << 1);

    /// Builds a mask from raw bits, dropping bits that have no name.
    #[must_use]
    pub const fn from_bits_truncate(bits: u32) -> Self { Self(bits & Self::KNOWN) }

    /// The raw bits of this mask.
    #[must_use]
    pub const fn bits(self) -> u32 { self.0 }

    /// Whether every bit of `other` is set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool { self.0 & other.0 == other.0 }

    /// Whether `self` and `other` share at least one bit.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool { self.0 & other.0 != 0 }

    /// The union of both masks.
    #[must_use]
    pub const fn union(self, other: Self) -> Self { Self(self.0 | other.0) }

    /// This mask with every bit of `other` cleared.
    #[must_use]
    pub const fn without(self, other: Self) -> Self { Self(self.0 & !other.0) }

    /// The physical-chord form of this mask: Caps Lock is a lock state, not a
    /// held modifier, so it is dropped.
    #[must_use]
    pub const fn normalized(self) -> Self { self.without(Self::CAPS_LOCK) }

    /// Whether no bit is set.
    #[must_use]
    pub const fn is_empty(self) -> bool { self.0 == 0 }
}

impl BitOr for ModifierFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self { self.union(rhs) }
}

/// The kind of one native input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputEventKind {
    /// A key went down (auto-repeat is a repeated `KeyDown`).
    KeyDown,
    /// A key went up.
    KeyUp,
    /// The modifier mask changed. This covers modifier press and release.
    FlagsChanged,
    /// A mouse button went down.
    MouseDown,
    /// A mouse button went up.
    MouseUp,
}

impl InputEventKind {
    pub(crate) const ALL: [Self; 5] = [
        Self::KeyDown,
        Self::KeyUp,
        Self::FlagsChanged,
        Self::MouseDown,
        Self::MouseUp,
    ];

    pub(crate) const fn bit(self) -> u8 {
        match self {
            Self::KeyDown => 1,
            Self::KeyUp => 2,
            Self::FlagsChanged => 4,
            Self::MouseDown => 8,
            Self::MouseUp => 16,
        }
    }

    pub(crate) const fn is_mouse(self) -> bool { matches!(self, Self::MouseDown | Self::MouseUp) }

    pub(crate) const fn is_down(self) -> bool { matches!(self, Self::KeyDown | Self::MouseDown) }

    pub(crate) const fn is_up(self) -> bool { matches!(self, Self::KeyUp | Self::MouseUp) }
}

/// The physical stream a press belongs to: one key code or one mouse button.
///
/// The macOS HID event tap carries no per-keyboard identity, so two keyboards
/// pressing the same key share one stream. Press identity is the stream plus the
/// owner [`Epoch`]; it never claims a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PressStream {
    /// A keyboard key.
    Key(KeyCode),
    /// A mouse button.
    Button(MouseButton),
}

/// Owner epoch, advanced whenever a tap was disabled and events may be missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Epoch(pub u64);

/// Identity of one physical press: its stream and the epoch it started in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PressId {
    /// The key or button.
    pub stream: PressStream,
    /// The epoch the press was observed in.
    pub epoch: Epoch,
}

/// A point in Quartz global display coordinates (origin at the top-left of the
/// main display, y growing downward).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputPoint {
    /// Horizontal coordinate.
    pub x: f64,
    /// Vertical coordinate.
    pub y: f64,
}

/// What the native owner reports for one event, before the router adds the
/// press identity and context snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawInput {
    /// Event kind.
    pub kind: InputEventKind,
    /// Key or button; must agree with `kind`.
    pub stream: PressStream,
    /// Modifier mask after this event.
    pub flags: ModifierFlags,
    /// Auto-repeat marker for `KeyDown`.
    pub repeat: bool,
    /// Event time as a monotonic offset.
    pub timestamp: Duration,
    /// Event location in Quartz global coordinates; `None` for keyboard events.
    pub location: Option<InputPoint>,
}

impl RawInput {
    pub(crate) const fn is_consistent(&self) -> bool {
        matches!(
            (self.kind.is_mouse(), self.stream),
            (true, PressStream::Button(_)) | (false, PressStream::Key(_))
        )
    }
}

/// One immutable event handed to a subscriber.
///
/// It exposes no native pointer and has no mutation or injection API.
#[derive(Debug, Clone)]
pub struct InputEvent {
    kind: InputEventKind,
    stream: PressStream,
    flags: ModifierFlags,
    repeat: bool,
    timestamp: Duration,
    location: Option<InputPoint>,
    press: PressId,
    context: Arc<ContextSnapshot>,
}

impl InputEvent {
    pub(crate) const fn new(raw: RawInput, epoch: Epoch, context: Arc<ContextSnapshot>) -> Self {
        Self {
            kind: raw.kind,
            stream: raw.stream,
            flags: raw.flags,
            repeat: raw.repeat,
            timestamp: raw.timestamp,
            location: raw.location,
            press: PressId { stream: raw.stream, epoch },
            context,
        }
    }

    /// Event kind.
    #[must_use]
    pub const fn kind(&self) -> InputEventKind { self.kind }

    /// The key or button this event is about.
    #[must_use]
    pub const fn stream(&self) -> PressStream { self.stream }

    /// Modifier mask after the event.
    #[must_use]
    pub const fn flags(&self) -> ModifierFlags { self.flags }

    /// Whether this is an auto-repeat.
    #[must_use]
    pub const fn repeat(&self) -> bool { self.repeat }

    /// Monotonic event time.
    #[must_use]
    pub const fn timestamp(&self) -> Duration { self.timestamp }

    /// The event's own Quartz location, captured by the native callback. `None`
    /// for keyboard events.
    #[must_use]
    pub const fn location(&self) -> Option<InputPoint> { self.location }

    /// Physical-press identity.
    #[must_use]
    pub const fn press(&self) -> PressId { self.press }

    /// The context snapshot read once for this event.
    #[must_use]
    pub fn context(&self) -> &ContextSnapshot { &self.context }
}

/// A subscriber's decision for one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Let the event continue to the system.
    Pass,
    /// Consume the event.
    Suppress,
}
