//! Hold-to-Quit Module for Stache.
//!
//! This module provides a "hold ⌘Q to quit" feature that requires users to hold
//! the ⌘Q key combination for 1.5 seconds before quitting the frontmost application.
//! If the user only taps ⌘Q, an alert message is displayed instead.
//!
//! This is a Rust implementation inspired by the Hammerspoon `HoldToQuit` Spoon.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPort;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use objc::runtime::{BOOL, Class, Object, YES};
use objc::{msg_send, sel, sel_impl};
use tauri::Emitter;

use crate::events;

/// Duration (in seconds) required to hold ⌘Q before quitting.
const HOLD_DURATION_SECS: f64 = 1.5;

/// Polling interval when idle (not tracking a key press).
const IDLE_POLL_MS: u64 = 100;

/// Polling interval when actively tracking a key hold.
const ACTIVE_POLL_MS: u64 = 16;

/// Virtual key code for Q on macOS.
const KEY_Q: i64 = 12;

// FFI declarations for Core Graphics event tap functions
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
    fn CGEventGetFlags(event: CGEventRef) -> u64;
}

// Constants for event tap
const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;

// Event types
const K_CG_EVENT_KEY_DOWN: u32 = 10;
const K_CG_EVENT_KEY_UP: u32 = 11;

// Event field for keycode
const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

// Modifier flags
const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;
const K_CG_EVENT_FLAG_MASK_SHIFT: u64 = 0x0002_0000;
const K_CG_EVENT_FLAG_MASK_CONTROL: u64 = 0x0004_0000;
const K_CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 0x0008_0000;

/// State for tracking the ⌘Q key hold.
#[derive(Default)]
struct HoldState {
    /// When the key was pressed (None if not currently pressed).
    press_start: Option<Instant>,
    /// Whether we've already triggered the quit action for this press.
    quit_triggered: bool,
}

/// Global state shared between the event tap callback and the timer thread.
static HOLD_STATE: Mutex<HoldState> = Mutex::new(HoldState {
    press_start: None,
    quit_triggered: false,
});

/// Flag to signal the timer thread to check for quit.
static CHECK_QUIT: AtomicBool = AtomicBool::new(false);

/// Flag indicating if the module is running.
static IS_RUNNING: AtomicBool = AtomicBool::new(false);

/// Tauri app handle for emitting events (stored when initialized).
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

/// Initializes the Hold-to-Quit module.
///
/// This sets up a global event tap to intercept ⌘Q key events and implements
/// the hold-to-quit behavior.
///
/// # Arguments
/// * `app_handle` - The Tauri app handle for emitting events.
pub fn init(app_handle: tauri::AppHandle) {
    // Store the app handle for later use
    if let Ok(mut handle) = APP_HANDLE.lock() {
        *handle = Some(app_handle);
    }

    // Start the event tap in a separate thread
    std::thread::spawn(|| {
        start_event_tap();
    });

    // Start the timer thread that checks if we should quit
    std::thread::spawn(|| {
        timer_loop();
    });

    IS_RUNNING.store(true, Ordering::SeqCst);
}

/// Main timer loop that checks if the ⌘Q key has been held long enough.
///
/// Uses adaptive polling: longer intervals when idle to save CPU,
/// shorter intervals when actively tracking a key hold for responsiveness.
fn timer_loop() {
    loop {
        // Use adaptive polling: longer sleep when idle, shorter when active
        let poll_interval = if CHECK_QUIT.load(Ordering::SeqCst) {
            ACTIVE_POLL_MS
        } else {
            IDLE_POLL_MS
        };
        std::thread::sleep(Duration::from_millis(poll_interval));

        if !CHECK_QUIT.load(Ordering::SeqCst) {
            continue;
        }

        let should_quit = {
            let Ok(mut state) = HOLD_STATE.lock() else {
                continue;
            };

            state.press_start.is_some_and(|start| {
                let elapsed = start.elapsed().as_secs_f64();
                if elapsed >= HOLD_DURATION_SECS && !state.quit_triggered {
                    state.quit_triggered = true;
                    true
                } else {
                    false
                }
            })
        };

        if should_quit {
            kill_frontmost_app();
            CHECK_QUIT.store(false, Ordering::SeqCst);
        }
    }
}

/// Starts the Core Graphics event tap to intercept ⌘Q key events.
fn start_event_tap() {
    unsafe {
        // Create an event tap for key down and key up events
        let event_mask = (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_KEY_UP);

        let tap = CGEventTapCreate(
            K_CG_HID_EVENT_TAP,
            K_CG_HEAD_INSERT_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_DEFAULT,
            event_mask,
            event_tap_callback,
            std::ptr::null_mut(),
        );

        if tap.is_null() {
            eprintln!(
                "stache: cmd_q: failed to create event tap - check accessibility permissions"
            );
            return;
        }

        // Wrap the tap in a CFMachPort and create a run loop source
        let tap_port = CFMachPort::wrap_under_create_rule(tap.cast());
        let Ok(run_loop_source) = tap_port.create_runloop_source(0) else {
            eprintln!("stache: cmd_q: failed to create run loop source");
            return;
        };

        // Add the source to the current run loop
        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&run_loop_source, kCFRunLoopCommonModes);

        // Enable the event tap
        CGEventTapEnable(tap, true);

        // Run the run loop
        CFRunLoop::run_current();
    }
}

/// Callback function for the event tap.
///
/// This function is called for every key event and filters for ⌘Q specifically.
extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if event.is_null() {
        return event;
    }

    // Get the key code using FFI
    let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };

    // Check if it's the Q key
    if keycode != KEY_Q {
        return event;
    }

    // Get the modifier flags
    let flags = unsafe { CGEventGetFlags(event) };

    // Check if Command is pressed (and other modifiers are NOT pressed)
    let cmd_pressed = (flags & K_CG_EVENT_FLAG_MASK_COMMAND) != 0;
    let shift_pressed = (flags & K_CG_EVENT_FLAG_MASK_SHIFT) != 0;
    let control_pressed = (flags & K_CG_EVENT_FLAG_MASK_CONTROL) != 0;
    let alt_pressed = (flags & K_CG_EVENT_FLAG_MASK_ALTERNATE) != 0;

    let cmd_only = cmd_pressed && !shift_pressed && !control_pressed && !alt_pressed;

    if !cmd_only {
        return event;
    }

    match event_type {
        K_CG_EVENT_KEY_DOWN => {
            on_key_down();
            // Return null to suppress the event (prevent normal ⌘Q behavior)
            std::ptr::null_mut()
        }
        K_CG_EVENT_KEY_UP => {
            on_key_up();
            // Return null to suppress the event
            std::ptr::null_mut()
        }
        _ => event,
    }
}

/// Called when ⌘Q is pressed down.
fn on_key_down() {
    if let Ok(mut state) = HOLD_STATE.lock() {
        // Only start timing if we're not already timing (avoid key repeat)
        if state.press_start.is_none() {
            state.press_start = Some(Instant::now());
            state.quit_triggered = false;
            CHECK_QUIT.store(true, Ordering::SeqCst);
        }
    }
}

/// Called when ⌘Q is released.
fn on_key_up() {
    let show_alert = {
        let Ok(mut state) = HOLD_STATE.lock() else {
            return;
        };

        let should_show = state.press_start.is_some() && !state.quit_triggered;

        // Reset state
        state.press_start = None;
        state.quit_triggered = false;
        drop(state);
        CHECK_QUIT.store(false, Ordering::SeqCst);

        should_show
    };

    if show_alert {
        show_hold_to_quit_alert();
    }
}

/// Gets the name of the frontmost application.
fn get_frontmost_app_name() -> Option<String> {
    unsafe {
        let workspace_class = Class::get("NSWorkspace")?;
        let workspace: *mut Object = msg_send![workspace_class, sharedWorkspace];

        if workspace.is_null() {
            return None;
        }

        let frontmost_app: *mut Object = msg_send![workspace, frontmostApplication];

        if frontmost_app.is_null() {
            return None;
        }

        let name: *mut Object = msg_send![frontmost_app, localizedName];

        if name.is_null() {
            return None;
        }

        let bytes: *const u8 = msg_send![name, UTF8String];

        if bytes.is_null() {
            return None;
        }

        Some(std::ffi::CStr::from_ptr(bytes.cast()).to_string_lossy().to_string())
    }
}

/// Kills the frontmost application.
fn kill_frontmost_app() {
    unsafe {
        let Some(workspace_class) = Class::get("NSWorkspace") else {
            eprintln!("stache: cmd_q: failed to get NSWorkspace class");
            return;
        };

        let workspace: *mut Object = msg_send![workspace_class, sharedWorkspace];

        if workspace.is_null() {
            eprintln!("stache: cmd_q: failed to get shared workspace");
            return;
        }

        let frontmost_app: *mut Object = msg_send![workspace, frontmostApplication];

        if frontmost_app.is_null() {
            eprintln!("stache: cmd_q: no frontmost application");
            return;
        }

        // Get the app name for logging
        let name: *mut Object = msg_send![frontmost_app, localizedName];
        let app_name = if name.is_null() {
            "Unknown".to_string()
        } else {
            let bytes: *const u8 = msg_send![name, UTF8String];
            if bytes.is_null() {
                "Unknown".to_string()
            } else {
                std::ffi::CStr::from_ptr(bytes.cast()).to_string_lossy().to_string()
            }
        };

        // Terminate the application
        let terminated: BOOL = msg_send![frontmost_app, terminate];

        if terminated == YES {
            println!("stache: cmd_q: quit application '{app_name}'");
        } else {
            // If terminate fails, try forceTerminate
            let force_terminated: BOOL = msg_send![frontmost_app, forceTerminate];
            if force_terminated == YES {
                println!("stache: cmd_q: force quit application '{app_name}'");
            } else {
                eprintln!("stache: cmd_q: failed to quit application '{app_name}'");
            }
        }
    }
}

/// Shows an alert indicating the user should hold ⌘Q to quit.
fn show_hold_to_quit_alert() {
    let app_name = get_frontmost_app_name().unwrap_or_else(|| "the app".to_string());
    let message = format!("Hold ⌘Q to quit {app_name}");

    // Try to emit a Tauri event for the frontend to display
    if let Ok(handle) = APP_HANDLE.lock()
        && let Some(app_handle) = handle.as_ref()
    {
        let _ = app_handle.emit(events::cmd_q::ALERT, &message);
    }

    // Fallback: print to console
    println!("stache: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hold_state_default() {
        let state = HoldState::default();
        assert!(state.press_start.is_none());
        assert!(!state.quit_triggered);
    }

    #[test]
    fn test_hold_state_can_track_time() {
        let state = HoldState {
            press_start: Some(Instant::now()),
            ..HoldState::default()
        };
        assert!(state.press_start.is_some());
    }

    #[test]
    fn test_hold_state_quit_triggered_flag() {
        let mut state = HoldState::default();
        assert!(!state.quit_triggered);
        state.quit_triggered = true;
        assert!(state.quit_triggered);
    }

    #[test]
    fn test_is_running_atomic() {
        // Test that IS_RUNNING is an atomic that can be read
        let _ = IS_RUNNING.load(Ordering::SeqCst);
    }

    #[test]
    fn test_check_quit_atomic() {
        // Test that CHECK_QUIT is an atomic that can be read
        let _ = CHECK_QUIT.load(Ordering::SeqCst);
    }
}
