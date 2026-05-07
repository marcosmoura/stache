use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPort;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};

use crate::config::ShortcutCommands;
use crate::modules::hotkey::execute_shortcut_commands;

type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;

type CGEventTapCallBack = extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;

    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventCreateKeyboardEvent(
        source: *mut c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const K_CG_EVENT_KEY_DOWN: u32 = 10;
const K_CG_EVENT_KEY_UP: u32 = 11;
const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
const K_CG_KEYBOARD_EVENT_AUTOREPEAT: u32 = 8;
const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
const KEY_CAPS_LOCK: i64 = 57;
const KEY_CAPS_LOCK_U16: u16 = 57;

static BINDINGS: Mutex<Option<CapsBindings>> = Mutex::new(None);
static STATE: Mutex<CapsState> = Mutex::new(CapsState {
    mode: CapsMode::Idle,
    active_key: None,
});
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static SYNTHETIC_CAPS_EVENTS: AtomicU8 = AtomicU8::new(0);

pub(super) type CapsBindings = HashMap<CapsKey, CapsBinding>;

#[derive(Debug, Clone)]
pub(super) struct CapsBinding {
    pub raw_shortcut: String,
    pub commands: ShortcutCommands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct CapsKey(i64);

impl CapsKey {
    #[must_use]
    pub(super) const fn new(keycode: i64) -> Self { Self(keycode) }
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
    if bindings.is_empty() {
        return false;
    }

    if let Ok(mut stored_bindings) = BINDINGS.lock() {
        *stored_bindings = Some(bindings);
    } else {
        tracing::warn!("CapsLock keybindings unavailable because binding state is poisoned");
        return false;
    }

    if INITIALIZED.swap(true, Ordering::SeqCst) {
        tracing::debug!("CapsLock keybinding event tap already initialized; bindings updated");
        return true;
    }

    std::thread::Builder::new()
        .name("stache-caps-lock-hotkeys".into())
        .spawn(start_event_tap)
        .map_or_else(
            |err| {
                tracing::warn!(error = %err, "failed to spawn CapsLock event tap thread");
                INITIALIZED.store(false, Ordering::SeqCst);
                false
            },
            |_| true,
        )
}

fn start_event_tap() {
    unsafe {
        let event_mask = (1u64 << K_CG_EVENT_FLAGS_CHANGED)
            | (1u64 << K_CG_EVENT_KEY_DOWN)
            | (1u64 << K_CG_EVENT_KEY_UP);

        let tap = CGEventTapCreate(
            K_CG_HID_EVENT_TAP,
            K_CG_HEAD_INSERT_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_DEFAULT,
            event_mask,
            event_tap_callback,
            ptr::null_mut(),
        );

        if tap.is_null() {
            tracing::warn!("failed to create CapsLock event tap - check accessibility permissions");
            INITIALIZED.store(false, Ordering::SeqCst);
            return;
        }

        let tap_port = CFMachPort::wrap_under_create_rule(tap.cast());
        let Ok(run_loop_source) = tap_port.create_runloop_source(0) else {
            tracing::warn!("failed to create CapsLock event tap run loop source");
            INITIALIZED.store(false, Ordering::SeqCst);
            return;
        };

        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&run_loop_source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        tracing::debug!("CapsLock keybinding event tap initialized");
        CFRunLoop::run_current();
    }
}

extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if event.is_null() {
        return event;
    }

    let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };

    let Some(input) = input_for_event(event_type, event, keycode) else {
        return event;
    };

    let action = {
        let Ok(mut state) = STATE.lock() else {
            tracing::warn!("CapsLock keybindings unavailable because state is poisoned");
            return event;
        };
        let Ok(bindings) = BINDINGS.lock() else {
            tracing::warn!("CapsLock keybindings unavailable because binding state is poisoned");
            return event;
        };
        let Some(bindings) = bindings.as_ref() else {
            return event;
        };

        action_for_input(&mut state, input, bindings)
    };

    if let Some(commands) = action.commands.as_ref() {
        execute_shortcut_commands(commands);
    }

    if action.synthesize_caps_tap {
        synthesize_caps_lock_tap();
    }

    if action.suppress {
        ptr::null_mut()
    } else {
        event
    }
}

fn input_for_event(event_type: u32, event: CGEventRef, keycode: i64) -> Option<CapsInput> {
    match event_type {
        K_CG_EVENT_FLAGS_CHANGED if keycode == KEY_CAPS_LOCK => {
            if SYNTHETIC_CAPS_EVENTS.load(Ordering::SeqCst) > 0 {
                SYNTHETIC_CAPS_EVENTS.fetch_sub(1, Ordering::SeqCst);
                None
            } else {
                next_caps_input()
            }
        }
        K_CG_EVENT_KEY_DOWN => {
            let is_repeat =
                unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_AUTOREPEAT) != 0 };
            Some(CapsInput::KeyDown(CapsKey::new(keycode), is_repeat))
        }
        K_CG_EVENT_KEY_UP => Some(CapsInput::KeyUp(CapsKey::new(keycode))),
        _ => None,
    }
}

fn next_caps_input() -> Option<CapsInput> {
    let Ok(state) = STATE.lock() else {
        tracing::warn!("CapsLock keybindings unavailable because state is poisoned");
        return None;
    };

    Some(match state.mode {
        CapsMode::Idle => CapsInput::CapsDown,
        CapsMode::CapsHeld | CapsMode::ChordUsed => CapsInput::CapsUp,
    })
}

fn synthesize_caps_lock_tap() {
    SYNTHETIC_CAPS_EVENTS.store(2, Ordering::SeqCst);

    unsafe {
        let down_event = CGEventCreateKeyboardEvent(ptr::null_mut(), KEY_CAPS_LOCK_U16, true);
        let up_event = CGEventCreateKeyboardEvent(ptr::null_mut(), KEY_CAPS_LOCK_U16, false);

        if down_event.is_null() || up_event.is_null() {
            tracing::warn!("failed to create synthetic CapsLock keyboard event");
        }

        if !down_event.is_null() {
            CGEventPost(K_CG_HID_EVENT_TAP, down_event);
            CFRelease(down_event.cast_const());
        }

        if !up_event.is_null() {
            CGEventPost(K_CG_HID_EVENT_TAP, up_event);
            CFRelease(up_event.cast_const());
        }
    }
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
struct CapsState {
    mode: CapsMode,
    active_key: Option<CapsKey>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum CapsMode {
    #[default]
    Idle,
    CapsHeld,
    ChordUsed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapsInput {
    CapsDown,
    CapsUp,
    KeyDown(CapsKey, bool),
    KeyUp(CapsKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapsDecision {
    Pass,
    Suppress,
    Execute(CapsKey),
    SynthesizeCapsTap,
}

impl CapsState {
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

#[derive(Debug, Default)]
struct CapsAction {
    suppress: bool,
    synthesize_caps_tap: bool,
    commands: Option<ShortcutCommands>,
}

fn action_for_input(
    state: &mut CapsState,
    input: CapsInput,
    bindings: &CapsBindings,
) -> CapsAction {
    match state.handle_input(input, |key| bindings.contains_key(&key)) {
        CapsDecision::Pass => CapsAction::default(),
        CapsDecision::Suppress => CapsAction {
            suppress: true,
            ..CapsAction::default()
        },
        CapsDecision::SynthesizeCapsTap => CapsAction {
            suppress: true,
            synthesize_caps_tap: true,
            commands: None,
        },
        CapsDecision::Execute(key) => CapsAction {
            suppress: true,
            synthesize_caps_tap: false,
            commands: bindings.get(&key).map(|binding| binding.commands.clone()),
        },
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

    #[test]
    fn action_for_caps_down_suppresses_native_event() {
        let mut state = CapsState::default();
        let bindings = CapsBindings::new();

        let action = action_for_input(&mut state, CapsInput::CapsDown, &bindings);

        assert!(action.suppress);
        assert!(!action.synthesize_caps_tap);
        assert!(action.commands.is_none());
    }

    #[test]
    fn action_for_configured_chord_clones_commands() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let mut bindings = CapsBindings::new();
        bindings.insert(key, CapsBinding {
            raw_shortcut: "CapsLock+S".to_string(),
            commands: ShortcutCommands::Single("screencapture -i -c".to_string()),
        });

        let caps_action = action_for_input(&mut state, CapsInput::CapsDown, &bindings);
        assert!(caps_action.suppress);

        let chord_action = action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);

        assert!(chord_action.suppress);
        assert!(matches!(
            chord_action.commands,
            Some(ShortcutCommands::Single(command)) if command == "screencapture -i -c"
        ));
    }

    #[test]
    fn action_for_caps_tap_requests_synthetic_caps_tap() {
        let mut state = CapsState::default();
        let bindings = CapsBindings::new();

        assert!(action_for_input(&mut state, CapsInput::CapsDown, &bindings).suppress);
        let action = action_for_input(&mut state, CapsInput::CapsUp, &bindings);

        assert!(action.suppress);
        assert!(action.synthesize_caps_tap);
        assert!(action.commands.is_none());
    }
}
