//! Window event observation using macOS Accessibility API.
//!
//! This module provides `AXObserver` functionality to receive notifications
//! about window events such as creation, destruction, focus changes, and
//! geometry changes.
//!
//! # Event Types
//!
//! The observer tracks the following events:
//! - Window created/destroyed
//! - Window focused/unfocused
//! - Window moved/resized
//! - Window minimized/unminimized
//! - Window title changed
//! - Application activated/deactivated
//!
//! # Architecture
//!
//! Each running application gets its own `AXObserver` instance. Events are
//! dispatched to a central callback that routes them to the `TilingManager`.
//!
//! # Thread Safety
//!
//! The observer system uses a single-threaded model where all observer operations
//! happen on the main thread. The global state is protected by a mutex for
//! thread-safe access during initialization and shutdown.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use core_foundation::base::TCFType;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use core_foundation::string::CFString;

use super::window::{AppInfo, get_running_apps};

// ============================================================================
// Accessibility Observer FFI
// ============================================================================

type AXObserverRef = *mut c_void;
type AXUIElementRef = *mut c_void;
type AXObserverCallback =
    unsafe extern "C" fn(AXObserverRef, AXUIElementRef, *const c_void, *mut c_void);

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXObserverCreate(
        application: i32,
        callback: AXObserverCallback,
        observer: *mut AXObserverRef,
    ) -> i32;
    fn AXObserverGetRunLoopSource(observer: AXObserverRef) -> *mut c_void;
    fn AXObserverAddNotification(
        observer: AXObserverRef,
        element: AXUIElementRef,
        notification: *const c_void,
        refcon: *mut c_void,
    ) -> i32;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
    fn CFRunLoopRemoveSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
    fn CFRelease(cf: *const c_void);
}

const K_AX_ERROR_SUCCESS: i32 = 0;

// ============================================================================
// Notification Constants
// ============================================================================

/// Notification names for accessibility events.
pub mod notifications {
    /// Window was created.
    pub const WINDOW_CREATED: &str = "AXWindowCreated";
    /// UI element was destroyed (window closed).
    pub const UI_ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
    /// Focused UI element changed.
    pub const FOCUSED_UI_ELEMENT_CHANGED: &str = "AXFocusedUIElementChanged";
    /// Focused window changed.
    pub const FOCUSED_WINDOW_CHANGED: &str = "AXFocusedWindowChanged";
    /// Window was moved.
    pub const WINDOW_MOVED: &str = "AXWindowMoved";
    /// Window was resized.
    pub const WINDOW_RESIZED: &str = "AXWindowResized";
    /// Window was minimized.
    pub const WINDOW_MINIMIZED: &str = "AXWindowMiniaturized";
    /// Window was unminimized.
    pub const WINDOW_UNMINIMIZED: &str = "AXWindowDeminiaturized";
    /// Window title changed.
    pub const TITLE_CHANGED: &str = "AXTitleChanged";
    /// Application was activated.
    pub const APPLICATION_ACTIVATED: &str = "AXApplicationActivated";
    /// Application was deactivated.
    pub const APPLICATION_DEACTIVATED: &str = "AXApplicationDeactivated";
    /// Application was hidden.
    pub const APPLICATION_HIDDEN: &str = "AXApplicationHidden";
    /// Application was shown.
    pub const APPLICATION_SHOWN: &str = "AXApplicationShown";
}

// ============================================================================
// Event Types
// ============================================================================

/// Types of window events that can be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowEventType {
    /// A new window was created.
    Created,
    /// A window was closed/destroyed.
    Destroyed,
    /// A window gained focus.
    Focused,
    /// A window lost focus.
    Unfocused,
    /// A window was moved to a new position.
    Moved,
    /// A window was resized.
    Resized,
    /// A window was minimized.
    Minimized,
    /// A window was restored from minimized state.
    Unminimized,
    /// A window's title changed.
    TitleChanged,
    /// An application was activated (brought to front).
    AppActivated,
    /// An application was deactivated (moved to background).
    AppDeactivated,
    /// An application was hidden.
    AppHidden,
    /// An application was shown.
    AppShown,
}

impl WindowEventType {
    /// Returns the accessibility notification name for this event type.
    #[must_use]
    pub const fn notification_name(self) -> &'static str {
        match self {
            Self::Created => notifications::WINDOW_CREATED,
            Self::Destroyed => notifications::UI_ELEMENT_DESTROYED,
            Self::Focused => notifications::FOCUSED_WINDOW_CHANGED,
            Self::Unfocused => notifications::FOCUSED_UI_ELEMENT_CHANGED,
            Self::Moved => notifications::WINDOW_MOVED,
            Self::Resized => notifications::WINDOW_RESIZED,
            Self::Minimized => notifications::WINDOW_MINIMIZED,
            Self::Unminimized => notifications::WINDOW_UNMINIMIZED,
            Self::TitleChanged => notifications::TITLE_CHANGED,
            Self::AppActivated => notifications::APPLICATION_ACTIVATED,
            Self::AppDeactivated => notifications::APPLICATION_DEACTIVATED,
            Self::AppHidden => notifications::APPLICATION_HIDDEN,
            Self::AppShown => notifications::APPLICATION_SHOWN,
        }
    }

    /// Parses a notification name string into an event type.
    #[must_use]
    pub fn from_notification(name: &str) -> Option<Self> {
        match name {
            notifications::WINDOW_CREATED => Some(Self::Created),
            notifications::UI_ELEMENT_DESTROYED => Some(Self::Destroyed),
            notifications::FOCUSED_WINDOW_CHANGED => Some(Self::Focused),
            notifications::FOCUSED_UI_ELEMENT_CHANGED => Some(Self::Unfocused),
            notifications::WINDOW_MOVED => Some(Self::Moved),
            notifications::WINDOW_RESIZED => Some(Self::Resized),
            notifications::WINDOW_MINIMIZED => Some(Self::Minimized),
            notifications::WINDOW_UNMINIMIZED => Some(Self::Unminimized),
            notifications::TITLE_CHANGED => Some(Self::TitleChanged),
            notifications::APPLICATION_ACTIVATED => Some(Self::AppActivated),
            notifications::APPLICATION_DEACTIVATED => Some(Self::AppDeactivated),
            notifications::APPLICATION_HIDDEN => Some(Self::AppHidden),
            notifications::APPLICATION_SHOWN => Some(Self::AppShown),
            _ => None,
        }
    }

    /// Returns all event types that should be observed.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Created,
            Self::Destroyed,
            Self::Focused,
            Self::Moved,
            Self::Resized,
            Self::Minimized,
            Self::Unminimized,
            Self::TitleChanged,
            Self::AppActivated,
            Self::AppDeactivated,
            Self::AppHidden,
            Self::AppShown,
        ]
    }
}

/// A window event received from the accessibility system.
#[derive(Debug, Clone, Copy)]
pub struct WindowEvent {
    /// The type of event.
    pub event_type: WindowEventType,
    /// The process ID of the application that owns the element.
    pub pid: i32,
    /// The accessibility element that triggered the event.
    /// This is an opaque pointer that should not be dereferenced directly.
    pub element: usize,
}

impl WindowEvent {
    /// Creates a new window event.
    #[must_use]
    pub const fn new(event_type: WindowEventType, pid: i32, element: usize) -> Self {
        Self { event_type, pid, element }
    }
}

// ============================================================================
// Observer State
// ============================================================================

/// Whether the observer system is initialized.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Global event callback (stored as a function pointer).
static EVENT_CALLBACK: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

/// Observer data for a single application.
struct AppObserver {
    /// The `AXObserver` instance.
    observer: *mut c_void,
    /// The application's `AXUIElement`.
    app_element: *mut c_void,
    /// The process ID.
    #[allow(dead_code)]
    pid: i32,
}

// SAFETY: AppObserver is only accessed from the main thread via the Mutex.
unsafe impl Send for AppObserver {}

impl Drop for AppObserver {
    fn drop(&mut self) {
        // Remove from run loop and release
        unsafe {
            let source = AXObserverGetRunLoopSource(self.observer);
            if !source.is_null() {
                let run_loop = CFRunLoop::get_main();
                let rl_ptr: *mut c_void = run_loop.as_concrete_TypeRef().cast();
                let mode_ptr: *const c_void = kCFRunLoopDefaultMode.cast();
                CFRunLoopRemoveSource(rl_ptr, source, mode_ptr);
            }
            CFRelease(self.observer.cast());
            CFRelease(self.app_element.cast());
        }
    }
}

/// Shared state for the observer system.
struct ObserverState {
    /// Map of PID to observer data.
    observers: HashMap<i32, AppObserver>,
}

/// Global observer state protected by a mutex.
static OBSERVER_STATE: Mutex<Option<ObserverState>> = Mutex::new(None);

// ============================================================================
// Observer Callback
// ============================================================================

/// The C callback function that receives accessibility notifications.
///
/// # Safety
///
/// This function is called from the accessibility system. The parameters are
/// provided by the system and must be valid.
unsafe extern "C" fn observer_callback(
    _observer: AXObserverRef,
    element: AXUIElementRef,
    notification: *const c_void,
    refcon: *mut c_void,
) {
    // SAFETY: This function is called from the accessibility system with valid parameters.
    unsafe {
        // refcon contains the PID
        let pid = refcon as i32;

        // Get the notification name
        let notification_cf = CFString::wrap_under_get_rule(notification.cast());
        let notification_name = notification_cf.to_string();

        // Parse the event type
        let Some(event_type) = WindowEventType::from_notification(&notification_name) else {
            return;
        };

        // Create the event
        let event = WindowEvent::new(event_type, pid, element as usize);

        // Dispatch to the callback
        let callback_ptr = EVENT_CALLBACK.load(Ordering::SeqCst);
        if !callback_ptr.is_null() {
            let callback: fn(WindowEvent) = std::mem::transmute(callback_ptr);
            callback(event);
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Initializes the observer system with a callback for events.
///
/// This function sets up observers for all running applications that can
/// own windows. The callback will be called on the main thread's run loop
/// when events occur.
///
/// # Arguments
///
/// * `callback` - The function to call when window events occur.
///
/// # Returns
///
/// `true` if initialization was successful, `false` otherwise.
///
/// # Safety
///
/// This function must be called from the main thread.
pub fn init(callback: fn(WindowEvent)) -> bool {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        // Already initialized
        return true;
    }

    // Store the callback as a raw pointer
    EVENT_CALLBACK.store(callback as *mut (), Ordering::SeqCst);

    // Initialize the observer state
    if let Ok(mut state) = OBSERVER_STATE.lock() {
        *state = Some(ObserverState { observers: HashMap::new() });
    }

    // Set up observers for running apps
    let apps = get_running_apps();
    for app in apps {
        if let Err(e) = add_observer(&app) {
            eprintln!("stache: tiling: failed to observe {}: {e}", app.name);
        }
    }

    true
}

/// Adds an observer for a new application.
///
/// Call this when a new application is launched to begin observing its windows.
///
/// # Arguments
///
/// * `app` - Information about the application to observe.
///
/// # Returns
///
/// `Ok(())` if successful, `Err` with a message otherwise.
#[allow(clippy::significant_drop_tightening)] // Guard needs to be held for entire function
pub fn add_observer(app: &AppInfo) -> Result<(), String> {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return Err("Observer system not initialized".to_string());
    }

    let mut state_guard = OBSERVER_STATE
        .lock()
        .map_err(|e| format!("Failed to lock observer state: {e}"))?;

    let state = state_guard.as_mut().ok_or("Observer state not initialized")?;

    // Skip if already observing
    if state.observers.contains_key(&app.pid) {
        return Ok(());
    }

    // Create the observer
    let mut observer: AXObserverRef = ptr::null_mut();
    let result =
        unsafe { AXObserverCreate(app.pid, observer_callback, std::ptr::addr_of_mut!(observer)) };

    if result != K_AX_ERROR_SUCCESS || observer.is_null() {
        return Err(format!("AXObserverCreate failed with error {result}"));
    }

    // Create the application element
    let app_element = unsafe { AXUIElementCreateApplication(app.pid) };
    if app_element.is_null() {
        unsafe { CFRelease(observer.cast()) };
        return Err("Failed to create application element".to_string());
    }

    // Add notifications
    let refcon = app.pid as *mut c_void;
    for event_type in WindowEventType::all() {
        let notification = CFString::new(event_type.notification_name());
        let result = unsafe {
            AXObserverAddNotification(
                observer,
                app_element,
                notification.as_concrete_TypeRef().cast(),
                refcon,
            )
        };

        if result != K_AX_ERROR_SUCCESS {
            // Non-fatal: some notifications may not be supported
            eprintln!(
                "stache: tiling: warning: could not add {} notification for PID {}: error {}",
                event_type.notification_name(),
                app.pid,
                result
            );
        }
    }

    // Add to run loop
    unsafe {
        let source = AXObserverGetRunLoopSource(observer);
        if !source.is_null() {
            let run_loop = CFRunLoop::get_main();
            let rl_ptr: *mut c_void = run_loop.as_concrete_TypeRef().cast();
            let mode_ptr: *const c_void = kCFRunLoopDefaultMode.cast();
            CFRunLoopAddSource(rl_ptr, source, mode_ptr);
        }
    }

    // Store the observer
    state.observers.insert(app.pid, AppObserver {
        observer,
        app_element,
        pid: app.pid,
    });

    Ok(())
}

/// Removes the observer for an application.
///
/// Call this when an application terminates.
///
/// # Arguments
///
/// * `pid` - The process ID of the application.
pub fn remove_observer(pid: i32) {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return;
    }

    if let Ok(mut state_guard) = OBSERVER_STATE.lock()
        && let Some(state) = state_guard.as_mut()
    {
        // Drop will clean up the observer
        state.observers.remove(&pid);
    }
}

/// Shuts down the observer system.
///
/// Removes all observers and clears the callback.
pub fn shutdown() {
    if !INITIALIZED.swap(false, Ordering::SeqCst) {
        return;
    }

    // Clear the callback
    EVENT_CALLBACK.store(ptr::null_mut(), Ordering::SeqCst);

    // Clear the state
    if let Ok(mut state) = OBSERVER_STATE.lock() {
        *state = None;
    }
}

/// Checks if the observer system is initialized.
#[must_use]
pub fn is_initialized() -> bool { INITIALIZED.load(Ordering::SeqCst) }

/// Returns the number of applications being observed.
#[must_use]
pub fn observer_count() -> usize {
    if !INITIALIZED.load(Ordering::SeqCst) {
        return 0;
    }

    OBSERVER_STATE.lock().map_or(0, |s| s.as_ref().map_or(0, |s| s.observers.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_window_event_type_notification_names() {
        assert_eq!(WindowEventType::Created.notification_name(), "AXWindowCreated");
        assert_eq!(
            WindowEventType::Destroyed.notification_name(),
            "AXUIElementDestroyed"
        );
        assert_eq!(
            WindowEventType::Focused.notification_name(),
            "AXFocusedWindowChanged"
        );
        assert_eq!(WindowEventType::Moved.notification_name(), "AXWindowMoved");
        assert_eq!(WindowEventType::Resized.notification_name(), "AXWindowResized");
        assert_eq!(
            WindowEventType::Minimized.notification_name(),
            "AXWindowMiniaturized"
        );
        assert_eq!(
            WindowEventType::Unminimized.notification_name(),
            "AXWindowDeminiaturized"
        );
        assert_eq!(
            WindowEventType::TitleChanged.notification_name(),
            "AXTitleChanged"
        );
        assert_eq!(
            WindowEventType::AppActivated.notification_name(),
            "AXApplicationActivated"
        );
        assert_eq!(
            WindowEventType::AppDeactivated.notification_name(),
            "AXApplicationDeactivated"
        );
        assert_eq!(
            WindowEventType::AppHidden.notification_name(),
            "AXApplicationHidden"
        );
        assert_eq!(
            WindowEventType::AppShown.notification_name(),
            "AXApplicationShown"
        );
    }

    #[test]
    fn test_window_event_type_from_notification() {
        assert_eq!(
            WindowEventType::from_notification("AXWindowCreated"),
            Some(WindowEventType::Created)
        );
        assert_eq!(
            WindowEventType::from_notification("AXUIElementDestroyed"),
            Some(WindowEventType::Destroyed)
        );
        assert_eq!(
            WindowEventType::from_notification("AXFocusedWindowChanged"),
            Some(WindowEventType::Focused)
        );
        assert_eq!(
            WindowEventType::from_notification("AXWindowMoved"),
            Some(WindowEventType::Moved)
        );
        assert_eq!(
            WindowEventType::from_notification("AXWindowResized"),
            Some(WindowEventType::Resized)
        );
        assert_eq!(
            WindowEventType::from_notification("AXWindowMiniaturized"),
            Some(WindowEventType::Minimized)
        );
        assert_eq!(
            WindowEventType::from_notification("AXWindowDeminiaturized"),
            Some(WindowEventType::Unminimized)
        );
        assert_eq!(
            WindowEventType::from_notification("AXTitleChanged"),
            Some(WindowEventType::TitleChanged)
        );
        assert_eq!(WindowEventType::from_notification("Unknown"), None);
    }

    #[test]
    fn test_window_event_type_all() {
        let all = WindowEventType::all();
        assert!(all.len() >= 10);
        assert!(all.contains(&WindowEventType::Created));
        assert!(all.contains(&WindowEventType::Destroyed));
        assert!(all.contains(&WindowEventType::Focused));
        assert!(all.contains(&WindowEventType::Moved));
        assert!(all.contains(&WindowEventType::Resized));
    }

    #[test]
    fn test_window_event_new() {
        let event = WindowEvent::new(WindowEventType::Created, 1234, 0x1234_5678);
        assert_eq!(event.event_type, WindowEventType::Created);
        assert_eq!(event.pid, 1234);
        assert_eq!(event.element, 0x1234_5678);
    }
}
