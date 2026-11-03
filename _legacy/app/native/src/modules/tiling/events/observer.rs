//! Standalone `AXObserver` system for tiling.
//!
//! This module provides `AXObserver` functionality to receive notifications
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
use crate::modules::tiling::identity::{AppIdentity, is_process_alive};

// ============================================================================
// Thread-Safe Wrapper
// ============================================================================

/// Wrapper for `AXObserverRef` that's `Send` + `Sync`.
///
/// This is safe because `AXObserver` operations are only performed on the main thread,
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
    fn CFRunLoopRemoveSource(rl: *const c_void, source: *const c_void, mode: *const c_void);
}

// ============================================================================
// Global State
// ============================================================================

/// Whether the observer system has been initialized.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Global observer state protected by a mutex.
static OBSERVER_STATE: Mutex<Option<ObserverState>> = Mutex::new(None);

/// One registered observer plus the exact identity it was created for.
struct ObserverRecord {
    observer: ObserverRef,
    identity: AppIdentity,
}

/// State for the observer system.
struct ObserverState {
    /// Primary index: `AXObserverRef` address → `ObserverRecord`.
    observers: HashMap<usize, ObserverRecord>,
    /// Exact reverse index: application identity → `AXObserverRef` address.
    identity_to_observer: HashMap<AppIdentity, usize>,
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
        tracing::debug!("tiling: observer already initialized");
        return true;
    }

    // Initialize the observer state
    {
        let mut state = OBSERVER_STATE.lock();
        *state = Some(ObserverState {
            observers: HashMap::new(),
            identity_to_observer: HashMap::new(),
        });
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

    tracing::info!("tiling: observers initialized ({observed} apps, {skipped} filtered)");
    true
}

/// Removes and releases every registered `AXObserver` and resets the system
/// so it can be re-initialized on a fresh resume.
///
/// # Safety
///
/// This function must be called from the main thread.
pub fn shutdown() {
    if !INITIALIZED.swap(false, Ordering::SeqCst) {
        return;
    }

    let mut state_guard = OBSERVER_STATE.lock();
    if let Some(mut state) = state_guard.take() {
        for (_address, record) in state.observers.drain() {
            unsafe { CFRelease(record.observer.0.cast()) };
            tracing::trace!(
                identity = ?record.identity,
                "Released standalone observer"
            );
        }
    }
    drop(state_guard);

    tracing::debug!("tiling: standalone AX observer system shut down");
}

/// Adds an observer for a new application by PID.
///
/// Call this when a new application is launched.
///
/// # Errors
/// Returns an error if the observer system is not initialized, if creating
/// the AX observer or application element fails, or if the application
/// identity cannot be captured or changes during setup.
#[allow(clippy::significant_drop_tightening)]
#[allow(clippy::too_many_lines)] // one long identity-validated registration flow
pub fn add_observer_for_pid(pid: i32) -> Result<(), String> {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};

    if !INITIALIZED.load(Ordering::SeqCst) {
        return Err("Observer system not initialized".to_string());
    }

    let identity = objc::rc::autoreleasepool(|| -> Option<AppIdentity> {
        unsafe {
            let app: *mut Object = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            if app.is_null() {
                return None;
            }
            // The PID is authoritative here: it was resolved from window
            // ownership, so apps that report NSNotFound for `processIdentifier`
            // still resolve.
            AppIdentity::from_ns_running_app_with_pid(app, pid)
        }
    });
    let Some(identity) = identity else {
        return Err(format!("no valid identity for pid {pid}"));
    };

    {
        let state_guard = OBSERVER_STATE.lock();
        let state = state_guard.as_ref().ok_or("Observer state not initialized")?;
        if state.identity_to_observer.contains_key(&identity) {
            return Ok(());
        }
    }

    let mut observer: AXObserverRef = ptr::null_mut();
    let result =
        unsafe { AXObserverCreate(pid, observer_callback, std::ptr::addr_of_mut!(observer)) };
    if result != K_AX_ERROR_SUCCESS || observer.is_null() {
        return Err(format!("AXObserverCreate failed for pid {pid}: {result}"));
    }
    let app_element = unsafe { AXUIElementCreateApplication(pid) };
    if app_element.is_null() {
        unsafe { CFRelease(observer.cast()) };
        return Err(format!("AXUIElementCreateApplication failed for pid {pid}"));
    }

    for name in [
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
    ] {
        let cf_name = CFString::new(name);
        let r = unsafe {
            AXObserverAddNotification(
                observer,
                app_element,
                cf_name.as_concrete_TypeRef().cast(),
                pid as *mut c_void,
            )
        };
        if r != K_AX_ERROR_SUCCESS {
            tracing::trace!("Failed to add notification {name} for pid {pid}: {r}");
        }
    }
    unsafe { CFRelease(app_element.cast()) };

    let source = unsafe { AXObserverGetRunLoopSource(observer) };
    if source.is_null() {
        unsafe { CFRelease(observer.cast()) };
        return Err(format!("observer has no run-loop source: pid {pid}"));
    }

    // Re-resolve immediately before publication to close the PID-reuse window.
    let current_identity = objc::rc::autoreleasepool(|| -> Option<AppIdentity> {
        unsafe {
            let app: *mut Object = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            if app.is_null() {
                return None;
            }
            AppIdentity::from_ns_running_app_with_pid(app, pid)
        }
    });
    if current_identity != Some(identity) {
        unsafe { CFRelease(observer.cast()) };
        return Err(format!(
            "application identity changed during observer setup: pid {pid}"
        ));
    }

    let record = ObserverRecord {
        observer: ObserverRef(observer),
        identity,
    };
    let mut state_guard = OBSERVER_STATE.lock();
    let Some(state) = state_guard.as_mut() else {
        drop(state_guard);
        unsafe { CFRelease(observer.cast()) };
        return Err("Observer state not initialized".to_string());
    };
    let address = observer as usize;
    if state.observers.contains_key(&address) || state.identity_to_observer.contains_key(&identity)
    {
        drop(state_guard);
        unsafe { CFRelease(observer.cast()) };
        return Ok(());
    }
    state.observers.insert(address, record);
    state.identity_to_observer.insert(identity, address);
    drop(state_guard);

    unsafe {
        let run_loop = CFRunLoop::get_main();
        let mode = core_foundation::runloop::kCFRunLoopDefaultMode;
        CFRunLoopAddSource(run_loop.as_concrete_TypeRef().cast(), source, mode.cast());
    }
    tracing::trace!("Added observer for identity {identity:?}");
    Ok(())
}

/// Exact removal seam (pure, never calls Core Foundation).
fn take_observer_record_for_identity(
    state: &mut ObserverState,
    identity: &AppIdentity,
) -> Option<ObserverRecord> {
    let address = state.identity_to_observer.remove(identity)?;
    state.observers.remove(&address)
}

/// Releases a removed observer's run-loop source and Core Foundation handle.
fn release_observer_record(record: &ObserverRecord) {
    let address = record.observer.0 as usize;
    unsafe {
        let source = AXObserverGetRunLoopSource(record.observer.0);
        if !source.is_null() {
            let run_loop = CFRunLoop::get_main();
            let mode = core_foundation::runloop::kCFRunLoopDefaultMode;
            CFRunLoopRemoveSource(run_loop.as_concrete_TypeRef().cast(), source, mode.cast());
        }
        CFRelease(record.observer.0.cast());
    }
    tracing::trace!(address, "ax_observer: released observer");
}

/// Removes and releases the observer registered for an exact identity.
pub fn remove_observer_for_identity(identity: &AppIdentity) {
    let record = {
        let mut state_guard = OBSERVER_STATE.lock();
        state_guard
            .as_mut()
            .and_then(|state| take_observer_record_for_identity(state, identity))
    };
    let Some(record) = record else {
        return;
    };
    release_observer_record(&record);
}

/// Removes and releases observers whose process is no longer alive, returning
/// their identities.
///
/// Application termination notifications carry no usable PID for apps that
/// report `NSNotFound` from `processIdentifier` (e.g. Device Hub). Pruning by
/// process liveness still attributes the termination, so those observers and
/// their tracked windows are cleaned up and layouts recomputed.
pub fn take_identities_for_dead_processes() -> Vec<AppIdentity> {
    let (identities, records) = {
        let mut state_guard = OBSERVER_STATE.lock();
        let Some(state) = state_guard.as_mut() else {
            return Vec::new();
        };
        let identities: Vec<AppIdentity> = state
            .identity_to_observer
            .keys()
            .filter(|identity| !is_process_alive(identity.pid))
            .copied()
            .collect();
        let records: Vec<ObserverRecord> = identities
            .iter()
            .filter_map(|identity| take_observer_record_for_identity(state, identity))
            .collect();
        drop(state_guard);
        (identities, records)
    };
    for record in &records {
        release_observer_record(record);
    }
    identities
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
    observer: AXObserverRef,
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

        // Get the PID from refcon (logging only)
        let pid = refcon as i32;

        // Log destroyed events specifically
        if notification_name == notifications::UI_ELEMENT_DESTROYED {
            tracing::debug!(
                "tiling: observer received AXUIElementDestroyed for pid={pid}, element={element:?}"
            );
        }

        // Convert notification to event type
        let Some(event_type) = WindowEventType::from_notification(&notification_name) else {
            tracing::trace!("Unknown notification: {notification_name}");
            return;
        };

        // Derive identity from the observer address, never from a bare PID.
        let identity = {
            let state_guard = OBSERVER_STATE.lock();
            let record = state_guard
                .as_ref()
                .and_then(|state| state.observers.get(&(observer as usize)))
                .map(|record| record.identity);
            drop(state_guard);
            let Some(identity) = record else {
                return;
            };
            identity
        };

        // Create event and forward to adapter
        let event = WindowEvent::new(event_type, pid, element as usize, identity);
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

    #[test]
    fn test_shutdown_is_idempotent() {
        // System is not initialized in unit tests; both calls must be no-ops.
        shutdown();
        assert!(!INITIALIZED.load(Ordering::SeqCst));
        shutdown();
        assert!(!INITIALIZED.load(Ordering::SeqCst));
    }

    #[test]
    fn removal_by_identity_preserves_same_pid_replacement() {
        use crate::modules::tiling::identity::{AppIdentity, LaunchDateBits};
        let mut state = ObserverState {
            observers: HashMap::new(),
            identity_to_observer: HashMap::new(),
        };
        let a = AppIdentity {
            pid: 42,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(100.0).unwrap(),
        };
        let b = AppIdentity {
            pid: 42,
            launch_date: LaunchDateBits::from_time_interval_since_reference_date(200.0).unwrap(),
        };
        state.observers.insert(0xAAA, ObserverRecord {
            observer: ObserverRef(0xAAA as *mut c_void),
            identity: a,
        });
        state.observers.insert(0xBBB, ObserverRecord {
            observer: ObserverRef(0xBBB as *mut c_void),
            identity: b,
        });
        state.identity_to_observer.insert(a, 0xAAA);
        state.identity_to_observer.insert(b, 0xBBB);

        let removed = take_observer_record_for_identity(&mut state, &a).expect("a present");
        assert_eq!(removed.observer.0, 0xAAA as *mut c_void);
        assert!(state.identity_to_observer.contains_key(&b));
        assert!(state.observers.contains_key(&0xBBB));
        assert!(!state.observers.contains_key(&0xAAA));
    }
}
