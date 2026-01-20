//! Mouse event monitoring for the tiling v2 window manager.
//!
//! This module provides global mouse button state tracking using `CGEventTap`.
//! It's used to detect when the user starts/stops dragging or resizing windows,
//! allowing the tiling system to freeze layout during these operations and then
//! recalculate ratios when done.
//!
//! # How It Works
//!
//! The tiling system receives window move/resize events continuously while a
//! window is being dragged/resized. Without knowing when the drag ends, we can't
//! properly respond to user-initiated moves/resizes.
//!
//! This module tracks:
//! - Mouse button down/up state
//! - Allows detection of: drag started (move event + mouse down) and
//!   drag ended (mouse up after drag started)

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPort;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};

// ============================================================================
// FFI Declarations
// ============================================================================

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
}

// Constants for event tap configuration
const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;

// Mouse event types
const K_CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const K_CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const K_CG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
const K_CG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
const K_CG_EVENT_RIGHT_MOUSE_DRAGGED: u32 = 7;

// ============================================================================
// Global State
// ============================================================================

/// Whether the mouse monitor is initialized.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Whether the left mouse button is currently pressed.
static MOUSE_DOWN: AtomicBool = AtomicBool::new(false);

/// Callback to invoke when mouse button is released.
static MOUSE_UP_CALLBACK: Mutex<Option<fn()>> = Mutex::new(None);

/// Counter for drag operations (incremented on each mouse down).
static DRAG_SEQUENCE: AtomicU32 = AtomicU32::new(0);

// ============================================================================
// Public API
// ============================================================================

/// Returns whether a mouse button is currently pressed.
#[must_use]
pub fn is_mouse_down() -> bool { MOUSE_DOWN.load(Ordering::SeqCst) }

/// Returns the current drag sequence number.
///
/// This is incremented each time a mouse button is pressed, allowing
/// callers to detect if a new drag operation has started.
#[must_use]
pub fn drag_sequence() -> u32 { DRAG_SEQUENCE.load(Ordering::SeqCst) }

/// Sets the callback to invoke when the mouse button is released.
///
/// This callback is called once per mouse-up event, on the mouse monitor thread.
pub fn set_mouse_up_callback(callback: fn()) {
    if let Ok(mut cb) = MOUSE_UP_CALLBACK.lock() {
        *cb = Some(callback);
    }
}

/// Clears the mouse-up callback.
pub fn clear_mouse_up_callback() {
    if let Ok(mut cb) = MOUSE_UP_CALLBACK.lock() {
        *cb = None;
    }
}

/// Initializes the mouse event monitor.
///
/// This spawns a background thread that sets up a `CGEventTap` to monitor
/// all mouse button down/up events globally.
///
/// # Returns
///
/// `true` if initialization succeeded or was already initialized.
pub fn init() -> bool {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        // Already initialized
        return true;
    }

    // Start the event tap in a separate thread
    std::thread::Builder::new()
        .name("stache-tiling-v2-mouse".into())
        .spawn(start_mouse_event_tap)
        .map_or_else(
            |e| {
                eprintln!("stache: tiling: mouse monitor thread spawn failed: {e}");
                INITIALIZED.store(false, Ordering::SeqCst);
                false
            },
            |_| true,
        )
}

/// Returns whether the mouse monitor is initialized.
#[must_use]
pub fn is_initialized() -> bool { INITIALIZED.load(Ordering::SeqCst) }

// ============================================================================
// Event Tap Implementation
// ============================================================================

/// Starts the Core Graphics event tap to monitor mouse button events.
fn start_mouse_event_tap() {
    unsafe {
        // Create an event tap for mouse button and drag events
        let event_mask = (1u64 << K_CG_EVENT_LEFT_MOUSE_DOWN)
            | (1u64 << K_CG_EVENT_LEFT_MOUSE_UP)
            | (1u64 << K_CG_EVENT_RIGHT_MOUSE_DOWN)
            | (1u64 << K_CG_EVENT_RIGHT_MOUSE_UP)
            | (1u64 << K_CG_EVENT_LEFT_MOUSE_DRAGGED)
            | (1u64 << K_CG_EVENT_RIGHT_MOUSE_DRAGGED);

        let tap = CGEventTapCreate(
            K_CG_HID_EVENT_TAP,
            K_CG_HEAD_INSERT_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
            event_mask,
            mouse_event_callback,
            std::ptr::null_mut(),
        );

        if tap.is_null() {
            eprintln!(
                "stache: tiling: mouse monitor: failed to create event tap - check accessibility permissions"
            );
            INITIALIZED.store(false, Ordering::SeqCst);
            return;
        }

        // Wrap the tap in a CFMachPort and create a run loop source
        let tap_port = CFMachPort::wrap_under_create_rule(tap.cast());
        let Ok(run_loop_source) = tap_port.create_runloop_source(0) else {
            eprintln!("stache: tiling: mouse monitor: failed to create run loop source");
            INITIALIZED.store(false, Ordering::SeqCst);
            return;
        };

        // Add the source to the current run loop
        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&run_loop_source, kCFRunLoopCommonModes);

        // Enable the event tap
        CGEventTapEnable(tap, true);

        eprintln!("stache: tiling: mouse monitor initialized");

        // Run the run loop (this blocks)
        CFRunLoop::run_current();
    }
}

/// Callback function for the mouse event tap.
extern "C" fn mouse_event_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    match event_type {
        K_CG_EVENT_LEFT_MOUSE_DOWN | K_CG_EVENT_RIGHT_MOUSE_DOWN => {
            MOUSE_DOWN.store(true, Ordering::SeqCst);
            DRAG_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        }
        K_CG_EVENT_LEFT_MOUSE_UP | K_CG_EVENT_RIGHT_MOUSE_UP => {
            let was_down = MOUSE_DOWN.swap(false, Ordering::SeqCst);

            // Only invoke callback if we were tracking a drag
            if was_down
                && let Ok(cb) = MOUSE_UP_CALLBACK.lock()
                && let Some(callback) = *cb
            {
                callback();
            }
        }
        K_CG_EVENT_LEFT_MOUSE_DRAGGED | K_CG_EVENT_RIGHT_MOUSE_DRAGGED => {
            // Drag events confirm mouse is still down
            MOUSE_DOWN.store(true, Ordering::SeqCst);
        }
        _ => {}
    }

    event
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_constants() {
        assert_eq!(K_CG_EVENT_LEFT_MOUSE_DOWN, 1);
        assert_eq!(K_CG_EVENT_LEFT_MOUSE_UP, 2);
        assert_eq!(K_CG_EVENT_RIGHT_MOUSE_DOWN, 3);
        assert_eq!(K_CG_EVENT_RIGHT_MOUSE_UP, 4);
        assert_eq!(K_CG_EVENT_LEFT_MOUSE_DRAGGED, 6);
        assert_eq!(K_CG_EVENT_RIGHT_MOUSE_DRAGGED, 7);
    }

    #[test]
    fn test_initial_state() {
        // Just verify we can call the functions without panicking
        let _ = is_mouse_down();
        let _ = drag_sequence();
    }

    #[test]
    fn test_callback_management() {
        fn dummy_callback() {}
        set_mouse_up_callback(dummy_callback);
        clear_mouse_up_callback();
    }
}
