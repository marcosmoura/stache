//! Pure conversion from plain `CGEvent` field values to the SDK's [`RawInput`].
//!
//! The tap callback reads the native fields and calls [`classify`]; nothing here
//! touches a native object, so the mapping is unit-tested without a tap.

use std::time::Duration;

use objc2_core_graphics::{CGEventFlags, CGEventType};
use stache_sdk::input::{
    InputEventKind, KeyCode, ModifierFlags, MouseButton, PressStream, RawInput,
};

/// The plain values the callback reads from one `CGEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventFields {
    /// `CGEventGetType` (or the callback's `type` argument).
    pub event_type: u32,
    /// `kCGKeyboardEventKeycode`.
    pub key_code: i64,
    /// `kCGKeyboardEventAutorepeat`.
    pub autorepeat: i64,
    /// `kCGMouseEventButtonNumber`.
    pub button: i64,
    /// `CGEventGetFlags`.
    pub flags: u64,
    /// `CGEventGetTimestamp` (nanoseconds).
    pub timestamp_ns: u64,
}

/// What one callback invocation means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classified {
    /// A routable input event.
    Input(RawInput),
    /// The OS disabled the tap (timeout or user input).
    TapDisabled,
    /// Not an event this owner routes; it passes untouched.
    Ignore,
}

/// Event types the taps subscribe to: key down/up, flags changed and
/// left/right/other mouse down/up.
pub const SUBSCRIBED_TYPES: [CGEventType; 9] = [
    CGEventType::KeyDown,
    CGEventType::KeyUp,
    CGEventType::FlagsChanged,
    CGEventType::LeftMouseDown,
    CGEventType::LeftMouseUp,
    CGEventType::RightMouseDown,
    CGEventType::RightMouseUp,
    CGEventType::OtherMouseDown,
    CGEventType::OtherMouseUp,
];

/// The `CGEventMask` covering [`SUBSCRIBED_TYPES`].
pub fn event_mask() -> u64 {
    SUBSCRIBED_TYPES.iter().fold(0, |mask, kind| mask | (1_u64 << kind.0))
}

/// Maps native modifier flags to the SDK layout. Unknown bits are dropped.
pub fn modifier_flags(flags: u64) -> ModifierFlags {
    let table = [
        (CGEventFlags::MaskAlphaShift, ModifierFlags::CAPS_LOCK),
        (CGEventFlags::MaskShift, ModifierFlags::SHIFT),
        (CGEventFlags::MaskControl, ModifierFlags::CONTROL),
        (CGEventFlags::MaskAlternate, ModifierFlags::OPTION),
        (CGEventFlags::MaskCommand, ModifierFlags::COMMAND),
        (CGEventFlags::MaskSecondaryFn, ModifierFlags::FUNCTION),
    ];
    table
        .into_iter()
        .filter(|(native, _)| flags & native.0 != 0)
        .fold(ModifierFlags::NONE, |mask, (_, sdk)| mask | sdk)
}

/// Classifies one callback invocation.
pub fn classify(fields: EventFields) -> Classified {
    let kind_type = CGEventType(fields.event_type);
    if kind_type == CGEventType::TapDisabledByTimeout
        || kind_type == CGEventType::TapDisabledByUserInput
    {
        return Classified::TapDisabled;
    }
    let (kind, is_mouse) = match kind_type {
        CGEventType::KeyDown => (InputEventKind::KeyDown, false),
        CGEventType::KeyUp => (InputEventKind::KeyUp, false),
        CGEventType::FlagsChanged => (InputEventKind::FlagsChanged, false),
        CGEventType::LeftMouseDown | CGEventType::RightMouseDown | CGEventType::OtherMouseDown => {
            (InputEventKind::MouseDown, true)
        }
        CGEventType::LeftMouseUp | CGEventType::RightMouseUp | CGEventType::OtherMouseUp => {
            (InputEventKind::MouseUp, true)
        }
        _ => return Classified::Ignore,
    };
    let stream = if is_mouse {
        let Ok(button) = u32::try_from(fields.button) else {
            return Classified::Ignore;
        };
        PressStream::Button(MouseButton(button))
    } else {
        let Ok(code) = u16::try_from(fields.key_code) else {
            return Classified::Ignore;
        };
        PressStream::Key(KeyCode(code))
    };
    Classified::Input(RawInput {
        kind,
        stream,
        flags: modifier_flags(fields.flags),
        repeat: kind == InputEventKind::KeyDown && fields.autorepeat != 0,
        timestamp: Duration::from_nanos(fields.timestamp_ns),
    })
}
