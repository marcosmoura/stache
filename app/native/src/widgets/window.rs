//! Click-outside detection for the widgets window.
//!
//! This module provides global mouse click monitoring to detect when the user
//! clicks outside the widgets window. Since the window is non-focusable, we cannot
//! rely on focus events - instead we use a Core Graphics event tap to monitor
//! all mouse clicks and compare them against the window frame.

use std::ffi::c_void;
use std::sync::Mutex;

use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPort;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use tauri::{Emitter, Manager};

use crate::events;

// FFI declarations for Core Graphics event tap and mouse location
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CFMachPortRef = *mut c_void;

type CGEventTapCallBack = extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

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
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
}

// Constants for event tap configuration
const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;

// Mouse event types
const K_CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
const K_CG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const K_CG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;

/// Represents a window frame with position and size.
#[derive(Clone, Copy, Default)]
struct WindowFrame {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl WindowFrame {
    /// Check if a point is inside this frame.
    const fn contains(&self, point: CGPoint) -> bool {
        point.x >= self.x
            && point.x <= self.x + self.width
            && point.y >= self.y
            && point.y <= self.y + self.height
    }
}

/// Global state for the click-outside monitor.
struct ClickOutsideState {
    app_handle: Option<tauri::AppHandle>,
}

static STATE: Mutex<ClickOutsideState> = Mutex::new(ClickOutsideState { app_handle: None });

/// Initialize click-outside monitoring for the widgets window.
///
/// This spawns a background thread that sets up a Core Graphics event tap
/// to monitor all mouse clicks globally. When a click is detected outside
/// the widgets window (and the window is visible), an event is emitted.
pub fn monitor_click_outside(app: &tauri::App) {
    // Store the app handle for use in the callback
    if let Ok(mut state) = STATE.lock() {
        state.app_handle = Some(app.handle().clone());
    }

    // Start the event tap in a separate thread
    std::thread::spawn(|| {
        start_mouse_event_tap();
    });
}

/// Starts the Core Graphics event tap to monitor mouse clicks.
fn start_mouse_event_tap() {
    unsafe {
        // Create an event tap for all mouse down events
        let event_mask = (1u64 << K_CG_EVENT_LEFT_MOUSE_DOWN)
            | (1u64 << K_CG_EVENT_RIGHT_MOUSE_DOWN)
            | (1u64 << K_CG_EVENT_OTHER_MOUSE_DOWN);

        let tap = CGEventTapCreate(
            K_CG_HID_EVENT_TAP,
            K_CG_HEAD_INSERT_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_LISTEN_ONLY, // Listen only, don't modify events
            event_mask,
            mouse_event_callback,
            std::ptr::null_mut(),
        );

        if tap.is_null() {
            eprintln!(
                "stache: widgets: failed to create mouse event tap - check accessibility permissions"
            );
            return;
        }

        // Wrap the tap in a CFMachPort and create a run loop source
        let tap_port = CFMachPort::wrap_under_create_rule(tap.cast());
        let Ok(run_loop_source) = tap_port.create_runloop_source(0) else {
            eprintln!("stache: widgets: failed to create run loop source");
            return;
        };

        // Add the source to the current run loop
        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&run_loop_source, kCFRunLoopCommonModes);

        // Enable the event tap
        CGEventTapEnable(tap, true);

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
    // Only handle mouse down events
    if event_type != K_CG_EVENT_LEFT_MOUSE_DOWN
        && event_type != K_CG_EVENT_RIGHT_MOUSE_DOWN
        && event_type != K_CG_EVENT_OTHER_MOUSE_DOWN
    {
        return event;
    }

    if event.is_null() {
        return event;
    }

    // Get the mouse location
    let location = unsafe { CGEventGetLocation(event) };

    // Check if we should emit the click-outside event
    let Ok(state) = STATE.lock() else {
        return event;
    };

    let Some(ref app_handle) = state.app_handle else {
        return event;
    };

    let Some(window) = app_handle.get_webview_window("widgets") else {
        return event;
    };

    // Only check if the window is visible
    let Ok(is_visible) = window.is_visible() else {
        return event;
    };

    if !is_visible {
        return event;
    }

    // Get the window frame
    let frame = get_window_frame(&window);

    // If the click is outside the window, emit the event
    if !frame.contains(location)
        && let Err(e) = app_handle.emit(events::widgets::CLICK_OUTSIDE, ())
    {
        eprintln!("stache: warning: failed to emit click-outside event: {e}");
    }

    event
}

/// Get the window frame in screen coordinates.
fn get_window_frame(window: &tauri::WebviewWindow) -> WindowFrame {
    let scale = window.scale_factor().unwrap_or(1.0);

    let position = window
        .outer_position()
        .map(|p| (f64::from(p.x) / scale, f64::from(p.y) / scale))
        .unwrap_or((0.0, 0.0));

    let size = window
        .outer_size()
        .map(|s| (f64::from(s.width) / scale, f64::from(s.height) / scale))
        .unwrap_or((0.0, 0.0));

    WindowFrame {
        x: position.0,
        y: position.1,
        width: size.0,
        height: size.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_frame_contains_point_inside() {
        let frame = WindowFrame {
            x: 100.0,
            y: 100.0,
            width: 200.0,
            height: 150.0,
        };

        // Point clearly inside
        assert!(frame.contains(CGPoint { x: 150.0, y: 150.0 }));

        // Point on the left edge
        assert!(frame.contains(CGPoint { x: 100.0, y: 150.0 }));

        // Point on the top edge
        assert!(frame.contains(CGPoint { x: 150.0, y: 100.0 }));

        // Point on the right edge
        assert!(frame.contains(CGPoint { x: 300.0, y: 150.0 }));

        // Point on the bottom edge
        assert!(frame.contains(CGPoint { x: 150.0, y: 250.0 }));

        // Point on corner
        assert!(frame.contains(CGPoint { x: 100.0, y: 100.0 }));
    }

    #[test]
    fn window_frame_does_not_contain_point_outside() {
        let frame = WindowFrame {
            x: 100.0,
            y: 100.0,
            width: 200.0,
            height: 150.0,
        };

        // Point to the left
        assert!(!frame.contains(CGPoint { x: 50.0, y: 150.0 }));

        // Point to the right
        assert!(!frame.contains(CGPoint { x: 350.0, y: 150.0 }));

        // Point above
        assert!(!frame.contains(CGPoint { x: 150.0, y: 50.0 }));

        // Point below
        assert!(!frame.contains(CGPoint { x: 150.0, y: 300.0 }));

        // Point at origin
        assert!(!frame.contains(CGPoint { x: 0.0, y: 0.0 }));
    }

    #[test]
    fn default_window_frame_is_zero() {
        let frame = WindowFrame::default();
        assert!((frame.x).abs() < f64::EPSILON);
        assert!((frame.y).abs() < f64::EPSILON);
        assert!((frame.width).abs() < f64::EPSILON);
        assert!((frame.height).abs() < f64::EPSILON);
    }

    #[test]
    fn event_constants_are_valid() {
        assert_eq!(K_CG_EVENT_LEFT_MOUSE_DOWN, 1);
        assert_eq!(K_CG_EVENT_RIGHT_MOUSE_DOWN, 3);
        assert_eq!(K_CG_EVENT_OTHER_MOUSE_DOWN, 25);
    }

    #[test]
    fn tap_constants_are_valid() {
        assert_eq!(K_CG_HID_EVENT_TAP, 0);
        assert_eq!(K_CG_HEAD_INSERT_EVENT_TAP, 0);
        assert_eq!(K_CG_EVENT_TAP_OPTION_LISTEN_ONLY, 1);
    }
}
