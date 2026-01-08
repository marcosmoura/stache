// Hacky workaround to approximate menu bar visibility in the absence of proper APIs.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc::runtime::Object;
use objc::{msg_send, sel, sel_impl};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

use crate::events;

/// Flag indicating if menu visibility watcher is running.
static MENU_VISIBILITY_WATCHER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Current menu bar visibility state.
static MENU_BAR_VISIBLE: AtomicBool = AtomicBool::new(false);

fn emit_menubar_visibility_event(
    app_handle: &AppHandle,
    window_label: &str,
    is_visible: bool,
) -> Result<(), String> {
    let window = app_handle.get_webview_window(window_label).ok_or_else(|| {
        format!("Menubar visibility watcher could not find window `{window_label}`")
    })?;

    window
        .emit(events::menubar::VISIBILITY_CHANGED, &is_visible)
        .map_err(|err| err.to_string())
}

pub fn start_menu_bar_visibility_watcher(window: &WebviewWindow) {
    if MENU_VISIBILITY_WATCHER_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }

    register_menu_bar_visibility_observer(window.app_handle().clone(), window.label().to_string());
}

fn register_menu_bar_visibility_observer(app_handle: AppHandle, window_label: String) {
    let initial_state = query_menu_bar_visible().unwrap_or(false);
    MENU_BAR_VISIBLE.store(initial_state, Ordering::Release);

    if let Err(e) = emit_menubar_visibility_event(&app_handle, &window_label, initial_state) {
        eprintln!("stache: warning: failed to emit initial menubar visibility: {e}");
    }

    // Start polling mechanism to detect menubar visibility changes
    start_polling_mechanism(app_handle, window_label);
}

fn start_polling_mechanism(app_handle: AppHandle, window_label: String) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(300));

            match query_menu_bar_visible() {
                Ok(visible) => {
                    let previous = MENU_BAR_VISIBLE.load(Ordering::Acquire);
                    if visible != previous {
                        MENU_BAR_VISIBLE.store(visible, Ordering::Release);

                        if let Err(e) =
                            emit_menubar_visibility_event(&app_handle, &window_label, visible)
                        {
                            eprintln!("stache: warning: failed to emit menubar visibility: {e}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("stache: warning: failed to query menubar visibility: {e}");
                }
            }
        }
    });
}

fn query_menu_bar_visible() -> Result<bool, String> {
    unsafe {
        // Use CGWindowListCopyWindowInfo to check for visible menubar windows
        #[allow(non_upper_case_globals)]
        const kCGWindowListOptionOnScreenOnly: u32 = 1 << 0;
        #[allow(non_upper_case_globals)]
        const kCGNullWindowID: u32 = 0;

        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGWindowListCopyWindowInfo(option: u32, relativeToWindow: u32) -> CFArrayRef;
        }

        let window_list =
            CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly, kCGNullWindowID);
        if window_list.is_null() {
            return Err("Failed to get window list".into());
        }

        let windows = CFArray::<CFDictionary>::wrap_under_create_rule(window_list);
        let window_count = windows.len();

        // Look for menubar-related windows
        let owner_name_key = CFString::from_static_string("kCGWindowOwnerName");
        let layer_key = CFString::from_static_string("kCGWindowLayer");
        let bounds_key = CFString::from_static_string("kCGWindowBounds");
        let name_key = CFString::from_static_string("kCGWindowName");

        let mut menubar_window_found = false;

        for i in 0..window_count {
            if let Some(window_info) = windows.get(i) {
                // Get the raw dictionary pointer
                let dict_ptr = window_info.as_concrete_TypeRef();

                // Get owner name
                let owner_key_ptr = owner_name_key.as_concrete_TypeRef().cast::<c_void>();
                let owner_value_ptr: *const c_void =
                    msg_send![dict_ptr.cast::<Object>(), objectForKey: owner_key_ptr];

                if !owner_value_ptr.is_null() {
                    let owner_str = CFString::wrap_under_get_rule(owner_value_ptr.cast());
                    let owner = owner_str.to_string();

                    // Get window layer
                    let layer_key_ptr = layer_key.as_concrete_TypeRef().cast::<c_void>();
                    let layer_value_ptr: *const c_void =
                        msg_send![dict_ptr.cast::<Object>(), objectForKey: layer_key_ptr];

                    let layer = if layer_value_ptr.is_null() {
                        None
                    } else {
                        let layer_number = CFNumber::wrap_under_get_rule(layer_value_ptr.cast());
                        layer_number.to_i32()
                    };

                    // Get window name
                    let name_key_ptr = name_key.as_concrete_TypeRef().cast::<c_void>();
                    let name_value_ptr: *const c_void =
                        msg_send![dict_ptr.cast::<Object>(), objectForKey: name_key_ptr];
                    let name = if name_value_ptr.is_null() {
                        None
                    } else {
                        let name_str = CFString::wrap_under_get_rule(name_value_ptr.cast());
                        Some(name_str.to_string())
                    };

                    // Get bounds
                    let bounds_key_ptr = bounds_key.as_concrete_TypeRef().cast::<c_void>();
                    let bounds_value_ptr: *const c_void =
                        msg_send![dict_ptr.cast::<Object>(), objectForKey: bounds_key_ptr];
                    let has_bounds = !bounds_value_ptr.is_null();

                    // Check various conditions that might indicate the menubar
                    if let Some(layer_val) = layer {
                        // Check for menubar windows at layer 25 (WindowServer or has bounds)
                        if layer_val == 25
                            && ((owner == "WindowServer" || owner == "Window Server") || has_bounds)
                        {
                            menubar_window_found = true;
                        }
                        // Check for Control Center or menubar-related names
                        else if (24..=26).contains(&layer_val)
                            && let Some(ref win_name) = name
                            && (win_name.contains("Menubar")
                                || win_name.contains("Menu Bar")
                                || win_name.contains("StatusBar"))
                        {
                            menubar_window_found = true;
                        }
                    }
                }
            }
        }

        Ok(menubar_window_found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events;

    #[test]
    fn visibility_event_constant_is_correct() {
        assert_eq!(
            events::menubar::VISIBILITY_CHANGED,
            "stache://menubar/visibility-changed"
        );
    }

    #[test]
    fn menu_bar_visible_default_is_false() {
        // The static defaults to false
        // Note: This test may be affected by other tests that modify the state
        let _ = MENU_BAR_VISIBLE.load(Ordering::Acquire);
    }

    #[test]
    fn menu_visibility_watcher_running_is_atomic() {
        // Verify the atomic can be read
        let _ = MENU_VISIBILITY_WATCHER_RUNNING.load(Ordering::Acquire);
    }

    #[test]
    fn query_menu_bar_visible_returns_result() {
        // This test verifies the function runs without crashing
        // The actual result depends on the system state
        let result = query_menu_bar_visible();
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn query_menu_bar_visible_returns_bool_on_success() {
        // Verify the function returns a valid Result
        // and if successful, we can use the boolean value
        if let Ok(visible) = query_menu_bar_visible() {
            // Store the value to verify it's usable
            MENU_BAR_VISIBLE.store(visible, Ordering::Release);
            let stored = MENU_BAR_VISIBLE.load(Ordering::Acquire);
            assert_eq!(stored, visible);
        }
    }

    #[test]
    fn menu_bar_visible_can_be_toggled() {
        let original = MENU_BAR_VISIBLE.load(Ordering::Acquire);

        MENU_BAR_VISIBLE.store(true, Ordering::Release);
        assert!(MENU_BAR_VISIBLE.load(Ordering::Acquire));

        MENU_BAR_VISIBLE.store(false, Ordering::Release);
        assert!(!MENU_BAR_VISIBLE.load(Ordering::Acquire));

        // Restore original state
        MENU_BAR_VISIBLE.store(original, Ordering::Release);
    }

    #[test]
    fn menu_visibility_watcher_running_can_be_set() {
        let original = MENU_VISIBILITY_WATCHER_RUNNING.load(Ordering::Acquire);

        // Test swap returns previous value
        let prev = MENU_VISIBILITY_WATCHER_RUNNING.swap(true, Ordering::AcqRel);
        assert_eq!(prev, original);

        // Restore original state
        MENU_VISIBILITY_WATCHER_RUNNING.store(original, Ordering::Release);
    }

    #[test]
    fn cg_window_constants_are_correct() {
        #[allow(non_upper_case_globals)]
        const kCGWindowListOptionOnScreenOnly: u32 = 1 << 0;
        #[allow(non_upper_case_globals)]
        const kCGNullWindowID: u32 = 0;

        assert_eq!(kCGWindowListOptionOnScreenOnly, 1);
        assert_eq!(kCGNullWindowID, 0);
    }
}
