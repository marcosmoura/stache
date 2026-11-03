//! App lifecycle monitor adapter for the tiling v2 event pipeline.
//!
//! This module bridges the `NSWorkspace` app lifecycle notifications to the new
//! event processor architecture. It translates app launch/terminate events
//! into `StateMessage`s for the state actor.
//!
//! # Architecture
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────────┐
//! │                    NSWorkspace Notifications                   │
//! │  (NSWorkspaceDidLaunchApplicationNotification, etc.)          │
//! └───────────────────────────┬───────────────────────────────────┘
//!                             │ Objective-C callback
//!                             ▼
//! ┌───────────────────────────────────────────────────────────────┐
//! │                  AppMonitorAdapter                             │
//! │  - Registers with NSNotificationCenter                         │
//! │  - Extracts app info from NSRunningApplication                 │
//! │  - Forwards events to EventProcessor                           │
//! └───────────────────────────┬───────────────────────────────────┘
//!                             │ EventProcessor methods
//!                             ▼
//! ┌───────────────────────────────────────────────────────────────┐
//! │                    EventProcessor                              │
//! │  - Dispatches to StateActor                                    │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Thread Safety
//!
//! `NSWorkspace` notifications are delivered on the main thread. The adapter
//! references a thread-safe `EventProcessor`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use parking_lot::RwLock;

use crate::modules::tiling::events::EventProcessor;
use crate::modules::tiling::events::observer::{
    add_observer_for_pid, remove_observer_for_identity, should_observe_app,
    take_identities_for_dead_processes,
};
use crate::modules::tiling::identity::{AppIdentity, process_id_for_app};
use crate::platform::objc::nsstring;

// ============================================================================
// App Monitor Adapter
// ============================================================================

/// Adapter that receives app lifecycle events and routes them to the `EventProcessor`.
pub struct AppMonitorAdapter {
    /// Reference to the event processor.
    processor: Arc<EventProcessor>,

    /// Generation this adapter belongs to (from `init::current_generation()`).
    generation: u64,

    /// Whether the adapter is initialized (observer registered).
    initialized: AtomicBool,

    /// Whether callbacks should be forwarded.
    active: AtomicBool,

    /// Retained `NSWorkspace` observer object (`*mut Object` as `usize`).
    /// Only touched on the main thread.
    observer: std::sync::Mutex<Option<usize>>,

    /// Retained workspace notification center (`*mut Object` as `usize`).
    notification_center: std::sync::Mutex<Option<usize>>,
}

impl AppMonitorAdapter {
    /// Creates a new adapter with the given event processor.
    #[must_use]
    pub fn new(processor: Arc<EventProcessor>) -> Self {
        Self {
            processor,
            generation: crate::modules::tiling::init::current_generation(),
            initialized: AtomicBool::new(false),
            active: AtomicBool::new(false),
            observer: std::sync::Mutex::new(None),
            notification_center: std::sync::Mutex::new(None),
        }
    }

    /// Whether this adapter's callbacks may currently be forwarded.
    fn gate_open(&self) -> bool {
        self.active.load(Ordering::SeqCst)
            && self.generation == crate::modules::tiling::init::current_generation()
    }

    /// Initializes the adapter by registering with `NSNotificationCenter`.
    ///
    /// # Returns
    ///
    /// `true` if initialization succeeded, `false` if already initialized or failed.
    ///
    /// # Panics
    ///
    /// Panics if an observer-retention mutex is poisoned.
    ///
    /// # Safety
    ///
    /// This function must be called from the main thread.
    pub fn init(&self) -> bool {
        if self.initialized.swap(true, Ordering::SeqCst) {
            tracing::warn!("AppMonitorAdapter already initialized");
            return false;
        }

        unsafe {
            // Get NSWorkspace's shared instance
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if workspace.is_null() {
                tracing::error!("Failed to get shared workspace");
                self.initialized.store(false, Ordering::SeqCst);
                return false;
            }

            // Get the notification center
            let notification_center: *mut Object = msg_send![workspace, notificationCenter];
            if notification_center.is_null() {
                tracing::error!("Failed to get workspace notification center");
                self.initialized.store(false, Ordering::SeqCst);
                return false;
            }

            // Create the observer
            let observer = create_workspace_observer();
            if observer.is_null() {
                tracing::error!("Failed to create app lifecycle observer");
                self.initialized.store(false, Ordering::SeqCst);
                return false;
            }

            // Register for NSWorkspaceDidLaunchApplicationNotification
            let launch_notification = nsstring("NSWorkspaceDidLaunchApplicationNotification");
            let _: () = msg_send![
                notification_center,
                addObserver: observer
                selector: sel!(handleAppLaunch:)
                name: launch_notification
                object: std::ptr::null::<Object>()
            ];

            // Register for NSWorkspaceDidTerminateApplicationNotification
            let terminate_notification = nsstring("NSWorkspaceDidTerminateApplicationNotification");
            let _: () = msg_send![
                notification_center,
                addObserver: observer
                selector: sel!(handleAppTerminate:)
                name: terminate_notification
                object: std::ptr::null::<Object>()
            ];

            // Retain the observer and notification center for shutdown removal.
            *self.observer.lock().unwrap() = Some(observer as usize);
            *self.notification_center.lock().unwrap() = Some(notification_center as usize);
        }

        self.active.store(true, Ordering::SeqCst);
        tracing::debug!("AppMonitorAdapter initialized");
        true
    }

    /// Returns whether the adapter is initialized.
    #[must_use]
    pub fn is_initialized(&self) -> bool { self.initialized.load(Ordering::SeqCst) }

    /// Removes the `NSWorkspace` observer and resets the adapter.
    ///
    /// Must be called on the main thread. Idempotent.
    ///
    /// # Panics
    ///
    /// Panics if an observer-retention mutex is poisoned.
    pub fn shutdown(&self) {
        self.active.store(false, Ordering::SeqCst);
        uninstall_adapter();
        if !self.initialized.swap(false, Ordering::SeqCst) {
            return;
        }

        let observer = *self.observer.lock().unwrap();
        let notification_center = *self.notification_center.lock().unwrap();

        if let (Some(observer), Some(notification_center)) = (observer, notification_center) {
            unsafe {
                let observer: *mut Object = observer as *mut Object;
                let notification_center: *mut Object = notification_center as *mut Object;

                let launch = nsstring("NSWorkspaceDidLaunchApplicationNotification");
                let _: () = msg_send![
                    notification_center,
                    removeObserver: observer
                    name: launch
                    object: std::ptr::null::<Object>()
                ];

                let terminate = nsstring("NSWorkspaceDidTerminateApplicationNotification");
                let _: () = msg_send![
                    notification_center,
                    removeObserver: observer
                    name: terminate
                    object: std::ptr::null::<Object>()
                ];

                let _: () = msg_send![observer, release];
            }
        }

        *self.observer.lock().unwrap() = None;
        *self.notification_center.lock().unwrap() = None;

        uninstall_adapter();
        tracing::debug!("AppMonitorAdapter shut down");
    }

    /// Handles an app launch event.
    fn on_app_launched(
        &self,
        identity: Option<AppIdentity>,
        pid: i32,
        bundle_id: Option<String>,
        name: Option<String>,
    ) {
        if !self.gate_open() {
            return;
        }

        let Some(identity) = identity else {
            tracing::trace!(pid, "app_monitor: dropping launch event (no identity)");
            return;
        };
        let bundle_id = bundle_id.unwrap_or_default();
        let name = name.unwrap_or_default();

        tracing::debug!("App launched: pid={pid}, bundle={bundle_id}, name={name}");

        // Create AX observer for the new app (must happen on main thread)
        if should_observe_app(&bundle_id, &name)
            && let Err(e) = add_observer_for_pid(pid)
        {
            tracing::warn!("Failed to add observer for pid {pid}: {e}");
        }

        // Adopt windows created before the observer was attached.
        crate::modules::tiling::init::adopt_existing_windows_for_instance(
            &self.processor,
            identity,
            pid,
        );

        self.processor.on_app_launched(identity, pid, bundle_id, name);
    }

    /// Handles an app termination event.
    fn on_app_terminated(
        &self,
        identity: Option<AppIdentity>,
        pid: i32,
        bundle_id: Option<&str>,
        name: Option<&str>,
    ) {
        if !self.gate_open() {
            return;
        }

        tracing::debug!("App terminated: pid={pid}, bundle={bundle_id:?}, name={name:?}");

        if let Some(identity) = identity {
            // Remove the AX observer for this app (must happen on main thread)
            remove_observer_for_identity(&identity);
            self.processor.on_app_terminated(identity, pid);
            return;
        }

        // Apps that report `NSNotFound` from `processIdentifier` (e.g. Device
        // Hub) cannot be attributed to an identity here. Prune observers whose
        // process is gone; the actor removes their windows and recomputes
        // layouts.
        let dead = take_identities_for_dead_processes();
        if dead.is_empty() {
            tracing::trace!(
                pid,
                "app_monitor: no resolvable identity or dead observers to clean up"
            );
            return;
        }
        for identity in dead {
            tracing::debug!(
                "app_monitor: cleaning up terminated instance {identity:?} (bundle={bundle_id:?})"
            );
            self.processor.on_app_terminated(identity, identity.pid);
        }
    }
}

// ============================================================================
// Objective-C Observer
// ============================================================================

/// Creates an Objective-C observer object for handling workspace notifications.
///
/// # Safety
///
/// Caller must ensure:
/// - This is called within a valid Objective-C runtime context
/// - The returned object is retained by `NSNotificationCenter`
unsafe fn create_workspace_observer() -> *mut Object {
    let superclass = class!(NSObject);
    let class_name = "StacheAppLifecycleObserverV2";

    // Check if class already exists
    let existing_class = Class::get(class_name);
    let observer_class = existing_class.unwrap_or_else(|| {
        let mut decl = ClassDecl::new(class_name, superclass)
            .expect("Failed to create StacheAppLifecycleObserverV2 class");

        unsafe {
            decl.add_method(
                sel!(handleAppLaunch:),
                handle_app_launch_notification as extern "C" fn(&Object, Sel, *mut Object),
            );
            decl.add_method(
                sel!(handleAppTerminate:),
                handle_app_terminate_notification as extern "C" fn(&Object, Sel, *mut Object),
            );
        }

        decl.register()
    });

    let instance: *mut Object = msg_send![observer_class, alloc];
    msg_send![instance, init]
}

/// Callback function for app launch notifications.
extern "C" fn handle_app_launch_notification(_self: &Object, _cmd: Sel, notification: *mut Object) {
    if notification.is_null() {
        return;
    }

    let info = extract_app_info(notification);
    if info.identity.is_none() || info.pid <= 0 {
        return;
    }

    if let Some(adapter) = get_installed_adapter() {
        adapter.on_app_launched(info.identity, info.pid, info.bundle_id, info.app_name);
    }
}

/// Callback function for app termination notifications.
extern "C" fn handle_app_terminate_notification(
    _self: &Object,
    _cmd: Sel,
    notification: *mut Object,
) {
    if notification.is_null() {
        return;
    }

    let info = extract_app_info(notification);

    if let Some(adapter) = get_installed_adapter() {
        adapter.on_app_terminated(
            info.identity,
            info.pid,
            info.bundle_id.as_deref(),
            info.app_name.as_deref(),
        );
    }
}

/// Application lifecycle data extracted from an `NSWorkspace` notification.
struct AppNotificationInfo {
    /// Exact identity, or `None` when the PID is not resolvable (termination).
    identity: Option<AppIdentity>,
    /// Resolved PID, or 0 when it cannot be resolved.
    pid: i32,
    bundle_id: Option<String>,
    app_name: Option<String>,
}

impl AppNotificationInfo {
    const fn empty() -> Self {
        Self {
            identity: None,
            pid: 0,
            bundle_id: None,
            app_name: None,
        }
    }
}

/// Extracts app info from an `NSNotification`.
///
/// Identity resolution handles apps that report `NSNotFound` (-1) from
/// `processIdentifier` (e.g. Device Hub): on launch the real PID is recovered
/// from the executable path, while on termination the process is already gone
/// and only the bundle identifier remains.
fn extract_app_info(notification: *mut Object) -> AppNotificationInfo {
    if notification.is_null() {
        return AppNotificationInfo::empty();
    }

    unsafe {
        // Get userInfo dictionary from notification
        let user_info: *mut Object = msg_send![notification, userInfo];
        if user_info.is_null() {
            return AppNotificationInfo::empty();
        }

        // Get NSRunningApplication from userInfo
        let app_key = nsstring("NSWorkspaceApplicationKey");
        let running_app: *mut Object = msg_send![user_info, objectForKey: app_key];
        if running_app.is_null() {
            return AppNotificationInfo::empty();
        }

        // Get the bundle identifier
        let bundle_id: Option<String> = {
            let bundle_id_ns: *mut Object = msg_send![running_app, bundleIdentifier];
            if bundle_id_ns.is_null() {
                None
            } else {
                Some(crate::platform::objc::nsstring_to_string(bundle_id_ns))
            }
        };

        // Get the localized name
        let app_name: Option<String> = {
            let name_ns: *mut Object = msg_send![running_app, localizedName];
            if name_ns.is_null() {
                None
            } else {
                Some(crate::platform::objc::nsstring_to_string(name_ns))
            }
        };

        // Identity comes from this exact object; PID resolution falls back to
        // the executable path when `processIdentifier` reports `NSNotFound`.
        let resolved_pid = objc::rc::autoreleasepool(|| process_id_for_app(running_app));
        let identity = resolved_pid.and_then(|pid| {
            objc::rc::autoreleasepool(|| {
                AppIdentity::from_ns_running_app_with_pid(running_app, pid)
            })
        });

        AppNotificationInfo {
            identity,
            pid: resolved_pid.unwrap_or(0),
            bundle_id,
            app_name,
        }
    }
}

// ============================================================================
// Global Adapter Instance
// ============================================================================

/// Global adapter instance for use with the observer callback.
static ADAPTER: OnceLock<Arc<RwLock<Option<Arc<AppMonitorAdapter>>>>> = OnceLock::new();

fn get_adapter_storage() -> &'static Arc<RwLock<Option<Arc<AppMonitorAdapter>>>> {
    ADAPTER.get_or_init(|| Arc::new(RwLock::new(None)))
}

/// Installs the adapter as the global event handler.
///
/// This should be called once during initialization, after creating the adapter.
pub fn install_adapter(adapter: Arc<AppMonitorAdapter>) {
    *get_adapter_storage().write() = Some(adapter);
}

/// Removes the installed adapter.
pub fn uninstall_adapter() { *get_adapter_storage().write() = None; }

/// Gets the installed adapter, if any.
#[must_use]
pub fn get_installed_adapter() -> Option<Arc<AppMonitorAdapter>> {
    get_adapter_storage().read().clone()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::tiling::actor::StateActor;

    /// Serializes tests that install into / clear the global adapter slot.
    static TEST_SLOT_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[tokio::test]
    async fn test_adapter_creation() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = AppMonitorAdapter::new(processor);

        assert!(!adapter.is_initialized());

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_global_adapter_install() {
        let _guard = TEST_SLOT_LOCK.lock();
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = Arc::new(AppMonitorAdapter::new(processor));

        install_adapter(Arc::clone(&adapter));
        assert!(get_installed_adapter().is_some());

        uninstall_adapter();
        assert!(get_installed_adapter().is_none());

        handle.shutdown().unwrap();
    }

    #[test]
    fn test_extract_app_info_null_notification() {
        let info = extract_app_info(std::ptr::null_mut());
        assert!(info.identity.is_none());
        assert_eq!(info.pid, 0);
        assert!(info.bundle_id.is_none());
        assert!(info.app_name.is_none());
    }

    #[tokio::test]
    async fn test_gate_requires_active() {
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = AppMonitorAdapter::new(processor);

        assert!(!adapter.gate_open(), "inactive adapter must not forward");
        adapter.active.store(true, Ordering::SeqCst);
        assert!(adapter.gate_open(), "active+current generation forwards");
        adapter.active.store(false, Ordering::SeqCst);
        assert!(!adapter.gate_open());

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_shutdown_is_idempotent_and_deactivates() {
        let _guard = TEST_SLOT_LOCK.lock();
        let (handle, _stopped) = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = Arc::new(AppMonitorAdapter::new(processor));

        install_adapter(Arc::clone(&adapter));
        adapter.active.store(true, Ordering::SeqCst);

        adapter.shutdown();
        assert!(!adapter.is_initialized());
        assert!(!adapter.gate_open());
        assert!(get_installed_adapter().is_none(), "shutdown uninstalls adapter");

        adapter.shutdown(); // second call is a no-op
        assert!(!adapter.is_initialized());

        handle.shutdown().unwrap();
    }
}
