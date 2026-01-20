//! Standalone AXObserver system for tiling.
//!
//! This module provides AXObserver functionality to receive notifications
//! about window events from macOS Accessibility APIs.
//!
//! # Thread Safety
//!
//! The observer system uses a single-threaded model where all observer operations
//! happen on the main thread. The global state is protected by a mutex for
//! thread-safe access.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use core_foundation::base::TCFType;
use core_foundation::runloop::CFRunLoop;
use core_foundation::string::CFString;
use parking_lot::Mutex;

use super::types::{WindowEvent, WindowEventType};

// ============================================================================
// Thread-Safe Wrapper
// ============================================================================

/// Wrapper for AXObserverRef that's Send + Sync.
///
/// This is safe because AXObserver operations are only performed on the main thread,
/// and we only store/retrieve references under a lock.
#[derive(Clone, Copy)]
struct ObserverRef(*mut c_void);

// SAFETY: AXObserver is only accessed from the main thread via CFRunLoop.
// The mutex ensures exclusive access to the map.
unsafe impl Send for ObserverRef {}
unsafe impl Sync for ObserverRef {}

// ============================================================================
// Observer Filtering
// ============================================================================

/// Bundle IDs of apps that should not have observers created.
const SKIP_OBSERVER_BUNDLE_IDS: &[&str] = &[
    "com.apple.dock",
    "com.apple.SystemUIServer",
    "com.apple.controlcenter",
    "com.apple.notificationcenterui",
    "com.apple.Spotlight",
    "com.apple.WindowManager",
    "com.apple.loginwindow",
    "com.apple.screencaptureui",
    "com.apple.screensaver",
    "com.apple.SecurityAgent",
    "com.apple.UserNotificationCenter",
    "com.apple.universalcontrol",
    "com.apple.TouchBarServer",
    "com.apple.AirPlayUIAgent",
    "com.apple.wifi.WiFiAgent",
    "com.apple.bluetoothUIServer",
    "com.apple.CoreLocationAgent",
    "com.apple.VoiceOver",
    "com.apple.AssistiveControl",
    "com.apple.SpeechRecognitionCore",
    "com.apple.accessibility.universalAccessAuthWarn",
    "com.apple.launchpad.launcher",
    "com.apple.FolderActionsDispatcher",
    "com.marcosmoura.stache",
];

/// App names to skip observing when bundle ID is not available.
const SKIP_OBSERVER_APP_NAMES: &[&str] = &[
    "Dock",
    "SystemUIServer",
    "Control Center",
    "Notification Center",
    "Spotlight",
    "Window Manager",
    "WindowManager",
    "loginwindow",
    "Stache",
    "JankyBorders",
    "borders",
];

// ============================================================================
// FFI Declarations
// ============================================================================

type AXObserverRef = *mut c_void;
type AXUIElementRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type AXObserverCallback =
    unsafe extern "C" fn(AXObserverRef, AXUIElementRef, *const c_void, *mut c_void);

const K_AX_ERROR_SUCCESS: i32 = 0;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXObserverCreate(
        application: i32,
        callback: AXObserverCallback,
        out_observer: *mut AXObserverRef,
    ) -> i32;
    fn AXObserverAddNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: *const c_void,
        refcon: *mut c_void,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: AXObserverRef) -> CFRunLoopSourceRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFRunLoopAddSource(rl: *const c_void, source: *const c_void, mode: *const c_void);
}

// ============================================================================
// Global State
// ============================================================================

/// Whether the observer system has been initialized.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Global observer state protected by a mutex.
static OBSERVER_STATE: Mutex<Option<ObserverState>> = Mutex::new(None);

/// State for the observer system.
struct ObserverState {
    /// Map of PID to observer reference.
    observers: HashMap<i32, ObserverRef>,
}

// ============================================================================
// Notification Names
// ============================================================================

/// macOS accessibility notification names.
mod notifications {
    pub const WINDOW_CREATED: &str = "AXWindowCreated";
    pub const WINDOW_MOVED: &str = "AXWindowMoved";
    pub const WINDOW_RESIZED: &str = "AXWindowResized";
    pub const WINDOW_MINIMIZED: &str = "AXWindowMiniaturized";
    pub const WINDOW_UNMINIMIZED: &str = "AXWindowDeminiaturized";
    pub const FOCUSED_WINDOW_CHANGED: &str = "AXFocusedWindowChanged";
    pub const UI_ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
    pub const TITLE_CHANGED: &str = "AXTitleChanged";
    pub const APP_ACTIVATED: &str = "AXApplicationActivated";
    pub const APP_DEACTIVATED: &str = "AXApplicationDeactivated";
    pub const APP_HIDDEN: &str = "AXApplicationHidden";
    pub const APP_SHOWN: &str = "AXApplicationShown";
}

// ============================================================================
// Public API
// ============================================================================

/// Initializes the observer system for all running applications.
///
/// # Safety
///
/// This function must be called from the main thread.
pub fn init() -> bool {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        log::debug!("tiling: observer already initialized");
        return true;
    }

    // Initialize the observer state
    {
        let mut state = OBSERVER_STATE.lock();
        *state = Some(ObserverState { observers: HashMap::new() });
    }

    // Get running apps using our window module
    let apps = crate::modules::tiling::window::get_running_apps();
    let mut observed = 0;
    let mut skipped = 0;

    for app in apps {
        if !should_observe_app(&app.bundle_id, &app.name) {
            skipped += 1;
            continue;
        }

        if add_observer_for_pid(app.pid).is_ok() {
            observed += 1;
        }
    }

    log::info!("tiling: observers initialized ({observed} apps, {skipped} filtered)");
    true
}

/// Adds an observer for a new application by PID.
///
/// Call this when a new application is launched.
pub fn add_observer_for_pid(pid: i32) -> Result<(), String> {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return Err("Observer system not initialized".to_string());
    }

    let mut state_guard = OBSERVER_STATE.lock();

    let state = state_guard.as_mut().ok_or("Observer state not initialized")?;

    // Skip if already observing
    if state.observers.contains_key(&pid) {
        return Ok(());
    }

    // Create the observer
    let mut observer: AXObserverRef = ptr::null_mut();
    let result =
        unsafe { AXObserverCreate(pid, observer_callback, std::ptr::addr_of_mut!(observer)) };

    if result != K_AX_ERROR_SUCCESS || observer.is_null() {
        return Err(format!("AXObserverCreate failed for pid {pid}: {result}"));
    }

    // Create the application element
    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    if app_element.is_null() {
        unsafe { CFRelease(observer.cast()) };
        return Err(format!("AXUIElementCreateApplication failed for pid {pid}"));
    }

    // Add notifications
    let notification_names = [
        notifications::WINDOW_CREATED,
        notifications::WINDOW_MOVED,
        notifications::WINDOW_RESIZED,
        notifications::WINDOW_MINIMIZED,
        notifications::WINDOW_UNMINIMIZED,
        notifications::FOCUSED_WINDOW_CHANGED,
        notifications::UI_ELEMENT_DESTROYED,
        notifications::TITLE_CHANGED,
        notifications::APP_ACTIVATED,
        notifications::APP_DEACTIVATED,
        notifications::APP_HIDDEN,
        notifications::APP_SHOWN,
    ];

    for name in notification_names {
        let cf_name = CFString::new(name);
        let result = unsafe {
            AXObserverAddNotification(
                observer,
                app_element,
                cf_name.as_concrete_TypeRef().cast(),
                pid as *mut c_void,
            )
        };
        if result != K_AX_ERROR_SUCCESS {
            log::trace!("Failed to add notification {name} for pid {pid}: {result}");
        }
    }

    // Release the app element (observer keeps its own reference)
    unsafe { CFRelease(app_element.cast()) };

    // Add observer to run loop
    let source = unsafe { AXObserverGetRunLoopSource(observer) };
    if !source.is_null() {
        let run_loop = CFRunLoop::get_main();
        // Get the default mode constant
        let mode = unsafe { core_foundation::runloop::kCFRunLoopDefaultMode };
        unsafe {
            CFRunLoopAddSource(run_loop.as_concrete_TypeRef().cast(), source, mode.cast());
        }
    }

    // Store the observer
    state.observers.insert(pid, ObserverRef(observer));
    log::trace!("Added observer for pid {pid}");

    Ok(())
}

/// Removes the observer for an application.
pub fn remove_observer_for_pid(pid: i32) {
    let mut state_guard = OBSERVER_STATE.lock();
    if let Some(state) = state_guard.as_mut()
        && let Some(observer) = state.observers.remove(&pid)
    {
        // Release the observer
        unsafe { CFRelease(observer.0.cast()) };
        log::trace!("Removed observer for pid {pid}");
    }
}

/// Checks if we should observe an app.
#[must_use]
pub fn should_observe_app(bundle_id: &str, name: &str) -> bool {
    // Check bundle ID
    if !bundle_id.is_empty()
        && SKIP_OBSERVER_BUNDLE_IDS.iter().any(|&id| bundle_id.eq_ignore_ascii_case(id))
    {
        return false;
    }

    // Check app name
    if !name.is_empty() && SKIP_OBSERVER_APP_NAMES.iter().any(|&n| name.eq_ignore_ascii_case(n)) {
        return false;
    }

    true
}

// ============================================================================
// Observer Callback
// ============================================================================

/// Callback invoked by macOS when an accessibility notification fires.
///
/// # Safety
///
/// This function is called by macOS accessibility framework. The `element` and
/// `notification` pointers are valid for the duration of the callback.
unsafe extern "C" fn observer_callback(
    _observer: AXObserverRef,
    element: AXUIElementRef,
    notification: *const c_void,
    refcon: *mut c_void,
) {
    // SAFETY: All operations in this callback are unsafe but valid when called from macOS
    unsafe {
        // Get the notification name
        let cf_notification = notification as core_foundation::string::CFStringRef;
        let notification_str = CFString::wrap_under_get_rule(cf_notification);
        let notification_name = notification_str.to_string();

        // Get the PID from refcon
        let pid = refcon as i32;

        // Log destroyed events specifically
        if notification_name == notifications::UI_ELEMENT_DESTROYED {
            log::debug!(
                "tiling: observer received AXUIElementDestroyed for pid={pid}, element={:?}",
                element
            );
        }

        // Convert notification to event type
        let Some(event_type) = WindowEventType::from_notification(&notification_name) else {
            log::trace!("Unknown notification: {notification_name}");
            return;
        };

        // Create event and forward to adapter
        let event = WindowEvent::new(event_type, pid, element as usize);
        super::ax_observer::adapter_callback(event);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_observe_app() {
        // Should skip system apps
        assert!(!should_observe_app("com.apple.dock", "Dock"));
        assert!(!should_observe_app("com.apple.SystemUIServer", "SystemUIServer"));
        assert!(!should_observe_app("", "Dock"));

        // Should observe normal apps
        assert!(should_observe_app("com.apple.Safari", "Safari"));
        assert!(should_observe_app("com.google.Chrome", "Google Chrome"));
        assert!(should_observe_app("", "SomeApp"));
    }
}
