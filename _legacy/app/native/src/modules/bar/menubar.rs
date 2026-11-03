// Hacky workaround to approximate menu bar visibility in the absence of proper APIs.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc::declare::ClassDecl;
#[cfg(test)]
use objc::runtime::YES;
use objc::runtime::{BOOL, Class, NO, Object, Sel};
use objc::{Message, class, msg_send, sel, sel_impl};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

use crate::events;
use crate::modules::bar::watcher::start_best_effort_refresh_watcher;
use crate::platform::objc::{nsstring, nsstring_to_string};

/// Flag indicating if menu visibility watcher is running.
static MENU_VISIBILITY_WATCHER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Current menu bar visibility state.
static MENU_BAR_VISIBLE: AtomicBool = AtomicBool::new(false);

const MENU_BAR_FALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WORKSPACE_REFRESH_NOTIFICATION_NAMES: [&str; 3] = [
    "NSWorkspaceActiveSpaceDidChangeNotification",
    "NSWorkspaceDidActivateApplicationNotification",
    "NSWorkspaceDidDeactivateApplicationNotification",
];
const APPLICATION_REFRESH_NOTIFICATION_NAMES: [&str; 3] = [
    "NSApplicationDidBecomeActiveNotification",
    "NSApplicationDidResignActiveNotification",
    "NSApplicationDidChangeScreenParametersNotification",
];
static MENU_BAR_REFRESH_SIGNAL: OnceLock<Sender<()>> = OnceLock::new();

#[repr(C)]
#[derive(Clone, Copy)]
struct NSPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NSRect {
    origin: NSPoint,
    size: NSSize,
}

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
    let (nsmenu, hover) = query_menu_bar_signals(app_handle.clone(), window_label.clone());
    let initial_state =
        resolve_menu_bar_visible(nsmenu, hover, query_menu_bar_visible, None).unwrap_or(false);
    MENU_BAR_VISIBLE.store(initial_state, Ordering::Release);

    if let Err(e) = emit_menubar_visibility_event(&app_handle, &window_label, initial_state) {
        tracing::warn!(error = %e, "failed to emit initial menubar visibility");
    }

    let last_visible = RefCell::new(initial_state);

    thread::spawn(move || {
        start_best_effort_refresh_watcher(
            "menubar-watcher",
            MENU_BAR_FALLBACK_POLL_INTERVAL,
            |sender| {
                if MENU_BAR_REFRESH_SIGNAL.set(sender).is_err() {
                    tracing::warn!("menubar refresh signal was already initialized");
                }

                register_menu_bar_visibility_observers()
            },
            || {
                let mut last_visible = last_visible.borrow_mut();
                refresh_menu_bar_visibility(&app_handle, &window_label, &mut last_visible);
            },
        );
    });
}

fn refresh_menu_bar_visibility(
    app_handle: &AppHandle,
    window_label: &str,
    last_visible: &mut bool,
) {
    let (nsmenu, hover) = query_menu_bar_signals(app_handle.clone(), window_label.to_string());
    let visible =
        resolve_menu_bar_visible(nsmenu, hover, query_menu_bar_visible, Some(*last_visible))
            .unwrap_or(*last_visible);
    if visible != *last_visible {
        *last_visible = visible;
        MENU_BAR_VISIBLE.store(visible, Ordering::Release);

        if let Err(e) = emit_menubar_visibility_event(app_handle, window_label, visible) {
            tracing::warn!(error = %e, "failed to emit menubar visibility");
        }
    }
}

fn register_menu_bar_visibility_observers() -> Result<(), String> {
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return Err("failed to get NSWorkspace shared workspace".to_string());
        }

        let workspace_center: *mut Object = msg_send![workspace, notificationCenter];
        if workspace_center.is_null() {
            return Err("failed to get NSWorkspace notification center".to_string());
        }

        let default_center: *mut Object = msg_send![class!(NSNotificationCenter), defaultCenter];
        if default_center.is_null() {
            return Err("failed to get NSNotificationCenter default center".to_string());
        }

        let observer = create_menu_bar_observer();

        for notification_name in WORKSPACE_REFRESH_NOTIFICATION_NAMES {
            register_notification_observer(
                workspace_center,
                observer,
                notification_name,
                sel!(handleMenuBarRefreshNotification:),
            );
        }

        for notification_name in APPLICATION_REFRESH_NOTIFICATION_NAMES {
            register_notification_observer(
                default_center,
                observer,
                notification_name,
                sel!(handleMenuBarRefreshNotification:),
            );
        }
    }

    Ok(())
}

fn register_notification_observer(
    center: *mut Object,
    observer: *mut Object,
    notification_name: &str,
    selector: Sel,
) {
    unsafe {
        let name = nsstring(notification_name);
        let _: () = msg_send![
            center,
            addObserver: observer
            selector: selector
            name: name
            object: std::ptr::null::<Object>()
        ];
    }
}

fn create_menu_bar_observer() -> *mut Object {
    unsafe {
        let superclass = class!(NSObject);
        let class_name = "StacheMenuBarObserver";

        let existing_class = Class::get(class_name);
        let observer_class = existing_class.unwrap_or_else(|| {
            let mut decl = ClassDecl::new(class_name, superclass)
                .expect("Failed to create StacheMenuBarObserver class");

            decl.add_method(
                sel!(handleMenuBarRefreshNotification:),
                handle_menu_bar_refresh_notification as extern "C" fn(&Object, Sel, *mut Object),
            );

            decl.register()
        });

        let instance: *mut Object = msg_send![observer_class, alloc];
        msg_send![instance, init]
    }
}

extern "C" fn handle_menu_bar_refresh_notification(
    _self: &Object,
    _cmd: Sel,
    notification: *mut Object,
) {
    unsafe {
        if !notification.is_null() {
            let name_obj: *mut Object = msg_send![notification, name];
            let name = nsstring_to_string(name_obj);
            if !name.is_empty() {
                tracing::debug!(notification = %name, "menubar: received refresh notification");
            }
        }
    }

    signal_menu_bar_refresh();
}

fn signal_menu_bar_refresh() {
    if let Some(sender) = MENU_BAR_REFRESH_SIGNAL.get() {
        let _ = sender.send(());
    }
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

/// Select the best-available menu-bar visibility, falling back from the
/// primary `NSMenu` query and the pointer-hover signal to the `CGWindowList`
/// heuristic, then to prior state.
///
/// The bar is reported visible when either the `NSMenu` query says the menu
/// bar is shown or the pointer is inside the menu bar row — the latter covers
/// auto-hide reveal, where on recent macOS the pointer enters the row before
/// the system marks the menu bar visible. It is reported hidden only when both
/// signals agree the menu bar is not visible. The `cg` fallback is lazily
/// evaluated — it is only called when neither signal can determine visibility.
/// Returns `None` only when all sources fail and there is no prior state.
///
/// Infrastructure dispatch failure (e.g. the main thread dying) will still
/// panic. This function only handles recoverable failures: a missing `ObjC`
/// class or a thrown exception during the message send.
fn resolve_menu_bar_visible(
    nsmenu: Option<bool>,
    hover: Option<bool>,
    cg: impl FnOnce() -> Result<bool, String>,
    prior: Option<bool>,
) -> Option<bool> {
    match (nsmenu, hover) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => cg().ok().or(prior),
    }
}

/// Convert a raw `Result<BOOL, E>` (from an `ObjC` message send) into an
/// `Option<bool>` — `Ok(YES)` → `Some(true)`, `Ok(NO)` → `Some(false)`,
/// `Err(_)` → `None`.
#[allow(clippy::needless_pass_by_value, clippy::option_if_let_else)]
fn nsmenu_bool_to_option<E: std::fmt::Debug>(result: Result<BOOL, E>) -> Option<bool> {
    match result {
        Ok(val) => Some(val != NO),
        Err(_) => None,
    }
}

/// Queries both `AppKit` visibility signals in one main-thread dispatch,
/// resolving the bar window's target screen frame first.
fn query_menu_bar_signals(
    app_handle: AppHandle,
    window_label: String,
) -> (Option<bool>, Option<bool>) {
    crate::platform::thread::dispatch_on_main_sync(move || {
        let frame = resolve_bar_screen_frame(&app_handle, &window_label);
        (
            query_nsmenu_visible_on_main(),
            frame.and_then(|frame| query_mouse_in_menu_bar_on_main(&frame)),
        )
    })
}

/// Resolves the `AppKit` frame of the screen hosting the bar window, falling
/// back to the main screen when the window has no screen yet.
///
/// Must be called on the macOS main thread (`AppKit`).
fn resolve_bar_screen_frame(app_handle: &AppHandle, window_label: &str) -> Option<NSRect> {
    unsafe {
        let window = app_handle.get_webview_window(window_label)?;
        let ns_window = window.ns_window().ok()?;
        let screen: *mut Object = msg_send![ns_window.cast::<Object>(), screen];
        if !screen.is_null() {
            return Some(msg_send![screen, frame]);
        }
        let main_screen: *mut Object = msg_send![class!(NSScreen), mainScreen];
        if main_screen.is_null() {
            return None;
        }
        Some(msg_send![main_screen, frame])
    }
}

fn query_nsmenu_visible_on_main() -> Option<bool> {
    // SAFETY: Class::get returns None if the class is not registered.
    // send_message on the class is safe because we are on the main thread,
    // required by AppKit. With the "exception" feature enabled, ObjC
    // exceptions are caught and returned as Err(MessageError).
    let cls = Class::get("NSMenu")?;
    let result: Result<BOOL, _> = unsafe { cls.send_message(sel!(menuBarVisible), ()) };
    nsmenu_bool_to_option(result)
}

/// Returns whether a pointer at `mouse` lies within the menu bar row at the
/// top of the screen with `frame` and the given menu bar height.
const fn mouse_in_menu_bar_row(mouse: NSPoint, frame: NSRect, menu_bar_height: f64) -> bool {
    let top = frame.origin.y + frame.size.height;
    mouse.y >= top - menu_bar_height && mouse.y <= top
}

/// Query whether the pointer currently sits inside the menu bar row of the
/// screen hosting the bar window. This covers auto-hide reveal, where on
/// recent macOS the pointer enters the row before the system marks the menu
/// bar visible.
///
/// Must be called on the macOS main thread. Returns `None` if an `AppKit`
/// object is unavailable or an Objective-C message throws an exception.
fn query_mouse_in_menu_bar_on_main(frame: &NSRect) -> Option<bool> {
    unsafe {
        // SAFETY: Class::get returns None if a class is not registered.
        // The messages are safe because we are on the main thread, required
        // by AppKit. With the "exception" feature enabled, ObjC exceptions
        // are caught and returned as Err(MessageError).
        let status_bar_class = Class::get("NSStatusBar")?;

        let status_bar: *mut Object = msg_send![status_bar_class, systemStatusBar];
        if status_bar.is_null() {
            return None;
        }

        let menu_bar_height: f64 = msg_send![status_bar, thickness];
        let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];

        Some(mouse_in_menu_bar_row(mouse, *frame, menu_bar_height))
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
    fn menu_bar_refresh_notification_names_cover_workspace_and_application_events() {
        assert_eq!(WORKSPACE_REFRESH_NOTIFICATION_NAMES, [
            "NSWorkspaceActiveSpaceDidChangeNotification",
            "NSWorkspaceDidActivateApplicationNotification",
            "NSWorkspaceDidDeactivateApplicationNotification",
        ]);
        assert_eq!(APPLICATION_REFRESH_NOTIFICATION_NAMES, [
            "NSApplicationDidBecomeActiveNotification",
            "NSApplicationDidResignActiveNotification",
            "NSApplicationDidChangeScreenParametersNotification",
        ]);
    }

    #[test]
    fn menu_bar_fallback_poll_interval_is_100ms() {
        assert_eq!(MENU_BAR_FALLBACK_POLL_INTERVAL, Duration::from_millis(100));
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

    // --- nsmenu_bool_to_option ---

    #[test]
    fn nsmenu_bool_to_option_maps_yes_to_some_true() {
        assert_eq!(super::nsmenu_bool_to_option::<()>(Ok(YES)), Some(true));
    }

    #[test]
    fn nsmenu_bool_to_option_maps_no_to_some_false() {
        assert_eq!(super::nsmenu_bool_to_option::<()>(Ok(NO)), Some(false));
    }

    #[test]
    fn nsmenu_bool_to_option_maps_error_to_none() {
        let result: Result<BOOL, String> = Err("failed".into());
        assert_eq!(super::nsmenu_bool_to_option(result), None);
    }

    // --- mouse_in_menu_bar_row ---

    const TEST_SCREEN: NSRect = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize { width: 1920.0, height: 1200.0 },
    };

    #[test]
    fn mouse_in_menu_bar_row_inside_is_true() {
        assert!(mouse_in_menu_bar_row(
            NSPoint { x: 960.0, y: 1190.0 },
            TEST_SCREEN,
            22.0
        ));
    }

    #[test]
    fn mouse_in_menu_bar_row_below_is_false() {
        assert!(!mouse_in_menu_bar_row(
            NSPoint { x: 960.0, y: 1177.0 },
            TEST_SCREEN,
            22.0
        ));
    }

    #[test]
    fn mouse_in_menu_bar_row_top_edge_is_included() {
        assert!(mouse_in_menu_bar_row(
            NSPoint { x: 10.0, y: 1200.0 },
            TEST_SCREEN,
            22.0
        ));
    }

    #[test]
    fn mouse_in_menu_bar_row_bottom_edge_is_included() {
        assert!(mouse_in_menu_bar_row(
            NSPoint { x: 10.0, y: 1178.0 },
            TEST_SCREEN,
            22.0
        ));
    }

    #[test]
    fn mouse_in_menu_bar_row_above_screen_is_false() {
        assert!(!mouse_in_menu_bar_row(
            NSPoint { x: 10.0, y: 1200.5 },
            TEST_SCREEN,
            22.0
        ));
    }

    #[test]
    fn mouse_in_menu_bar_row_uses_screen_origin() {
        let frame = NSRect {
            origin: NSPoint { x: 0.0, y: 100.0 },
            size: NSSize { width: 1920.0, height: 1200.0 },
        };
        assert!(mouse_in_menu_bar_row(
            NSPoint { x: 10.0, y: 1280.0 },
            frame,
            22.0
        ));
        assert!(!mouse_in_menu_bar_row(
            NSPoint { x: 10.0, y: 1277.0 },
            frame,
            22.0
        ));
    }

    #[test]
    fn mouse_in_menu_bar_row_uses_secondary_screen_frame() {
        // A screen placed above the primary (AppKit bottom-left origin). The
        // hover check must use the bar window's own screen frame, not the
        // hardcoded main screen.
        let primary = NSRect {
            origin: NSPoint { x: 0.0, y: 0.0 },
            size: NSSize { width: 1920.0, height: 1080.0 },
        };
        let secondary = NSRect {
            origin: NSPoint { x: 0.0, y: 1080.0 },
            size: NSSize { width: 2560.0, height: 1440.0 },
        };

        // Pointer at the very top of the secondary screen: inside its row...
        let mouse = NSPoint { x: 1280.0, y: 2516.0 };
        assert!(mouse_in_menu_bar_row(mouse, secondary, 24.0));
        // ...but far outside the primary screen's row.
        assert!(!mouse_in_menu_bar_row(mouse, primary, 24.0));

        // Just below the secondary screen's row is outside it.
        assert!(!mouse_in_menu_bar_row(
            NSPoint { x: 1280.0, y: 2495.0 },
            secondary,
            24.0
        ));
    }

    // --- resolve_menu_bar_visible ---

    #[test]
    fn resolve_visible_when_nsmenu_true() {
        assert_eq!(
            resolve_menu_bar_visible(Some(true), Some(false), || Ok(false), None),
            Some(true)
        );
        assert_eq!(
            resolve_menu_bar_visible(Some(true), None, || Ok(false), None),
            Some(true)
        );
    }

    #[test]
    fn resolve_hover_overrides_nsmenu_false() {
        // Auto-hide reveal: the pointer enters the menu bar row before the
        // system reports the menu bar visible (observed on macOS 26+).
        assert_eq!(
            resolve_menu_bar_visible(Some(false), Some(true), || Ok(false), None),
            Some(true)
        );
    }

    #[test]
    fn resolve_hidden_when_both_signals_agree_false() {
        assert_eq!(
            resolve_menu_bar_visible(Some(false), Some(false), || Ok(true), None),
            Some(false)
        );
    }

    #[test]
    fn resolve_falls_back_when_both_signals_unknown() {
        assert_eq!(
            resolve_menu_bar_visible(None, None, || Ok(true), None),
            Some(true)
        );
        assert_eq!(
            resolve_menu_bar_visible(None, None, || Ok(false), None),
            Some(false)
        );
    }

    #[test]
    fn resolve_falls_back_when_nsmenu_false_and_hover_unknown() {
        assert_eq!(
            resolve_menu_bar_visible(Some(false), None, || Ok(true), None),
            Some(true)
        );
    }

    #[test]
    fn resolve_visible_does_not_call_fallback() {
        let mut fallback_called = false;
        let result = resolve_menu_bar_visible(
            Some(false),
            Some(true),
            || {
                fallback_called = true;
                Ok(false)
            },
            None,
        );
        assert_eq!(result, Some(true));
        assert!(
            !fallback_called,
            "fallback should not run when visibility is known"
        );
    }

    #[test]
    fn resolve_hidden_does_not_call_fallback() {
        let mut fallback_called = false;
        let result = resolve_menu_bar_visible(
            Some(false),
            Some(false),
            || {
                fallback_called = true;
                Ok(true)
            },
            None,
        );
        assert_eq!(result, Some(false));
        assert!(
            !fallback_called,
            "fallback should not run when visibility is known"
        );
    }

    #[test]
    fn resolve_calls_fallback_when_both_signals_unknown() {
        let mut fallback_called = false;
        let result = resolve_menu_bar_visible(
            None,
            None,
            || {
                fallback_called = true;
                Ok(true)
            },
            None,
        );
        assert_eq!(result, Some(true));
        assert!(fallback_called, "fallback should run when visibility is unknown");
    }

    #[test]
    fn resolve_preserves_prior_when_all_fail() {
        assert_eq!(
            resolve_menu_bar_visible(None, None, || Err("fail".into()), Some(true)),
            Some(true)
        );
        assert_eq!(
            resolve_menu_bar_visible(None, None, || Err("fail".into()), Some(false)),
            Some(false)
        );
    }

    #[test]
    fn resolve_returns_none_when_all_fail_and_no_prior() {
        assert_eq!(
            resolve_menu_bar_visible(None, None, || Err("fail".into()), None),
            None
        );
    }
}
