//! Event Monitor for `MenuAnywhere`.
//!
//! This module provides a global event tap that monitors for the configured
//! mouse button + modifier key combination to trigger the menu display.

use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use barba_shared::config::{MenuAnywhereConfig, MenuAnywhereMouseButton};
use core_foundation::base::TCFType;
use core_foundation::mach_port::CFMachPort;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopCommonModes};
use objc::runtime::{Class, Object};
use objc::{msg_send, sel, sel_impl};

use super::menu_builder;

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
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
}

/// Point structure for Core Graphics.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

// Constants for event tap
const K_CG_HID_EVENT_TAP: u32 = 0;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;

// Event types
const K_CG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
const K_CG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;

// Modifier flags
const ALL_MODIFIER_FLAGS: u64 = 0x001E_0000; // Shift | Control | Option | Command

// Pre-computed configuration stored in atomics for fast access in callback
static EXPECTED_EVENT_TYPE: AtomicU32 = AtomicU32::new(0);
static REQUIRED_MODIFIERS: AtomicU64 = AtomicU64::new(0);

/// Starts the event monitor with the given configuration.
pub fn start(config: &MenuAnywhereConfig) {
    // Pre-compute and store configuration in atomics for fast callback access
    let expected_event = match config.mouse_button {
        MenuAnywhereMouseButton::RightClick => K_CG_EVENT_RIGHT_MOUSE_DOWN,
        MenuAnywhereMouseButton::MiddleClick => K_CG_EVENT_OTHER_MOUSE_DOWN,
    };
    EXPECTED_EVENT_TYPE.store(expected_event, Ordering::Relaxed);
    REQUIRED_MODIFIERS.store(config.required_modifier_flags(), Ordering::Relaxed);

    unsafe {
        let event_mask = 1u64 << expected_event;

        let tap = CGEventTapCreate(
            K_CG_HID_EVENT_TAP,
            K_CG_HEAD_INSERT_EVENT_TAP,
            K_CG_EVENT_TAP_OPTION_DEFAULT,
            event_mask,
            event_tap_callback,
            ptr::null_mut(),
        );

        if tap.is_null() {
            return;
        }

        let tap_port = CFMachPort::wrap_under_create_rule(tap.cast());
        let Ok(run_loop_source) = tap_port.create_runloop_source(0) else {
            return;
        };

        let run_loop = CFRunLoop::get_current();
        run_loop.add_source(&run_loop_source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        CFRunLoop::run_current();
    }
}

/// Fast callback - uses atomics instead of mutex for configuration.
extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    // Fast path: check event type first (atomic load is very fast)
    if event_type >= K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event.is_null()
        || event_type != EXPECTED_EVENT_TYPE.load(Ordering::Relaxed)
    {
        return event;
    }

    // Check modifiers
    let flags = unsafe { CGEventGetFlags(event) };
    if (flags & ALL_MODIFIER_FLAGS) != REQUIRED_MODIFIERS.load(Ordering::Relaxed) {
        return event;
    }

    // Get location and trigger menu
    let location = unsafe { CGEventGetLocation(event) };
    trigger_menu_display(location);

    ptr::null_mut()
}

/// Pending menu location for cross-thread communication.
static PENDING_MENU_LOCATION: Mutex<Option<(f64, f64)>> = Mutex::new(None);

/// Wrapper for raw pointer to be Send + Sync.
struct SendSyncPtr(*mut Object);
unsafe impl Send for SendSyncPtr {}
unsafe impl Sync for SendSyncPtr {}

/// Singleton dispatch helper.
static DISPATCH_HELPER: Mutex<Option<SendSyncPtr>> = Mutex::new(None);

fn trigger_menu_display(location: CGPoint) {
    if let Ok(mut loc) = PENDING_MENU_LOCATION.lock() {
        *loc = Some((location.x, location.y));
    }

    unsafe {
        let helper = get_or_create_dispatch_helper();
        if !helper.is_null() {
            let sel = sel!(showMenuAtPendingLocation);
            let _: () = msg_send![helper, performSelectorOnMainThread: sel withObject: ptr::null::<Object>() waitUntilDone: false];
        }
    }
}

#[allow(clippy::items_after_statements)]
fn get_or_create_dispatch_helper() -> *mut Object {
    if let Ok(guard) = DISPATCH_HELPER.lock()
        && let Some(SendSyncPtr(ptr)) = guard.as_ref()
        && !ptr.is_null()
    {
        return *ptr;
    }

    use objc::declare::ClassDecl;

    let superclass = Class::get("NSObject").expect("NSObject not found");

    if let Some(existing) = Class::get("BarbaMenuDispatchHelper") {
        let helper: *mut Object = unsafe { msg_send![existing, new] };
        if let Ok(mut guard) = DISPATCH_HELPER.lock() {
            *guard = Some(SendSyncPtr(helper));
        }
        return helper;
    }

    let Some(mut decl) = ClassDecl::new("BarbaMenuDispatchHelper", superclass) else {
        return ptr::null_mut();
    };

    extern "C" fn show_menu_at_pending_location(_this: &Object, _sel: objc::runtime::Sel) {
        let location = PENDING_MENU_LOCATION.lock().ok().and_then(|mut guard| guard.take());

        let Some((x, y)) = location else {
            return;
        };

        let screen_height = get_main_screen_height();
        let ns_point = NSPoint { x, y: screen_height - y };

        if let Ok(Some(menu)) = std::panic::catch_unwind(menu_builder::build_frontmost_app_menu) {
            show_menu_at_location(menu, ns_point);
        }
    }

    use objc::runtime::Sel;
    unsafe {
        decl.add_method(
            sel!(showMenuAtPendingLocation),
            show_menu_at_pending_location as extern "C" fn(&Object, Sel),
        );
    }

    let helper_class = decl.register();
    let helper: *mut Object = unsafe { msg_send![helper_class, new] };

    if let Ok(mut guard) = DISPATCH_HELPER.lock() {
        *guard = Some(SendSyncPtr(helper));
    }

    helper
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

#[repr(C)]
struct NSSize {
    width: f64,
    height: f64,
}

fn get_main_screen_height() -> f64 {
    unsafe {
        let Some(screen_class) = Class::get("NSScreen") else {
            return 1080.0;
        };

        let main_screen: *mut Object = msg_send![screen_class, mainScreen];
        if main_screen.is_null() {
            return 1080.0;
        }

        let frame: NSRect = msg_send![main_screen, frame];
        frame.size.height
    }
}

fn show_menu_at_location(menu: *mut Object, location: NSPoint) {
    unsafe {
        if !menu.is_null() {
            let _: bool = msg_send![
                menu,
                popUpMenuPositioningItem: ptr::null::<Object>()
                atLocation: location
                inView: ptr::null::<Object>()
            ];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modifier_flags() {
        assert_eq!(ALL_MODIFIER_FLAGS, 0x001E_0000);
    }

    #[test]
    fn test_cgpoint_is_repr_c() {
        assert_eq!(std::mem::size_of::<CGPoint>(), 16);
    }
}
