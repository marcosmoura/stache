use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::mach_port::CFMachPort;
use core_foundation::number::CFNumber;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use core_foundation::string::CFString;

use crate::config::ShortcutCommands;
use crate::modules::hotkey::execute_shortcut_commands;

type CGEventRef = *mut c_void;
type CGEventSourceRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;
type IOHIDElementRef = *mut c_void;
type IOHIDManagerRef = *mut c_void;
type IOHIDValueRef = *mut c_void;

type CGEventTapCallBack = extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

type IOHIDValueCallback =
    extern "C" fn(context: *mut c_void, result: i32, sender: *mut c_void, value: IOHIDValueRef);

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
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
    fn CGEventSourceFlagsState(state_id: i32) -> u64;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> IOHIDManagerRef;
    fn IOHIDManagerSetDeviceMatching(manager: IOHIDManagerRef, matching: *const c_void);
    fn IOHIDManagerSetInputValueMatching(manager: IOHIDManagerRef, matching: *const c_void);
    fn IOHIDManagerRegisterInputValueCallback(
        manager: IOHIDManagerRef,
        callback: IOHIDValueCallback,
        context: *mut c_void,
    );
    fn IOHIDManagerScheduleWithRunLoop(
        manager: IOHIDManagerRef,
        run_loop: *mut c_void,
        run_loop_mode: *const c_void,
    );
    fn IOHIDManagerOpen(manager: IOHIDManagerRef, options: u32) -> i32;
    fn IOHIDValueGetElement(value: IOHIDValueRef) -> IOHIDElementRef;
    fn IOHIDValueGetIntegerValue(value: IOHIDValueRef) -> isize;
    fn IOHIDElementGetUsagePage(element: IOHIDElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IOHIDElementRef) -> u32;
}

const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const K_CG_EVENT_KEY_DOWN: u32 = 10;
const K_CG_EVENT_KEY_UP: u32 = 11;
const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
const K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE: i32 = 1;
const K_CG_EVENT_FLAG_MASK_ALPHA_SHIFT: u64 = 0x0001_0000;
const K_CG_KEYBOARD_EVENT_AUTOREPEAT: u32 = 8;
const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
const K_CG_EVENT_SOURCE_UNIX_PROCESS_ID: u32 = 41;
const K_CG_EVENT_SOURCE_USER_DATA: u32 = 42;
const HID_USAGE_PAGE_GENERIC_DESKTOP: u32 = 0x01;
const HID_USAGE_PAGE_KEYBOARD: u32 = 0x07;
const HID_USAGE_GENERIC_DESKTOP_KEYBOARD: u32 = 0x06;
const HID_USAGE_KEYBOARD_CAPS_LOCK: u32 = 0x39;
const KEY_CAPS_LOCK: i64 = 57;
const KEY_CAPS_LOCK_U16: u16 = 57;
const CAPS_RESTORE_DELAY_MILLIS: u64 = 80;
const SYNTHETIC_CAPS_EVENT_ALLOWANCE_MILLIS: u64 = 200;
const STACHE_SYNTHETIC_CAPS_MARKER: i64 = 0x5354_4341_5053;

static BINDINGS: Mutex<Option<CapsBindings>> = Mutex::new(None);
static EVENT_TAP: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static HID_MANAGER: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static CAPS_RESTORE_GENERATION: AtomicU64 = AtomicU64::new(0);
static SYNTHETIC_CAPS_EVENTS: Mutex<SyntheticCapsEventAllowance> =
    Mutex::new(SyntheticCapsEventAllowance { remaining: 0, expires_at: None });
static STATE: Mutex<CapsState> = Mutex::new(CapsState {
    mode: CapsMode::Idle,
    active_key: None,
    stable_caps_on: false,
    press_started_caps_on: false,
});
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

        EVENT_TAP.store(tap, Ordering::SeqCst);

        let tap_port = CFMachPort::wrap_under_create_rule(tap.cast());
        let Ok(run_loop_source) = tap_port.create_runloop_source(0) else {
            tracing::warn!("failed to create CapsLock event tap run loop source");
            EVENT_TAP.store(ptr::null_mut(), Ordering::SeqCst);
            INITIALIZED.store(false, Ordering::SeqCst);
            return;
        };

        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&run_loop_source, kCFRunLoopCommonModes);
        if !start_caps_lock_physical_monitor(&run_loop) {
            EVENT_TAP.store(ptr::null_mut(), Ordering::SeqCst);
            INITIALIZED.store(false, Ordering::SeqCst);
            return;
        }
        refresh_stable_caps_lock_state();
        CGEventTapEnable(tap, true);
        tracing::debug!("CapsLock keybinding event tap initialized");
        CFRunLoop::run_current();
    }
}

fn start_caps_lock_physical_monitor(run_loop: &CFRunLoop) -> bool {
    unsafe {
        let manager = IOHIDManagerCreate(ptr::null(), 0);
        if manager.is_null() {
            tracing::warn!("failed to create CapsLock IOHID monitor");
            return false;
        }

        let device_matching = hid_matching_dictionary(
            "DeviceUsagePage",
            HID_USAGE_PAGE_GENERIC_DESKTOP,
            "DeviceUsage",
            HID_USAGE_GENERIC_DESKTOP_KEYBOARD,
        );
        let value_matching = hid_matching_dictionary(
            "UsagePage",
            HID_USAGE_PAGE_KEYBOARD,
            "Usage",
            HID_USAGE_KEYBOARD_CAPS_LOCK,
        );

        IOHIDManagerSetDeviceMatching(manager, device_matching.as_concrete_TypeRef().cast());
        IOHIDManagerSetInputValueMatching(manager, value_matching.as_concrete_TypeRef().cast());
        IOHIDManagerRegisterInputValueCallback(
            manager,
            caps_lock_hid_value_callback,
            ptr::null_mut(),
        );

        let result = IOHIDManagerOpen(manager, 0);
        if result != 0 {
            tracing::warn!(result, "failed to open CapsLock IOHID monitor");
            CFRelease(manager.cast_const());
            return false;
        }

        IOHIDManagerScheduleWithRunLoop(
            manager,
            run_loop.as_concrete_TypeRef().cast(),
            kCFRunLoopCommonModes.cast(),
        );

        HID_MANAGER.store(manager, Ordering::SeqCst);
        tracing::debug!("CapsLock physical key monitor initialized");
        true
    }
}

fn hid_matching_dictionary(
    first_key: &str,
    first_value: u32,
    second_key: &str,
    second_value: u32,
) -> CFDictionary<CFType, CFType> {
    let first_key = CFString::new(first_key);
    let first_value = CFNumber::from(i64::from(first_value));
    let second_key = CFString::new(second_key);
    let second_value = CFNumber::from(i64::from(second_value));

    CFDictionary::from_CFType_pairs(&[
        (first_key.as_CFType(), first_value.as_CFType()),
        (second_key.as_CFType(), second_value.as_CFType()),
    ])
}

extern "C" fn caps_lock_hid_value_callback(
    _context: *mut c_void,
    _result: i32,
    _sender: *mut c_void,
    value: IOHIDValueRef,
) {
    if value.is_null() || !is_caps_lock_hid_value(value) {
        return;
    }

    let input = if unsafe { IOHIDValueGetIntegerValue(value) } == 0 {
        CapsInput::CapsUp
    } else {
        CapsInput::CapsDown
    };

    let Ok(mut state) = STATE.lock() else {
        tracing::warn!("CapsLock physical monitor unavailable because state is poisoned");
        return;
    };

    let ensure_caps_on = match state.handle_input(input, |_| false) {
        CapsDecision::EnsureCapsState(target_on) => Some(target_on),
        CapsDecision::Pass | CapsDecision::Suppress | CapsDecision::Execute(_) => None,
    };
    drop(state);

    if let Some(target_on) = ensure_caps_on {
        ensure_caps_lock_state(target_on);
    }
}

fn is_caps_lock_hid_value(value: IOHIDValueRef) -> bool {
    let element = unsafe { IOHIDValueGetElement(value) };
    !element.is_null()
        && unsafe { IOHIDElementGetUsagePage(element) } == HID_USAGE_PAGE_KEYBOARD
        && unsafe { IOHIDElementGetUsage(element) } == HID_USAGE_KEYBOARD_CAPS_LOCK
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

    if is_stache_synthetic_caps_event(event, keycode) {
        if consume_synthetic_caps_event_allowance(true, false) {
            return event;
        }

        return ptr::null_mut();
    }

    if is_physical_caps_lock_event(event_type, keycode) {
        if consume_synthetic_caps_event_allowance(false, is_current_process_event(event)) {
            return event;
        }

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

    if let Some(target_on) = action.ensure_caps_on {
        ensure_caps_lock_state(target_on);
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

fn is_stache_synthetic_caps_event(event: CGEventRef, keycode: i64) -> bool {
    keycode == KEY_CAPS_LOCK
        && unsafe { CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) }
            == STACHE_SYNTHETIC_CAPS_MARKER
}

fn is_current_process_event(event: CGEventRef) -> bool {
    let source_pid =
        unsafe { CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_UNIX_PROCESS_ID) };
    event_source_pid_matches_process(source_pid, std::process::id())
}

fn event_source_pid_matches_process(source_pid: i64, process_id: u32) -> bool {
    source_pid == i64::from(process_id)
}

const fn is_key_event(event_type: u32) -> bool {
    matches!(event_type, K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP)
}

const fn is_physical_caps_lock_event(event_type: u32, keycode: i64) -> bool {
    keycode == KEY_CAPS_LOCK
        && matches!(
            event_type,
            K_CG_EVENT_FLAGS_CHANGED | K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP
        )
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

fn refresh_stable_caps_lock_state() {
    let caps_on = caps_lock_is_on();
    let Ok(mut state) = STATE.lock() else {
        tracing::warn!("CapsLock state unavailable because state is poisoned");
        return;
    };

    state.set_caps_lock_state(caps_on);
}

fn ensure_caps_lock_state(target_on: bool) {
    let generation = next_caps_restore_generation();
    if let Err(err) =
        std::thread::Builder::new()
            .name("stache-caps-lock-restore".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(CAPS_RESTORE_DELAY_MILLIS));
                if !is_current_caps_restore_generation(generation) {
                    return;
                }

                if caps_lock_is_on() != target_on {
                    synthesize_caps_lock_tap();
                }

                if !is_current_caps_restore_generation(generation) {
                    return;
                }

                let Ok(mut state) = STATE.lock() else {
                    tracing::warn!("CapsLock restore could not update poisoned state");
                    return;
                };
                state.set_caps_lock_state(target_on);
            })
    {
        tracing::warn!(error = %err, "failed to spawn CapsLock restore thread");
    }
}

fn next_caps_restore_generation() -> u64 {
    CAPS_RESTORE_GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

fn is_current_caps_restore_generation(generation: u64) -> bool {
    CAPS_RESTORE_GENERATION.load(Ordering::SeqCst) == generation
}

fn caps_lock_is_on() -> bool {
    let flags = unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE) };
    flags & K_CG_EVENT_FLAG_MASK_ALPHA_SHIFT != 0
}

fn synthesize_caps_lock_tap() {
    unsafe {
        let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE);
        let down_event = CGEventCreateKeyboardEvent(source, KEY_CAPS_LOCK_U16, true);
        let up_event = CGEventCreateKeyboardEvent(source, KEY_CAPS_LOCK_U16, false);

        if down_event.is_null() || up_event.is_null() {
            tracing::warn!("failed to create synthetic CapsLock keyboard event");
        }

        let synthetic_event_count = u8::from(!down_event.is_null()) + u8::from(!up_event.is_null());
        if synthetic_event_count > 0 {
            allow_next_synthetic_caps_events(synthetic_event_count);
        }

        if !down_event.is_null() {
            mark_synthetic_caps_event(down_event);
            CGEventPost(K_CG_HID_EVENT_TAP, down_event);
            CFRelease(down_event.cast_const());
        }

        if !up_event.is_null() {
            mark_synthetic_caps_event(up_event);
            CGEventPost(K_CG_HID_EVENT_TAP, up_event);
            CFRelease(up_event.cast_const());
        }

        if !source.is_null() {
            CFRelease(source.cast_const());
        }
    }
}

fn allow_next_synthetic_caps_events(event_count: u8) {
    let Ok(mut allowance) = SYNTHETIC_CAPS_EVENTS.lock() else {
        tracing::warn!("CapsLock restore could not arm synthetic event allowance");
        return;
    };

    allowance.arm(Instant::now(), event_count);
}

fn consume_synthetic_caps_event_allowance(
    has_synthetic_marker: bool,
    source_pid_matches_process: bool,
) -> bool {
    let Ok(mut allowance) = SYNTHETIC_CAPS_EVENTS.lock() else {
        tracing::warn!("CapsLock restore could not read synthetic event allowance");
        return false;
    };

    should_pass_synthetic_caps_event(
        has_synthetic_marker,
        source_pid_matches_process,
        &mut allowance,
        Instant::now(),
    )
}

fn should_pass_synthetic_caps_event(
    has_synthetic_marker: bool,
    source_pid_matches_process: bool,
    allowance: &mut SyntheticCapsEventAllowance,
    now: Instant,
) -> bool {
    if !has_synthetic_marker && !source_pid_matches_process {
        return false;
    }

    allowance.consume(now)
}

unsafe fn mark_synthetic_caps_event(event: CGEventRef) {
    unsafe {
        CGEventSetIntegerValueField(
            event,
            K_CG_EVENT_SOURCE_USER_DATA,
            STACHE_SYNTHETIC_CAPS_MARKER,
        );
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
    stable_caps_on: bool,
    press_started_caps_on: bool,
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
    EnsureCapsState(bool),
}

impl CapsState {
    fn set_caps_lock_state(&mut self, caps_on: bool) {
        self.stable_caps_on = caps_on;
        if self.mode == CapsMode::Idle {
            self.press_started_caps_on = caps_on;
        }
    }

    fn handle_input(
        &mut self,
        input: CapsInput,
        has_binding: impl Fn(CapsKey) -> bool,
    ) -> CapsDecision {
        match input {
            CapsInput::CapsDown => {
                if self.mode == CapsMode::Idle {
                    self.mode = CapsMode::CapsHeld;
                    self.active_key = None;
                    self.press_started_caps_on = self.stable_caps_on;
                }
                CapsDecision::Pass
            }
            CapsInput::CapsUp => match self.mode {
                CapsMode::CapsHeld => {
                    let target_on = !self.press_started_caps_on;
                    self.mode = CapsMode::Idle;
                    self.active_key = None;
                    self.stable_caps_on = target_on;
                    CapsDecision::EnsureCapsState(target_on)
                }
                CapsMode::ChordUsed => {
                    let target_on = self.press_started_caps_on;
                    self.mode = CapsMode::Idle;
                    self.stable_caps_on = target_on;
                    CapsDecision::EnsureCapsState(target_on)
                }
                CapsMode::Idle => CapsDecision::Pass,
            },
            CapsInput::KeyDown(key, _) if key == CapsKey::new(KEY_CAPS_LOCK) => {
                CapsDecision::Suppress
            }
            CapsInput::KeyDown(key, true) if self.active_key == Some(key) => CapsDecision::Suppress,
            CapsInput::KeyDown(key, is_repeat) => match self.mode {
                CapsMode::CapsHeld
                    if has_binding(key) && !is_repeat && self.active_key.is_none() =>
                {
                    self.mode = CapsMode::ChordUsed;
                    self.active_key = Some(key);
                    CapsDecision::Execute(key)
                }
                CapsMode::CapsHeld if !is_repeat && self.active_key.is_none() => {
                    self.mode = CapsMode::ChordUsed;
                    CapsDecision::Pass
                }
                CapsMode::ChordUsed
                    if has_binding(key) && !is_repeat && self.active_key.is_none() =>
                {
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
    ensure_caps_on: Option<bool>,
    commands: Option<ShortcutCommands>,
}

#[derive(Debug, Default)]
struct SyntheticCapsEventAllowance {
    remaining: u8,
    expires_at: Option<Instant>,
}

impl SyntheticCapsEventAllowance {
    fn arm(&mut self, now: Instant, event_count: u8) {
        if event_count == 0 {
            self.clear();
            return;
        }

        self.remaining = event_count;
        self.expires_at = Some(now + Duration::from_millis(SYNTHETIC_CAPS_EVENT_ALLOWANCE_MILLIS));
    }

    fn consume(&mut self, now: Instant) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };

        if self.remaining == 0 || now > expires_at {
            self.clear();
            return false;
        }

        self.remaining -= 1;
        if self.remaining == 0 {
            self.expires_at = None;
        }

        true
    }

    const fn clear(&mut self) {
        self.remaining = 0;
        self.expires_at = None;
    }
}

fn action_for_input(
    state: &mut CapsState,
    input: CapsInput,
    bindings: &CapsBindings,
) -> CapsAction {
    let decision = state.handle_input(input, |key| bindings.contains_key(&key));

    match decision {
        CapsDecision::Pass => CapsAction::default(),
        CapsDecision::Suppress => CapsAction {
            suppress: true,
            ..CapsAction::default()
        },
        CapsDecision::EnsureCapsState(target_on) => CapsAction {
            suppress: false,
            ensure_caps_on: Some(target_on),
            commands: None,
        },
        CapsDecision::Execute(key) => CapsAction {
            suppress: true,
            ensure_caps_on: Some(state.press_started_caps_on),
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
    fn state_machine_ensures_caps_on_for_plain_tap_that_started_off() {
        let mut state = CapsState::default();
        let has_binding = |_: CapsKey| false;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::EnsureCapsState(true)
        );
    }

    #[test]
    fn state_machine_ensures_caps_off_for_plain_tap_that_started_on() {
        let mut state = CapsState::default();
        state.set_caps_lock_state(true);
        let has_binding = |_: CapsKey| false;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::EnsureCapsState(false)
        );
    }

    #[test]
    fn state_machine_ignores_duplicate_caps_key_down_during_plain_tap() {
        let mut state = CapsState::default();
        let caps_key = CapsKey::new(KEY_CAPS_LOCK);
        let has_binding = |_: CapsKey| false;

        assert_eq!(
            state.handle_input(CapsInput::CapsDown, has_binding),
            CapsDecision::Pass
        );
        assert_eq!(
            state.handle_input(CapsInput::KeyDown(caps_key, false), has_binding),
            CapsDecision::Suppress
        );
        assert_eq!(
            state.handle_input(CapsInput::CapsUp, has_binding),
            CapsDecision::EnsureCapsState(true)
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
            CapsDecision::EnsureCapsState(false)
        );
    }

    #[test]
    fn state_machine_restores_caps_on_after_chord_that_started_on() {
        let mut state = CapsState::default();
        state.set_caps_lock_state(true);
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
            CapsDecision::EnsureCapsState(true)
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
            CapsDecision::EnsureCapsState(false)
        );
    }

    #[test]
    fn state_machine_ignores_duplicate_physical_caps_down_during_chord() {
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
            CapsDecision::EnsureCapsState(false)
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
            CapsDecision::EnsureCapsState(false)
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
            CapsDecision::EnsureCapsState(false)
        );
    }

    #[test]
    fn action_for_physical_caps_down_arms_without_event_action() {
        let mut state = CapsState::default();
        let bindings = CapsBindings::new();

        let action = action_for_input(&mut state, CapsInput::CapsDown, &bindings);

        assert!(!action.suppress);
        assert!(action.ensure_caps_on.is_none());
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
    fn action_for_physical_caps_tap_requests_synthetic_caps_tap() {
        let mut state = CapsState::default();
        let bindings = CapsBindings::new();

        assert!(!action_for_input(&mut state, CapsInput::CapsDown, &bindings).suppress);
        let action = action_for_input(&mut state, CapsInput::CapsUp, &bindings);

        assert!(!action.suppress);
        assert_eq!(action.ensure_caps_on, Some(true));
        assert!(action.commands.is_none());
    }

    #[test]
    fn action_for_plain_caps_release_requests_synthetic_caps_tap() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let mut bindings = CapsBindings::new();
        bindings.insert(key, CapsBinding {
            raw_shortcut: "CapsLock+S".to_string(),
            commands: ShortcutCommands::Single("screencapture -i -c".to_string()),
        });

        assert!(!action_for_input(&mut state, CapsInput::CapsDown, &bindings).suppress);
        let release_action = action_for_input(&mut state, CapsInput::CapsUp, &bindings);

        assert!(!release_action.suppress);
        assert_eq!(release_action.ensure_caps_on, Some(true));
        assert!(release_action.commands.is_none());

        let next_key_action =
            action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);
        assert!(!next_key_action.suppress);
        assert!(next_key_action.commands.is_none());
    }

    #[test]
    fn action_for_configured_chord_uses_tracked_caps_prefix() {
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
        assert_eq!(key_action.ensure_caps_on, Some(false));
        assert!(key_action.commands.is_some());

        let release_action = action_for_input(&mut state, CapsInput::CapsUp, &bindings);
        assert!(!release_action.suppress);
        assert_eq!(release_action.ensure_caps_on, Some(false));
        assert!(release_action.commands.is_none());
    }

    #[test]
    fn action_for_configured_chord_that_started_on_restores_on_keydown() {
        let mut state = CapsState::default();
        state.set_caps_lock_state(true);
        let key = CapsKey::new(1);
        let mut bindings = CapsBindings::new();
        bindings.insert(key, CapsBinding {
            raw_shortcut: "CapsLock+S".to_string(),
            commands: ShortcutCommands::Single("screencapture -i -c".to_string()),
        });

        assert!(!action_for_input(&mut state, CapsInput::CapsDown, &bindings).suppress);
        let key_action = action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);

        assert!(key_action.suppress);
        assert_eq!(key_action.ensure_caps_on, Some(true));
        assert!(key_action.commands.is_some());
    }

    #[test]
    fn action_for_unbound_chord_does_not_toggle_caps_or_suppress_key() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let bindings = CapsBindings::new();

        assert!(!action_for_input(&mut state, CapsInput::CapsDown, &bindings).suppress);
        let key_action = action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);

        assert!(!key_action.suppress);
        assert!(key_action.ensure_caps_on.is_none());
        assert!(key_action.commands.is_none());

        let release_action = action_for_input(&mut state, CapsInput::CapsUp, &bindings);
        assert!(!release_action.suppress);
        assert_eq!(release_action.ensure_caps_on, Some(false));
        assert!(release_action.commands.is_none());
    }

    #[test]
    fn action_for_first_chord_does_not_toggle_caps() {
        let mut state = CapsState::default();
        let key = CapsKey::new(1);
        let mut bindings = CapsBindings::new();
        bindings.insert(key, CapsBinding {
            raw_shortcut: "CapsLock+S".to_string(),
            commands: ShortcutCommands::Single("screencapture -i -c".to_string()),
        });

        assert!(!action_for_input(&mut state, CapsInput::CapsDown, &bindings).suppress);
        let first_chord = action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);

        assert!(first_chord.suppress);
        assert_eq!(first_chord.ensure_caps_on, Some(false));
        assert!(first_chord.commands.is_some());

        assert!(action_for_input(&mut state, CapsInput::KeyUp(key), &bindings).suppress);
        let repeated_chord =
            action_for_input(&mut state, CapsInput::KeyDown(key, false), &bindings);

        assert!(repeated_chord.suppress);
        assert_eq!(repeated_chord.ensure_caps_on, Some(false));
        assert!(repeated_chord.commands.is_some());

        let release_action = action_for_input(&mut state, CapsInput::CapsUp, &bindings);
        assert!(!release_action.suppress);
        assert_eq!(release_action.ensure_caps_on, Some(false));
        assert!(release_action.commands.is_none());
    }

    #[test]
    fn physical_caps_events_are_suppressed() {
        assert!(is_physical_caps_lock_event(
            K_CG_EVENT_FLAGS_CHANGED,
            KEY_CAPS_LOCK,
        ));
        assert!(is_physical_caps_lock_event(K_CG_EVENT_KEY_DOWN, KEY_CAPS_LOCK,));
        assert!(is_physical_caps_lock_event(K_CG_EVENT_KEY_UP, KEY_CAPS_LOCK,));
    }

    #[test]
    fn physical_caps_event_helper_ignores_non_caps_events() {
        assert!(!is_physical_caps_lock_event(K_CG_EVENT_KEY_DOWN, 1));
        assert!(!is_physical_caps_lock_event(
            K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT,
            KEY_CAPS_LOCK,
        ));
    }

    #[test]
    fn stale_restore_generations_are_ignored() {
        let older = next_caps_restore_generation();
        let newer = next_caps_restore_generation();

        assert!(!is_current_caps_restore_generation(older));
        assert!(is_current_caps_restore_generation(newer));
    }

    #[test]
    fn synthetic_caps_allowance_passes_unmarked_restore_events() {
        let now = std::time::Instant::now();
        let mut allowance = SyntheticCapsEventAllowance::default();

        assert!(!allowance.consume(now));

        allowance.arm(now, 2);

        assert!(allowance.consume(now));
        assert!(allowance.consume(now));
        assert!(!allowance.consume(now));
    }

    #[test]
    fn synthetic_caps_allowance_uses_successfully_created_event_count() {
        let now = std::time::Instant::now();
        let mut allowance = SyntheticCapsEventAllowance::default();

        allowance.arm(now, 1);

        assert!(allowance.consume(now));
        assert!(!allowance.consume(now));
    }

    #[test]
    fn synthetic_caps_allowance_expires() {
        let now = std::time::Instant::now();
        let mut allowance = SyntheticCapsEventAllowance::default();

        allowance.arm(now, 2);
        let expired_at = now + Duration::from_millis(SYNTHETIC_CAPS_EVENT_ALLOWANCE_MILLIS + 1);

        assert!(!allowance.consume(expired_at));
    }

    #[test]
    fn marked_synthetic_caps_events_require_active_allowance() {
        let now = std::time::Instant::now();
        let mut allowance = SyntheticCapsEventAllowance::default();

        assert!(!should_pass_synthetic_caps_event(
            true,
            false,
            &mut allowance,
            now,
        ));

        allowance.arm(now, 1);
        assert!(should_pass_synthetic_caps_event(
            true,
            false,
            &mut allowance,
            now,
        ));
        assert!(!should_pass_synthetic_caps_event(
            true,
            false,
            &mut allowance,
            now,
        ));
    }

    #[test]
    fn unmarked_synthetic_caps_events_require_current_process_source() {
        let now = std::time::Instant::now();
        let mut allowance = SyntheticCapsEventAllowance::default();
        allowance.arm(now, 1);

        assert!(!should_pass_synthetic_caps_event(
            false,
            false,
            &mut allowance,
            now,
        ));
        assert!(should_pass_synthetic_caps_event(
            false,
            true,
            &mut allowance,
            now,
        ));
    }

    #[test]
    fn event_source_pid_must_match_current_process() {
        assert!(event_source_pid_matches_process(42, 42));
        assert!(!event_source_pid_matches_process(0, 42));
        assert!(!event_source_pid_matches_process(-1, 42));
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

    #[test]
    fn synthetic_caps_helper_ignores_non_caps_key() {
        assert!(!is_stache_synthetic_caps_event(ptr::null_mut(), 1));
    }
}
