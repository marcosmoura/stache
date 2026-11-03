mod parser;
mod remap;
mod state;

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPort;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
pub use parser::parse_shortcut;
#[cfg(test)]
use state::{CapsDecision, CapsState};
use state::{CapsInput, STATE, action_for_input};

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
}

const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const K_CG_EVENT_KEY_DOWN: u32 = 10;
const K_CG_EVENT_KEY_UP: u32 = 11;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
const K_CG_KEYBOARD_EVENT_AUTOREPEAT: u32 = 8;

/// `kVK_F18` — physical Caps Lock arrives here after the hidutil remap.
const KEY_F18: i64 = 0x4F;
/// How long to wait for the event tap thread before giving up on the remap.
const EVENT_TAP_READY_TIMEOUT: Duration = Duration::from_secs(2);

static BINDINGS: Mutex<Option<CapsBindings>> = Mutex::new(None);
static EVENT_TAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static INITIALIZED: AtomicBool = AtomicBool::new(false);

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

/// Starts the `CapsLock` pseudo-modifier engine.
///
/// The event tap is brought up first, and only once it is confirmed running is
/// the Caps Lock → F18 HID remap applied. That ordering keeps the physical
/// Caps Lock key untouched if Stache lacks Accessibility permission.
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

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let spawn_result = std::thread::Builder::new()
        .name("stache-caps-lock-hotkeys".into())
        .spawn(move || start_event_tap(&ready_tx));

    if let Err(err) = spawn_result {
        tracing::warn!(error = %err, "failed to spawn CapsLock event tap thread");
        INITIALIZED.store(false, Ordering::SeqCst);
        false
    } else {
        let tap_ready = ready_rx.recv_timeout(EVENT_TAP_READY_TIMEOUT).unwrap_or(false);
        if !tap_ready {
            tracing::warn!("CapsLock event tap failed to start - check accessibility permissions");
            INITIALIZED.store(false, Ordering::SeqCst);
            return false;
        }

        if remap::apply() {
            true
        } else {
            // The tap is running but Caps Lock is not remapped, so the
            // bindings cannot fire. Keep the tap so nothing is intercepted
            // unexpectedly, but report failure.
            false
        }
    }
}

/// Restores the Caps Lock remapping so the key behaves normally again.
pub(super) fn shutdown() { remap::restore(); }

fn start_event_tap(ready_tx: &std::sync::mpsc::Sender<bool>) {
    unsafe {
        let event_mask = (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_KEY_UP);

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
            let _ = ready_tx.send(false);
            return;
        }

        EVENT_TAP.store(tap, Ordering::SeqCst);

        let tap_port = CFMachPort::wrap_under_create_rule(tap.cast());
        let Ok(run_loop_source) = tap_port.create_runloop_source(0) else {
            tracing::warn!("failed to create CapsLock event tap run loop source");
            EVENT_TAP.store(ptr::null_mut(), Ordering::SeqCst);
            INITIALIZED.store(false, Ordering::SeqCst);
            let _ = ready_tx.send(false);
            return;
        };

        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&run_loop_source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        tracing::debug!("CapsLock keybinding event tap initialized");
        let _ = ready_tx.send(true);
        CFRunLoop::run_current();
    }
}

extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if is_tap_disabled_event(event_type) {
        reenable_event_tap();
        return event;
    }

    if event.is_null() {
        return event;
    }

    let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };

    // Physical Caps Lock arrives as F18 down/up after the HID remap. Treat it
    // as the pseudo-modifier and always suppress it so apps never see it.
    if is_remapped_caps_lock_event(event_type, keycode) {
        let input = if event_type == K_CG_EVENT_KEY_DOWN {
            CapsInput::CapsDown
        } else {
            CapsInput::CapsUp
        };

        let Ok(mut state) = STATE.lock() else {
            tracing::warn!("CapsLock keybindings unavailable because state is poisoned");
            return ptr::null_mut();
        };
        let _ = state.handle_input(input, |_| false);
        return ptr::null_mut();
    }

    if !is_key_event(event_type) {
        return event;
    }

    let input = key_input_for_event(event_type, event, keycode);

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

    if action.suppress {
        ptr::null_mut()
    } else {
        event
    }
}

const fn is_tap_disabled_event(event_type: u32) -> bool {
    matches!(
        event_type,
        K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT | K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    )
}

fn reenable_event_tap() {
    let tap = EVENT_TAP.load(Ordering::SeqCst);
    if tap.is_null() {
        tracing::warn!("CapsLock event tap disabled but tap handle is unavailable");
        return;
    }

    unsafe { CGEventTapEnable(tap, true) };
    tracing::debug!("re-enabled CapsLock event tap");
}

const fn is_key_event(event_type: u32) -> bool {
    matches!(event_type, K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP)
}

/// Detects the F18 events that carry the physical Caps Lock after the remap.
const fn is_remapped_caps_lock_event(event_type: u32, keycode: i64) -> bool {
    keycode == KEY_F18 && is_key_event(event_type)
}

fn key_input_for_event(event_type: u32, event: CGEventRef, keycode: i64) -> CapsInput {
    match event_type {
        K_CG_EVENT_KEY_DOWN => {
            let is_repeat =
                unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_AUTOREPEAT) != 0 };
            CapsInput::KeyDown(CapsKey::new(keycode), is_repeat)
        }
        _ => CapsInput::KeyUp(CapsKey::new(keycode)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(test)]
    const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;

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
        assert_eq!(
            parse_shortcut("CapsLock+§"),
            CapsShortcut::Binding(CapsKey::new(10))
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
    fn state_machine_plain_tap_does_nothing() {
        let mut state = CapsState::default();
        let has_binding = |_: CapsKey| false;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
        );
    }

    #[test]
    fn state_machine_executes_configured_chord_once() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let has_binding = |candidate: CapsKey| candidate == key;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
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
            CapsDecision::Pass
        );
    }

    #[test]
    fn state_machine_executes_repeated_discrete_configured_chords() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let has_binding = |candidate: CapsKey| candidate == key;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Execute(key)
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyUp(key), has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Execute(key)
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyUp(key), has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::Pass
        );
    }

    #[test]
    fn state_machine_ignores_duplicate_caps_down_during_chord() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let has_binding = |candidate: CapsKey| candidate == key;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Execute(key)
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, true), has_binding),
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
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Execute(key)
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::Pass
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
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(key, false), has_binding),
            CapsDecision::Execute(key)
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::Pass
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
    fn state_machine_treats_unbound_key_as_chord_without_suppressing_key() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let has_binding = |_: CapsKey| false;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
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
            CapsDecision::Pass
        );
    }

    #[test]
    fn action_for_caps_down_arms_without_event_action() {
        let mut state = CapsState::default();
        let bindings = CapsBindings::new();

        let action = action_for_input(&mut state, CapsInput::CapsDown, &bindings);

        assert!(!action.suppress);
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
        assert!(!caps_action.suppress);

        let chord_action = action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);

        assert!(chord_action.suppress);
        assert!(matches!(
            chord_action.commands,
            Some(ShortcutCommands::Single(command)) if command == "screencapture -i -c"
        ));
    }

    #[test]
    fn action_for_unbound_chord_does_not_suppress_key() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let bindings = CapsBindings::new();

        assert!(!action_for_input(&mut state, CapsInput::CapsDown, &bindings).suppress);
        let key_action = action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);

        assert!(!key_action.suppress);
        assert!(key_action.commands.is_none());

        let release_action = action_for_input(&mut state, CapsInput::CapsUp, &bindings);
        assert!(!release_action.suppress);
        assert!(release_action.commands.is_none());
    }

    #[test]
    fn action_for_configured_chord_returns_commands_only() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let mut bindings = CapsBindings::new();
        bindings.insert(key, CapsBinding {
            raw_shortcut: "CapsLock+S".to_string(),
            commands: ShortcutCommands::Single("screencapture -i -c".to_string()),
        });

        assert!(!action_for_input(&mut state, CapsInput::CapsDown, &bindings).suppress);
        let key_action = action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);

        assert!(key_action.suppress);
        assert!(key_action.commands.is_some());
    }

    #[test]
    fn remapped_caps_events_are_detected() {
        assert!(is_remapped_caps_lock_event(K_CG_EVENT_KEY_DOWN, 0x4F));
        assert!(is_remapped_caps_lock_event(K_CG_EVENT_KEY_UP, 0x4F));
        assert!(!is_remapped_caps_lock_event(K_CG_EVENT_KEY_DOWN, 0x50));
    }

    #[test]
    fn remapped_caps_helper_ignores_non_f18_keys() {
        assert!(!is_remapped_caps_lock_event(K_CG_EVENT_KEY_DOWN, 1));
        assert!(!is_remapped_caps_lock_event(
            K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT,
            KEY_F18,
        ));
        assert!(!is_remapped_caps_lock_event(K_CG_EVENT_FLAGS_CHANGED, KEY_F18));
    }

    #[test]
    fn tap_disabled_timeout_event_is_detected() {
        assert!(is_tap_disabled_event(K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT));
    }

    #[test]
    fn tap_disabled_user_input_event_is_detected() {
        assert!(is_tap_disabled_event(K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT));
    }

    #[test]
    fn tap_disabled_helper_ignores_normal_event_type() {
        assert!(!is_tap_disabled_event(K_CG_EVENT_KEY_DOWN));
    }
}
