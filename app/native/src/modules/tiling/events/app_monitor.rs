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
    add_observer_for_pid, remove_observer_for_pid, should_observe_app,
};
use crate::platform::objc::nsstring;

// ============================================================================
// App Monitor Adapter
// ============================================================================

/// Adapter that receives app lifecycle events and routes them to the `EventProcessor`.
pub struct AppMonitorAdapter {
    /// Reference to the event processor.
    processor: Arc<EventProcessor>,

    /// Whether the adapter is initialized (observer registered).
    initialized: AtomicBool,
}

impl AppMonitorAdapter {
    /// Creates a new adapter with the given event processor.
    #[must_use]
    pub const fn new(processor: Arc<EventProcessor>) -> Self {
        Self {
            processor,
            initialized: AtomicBool::new(false),
        }
    }

    /// Initializes the adapter by registering with `NSNotificationCenter`.
    ///
    /// # Returns
    ///
    /// `true` if initialization succeeded, `false` if already initialized or failed.
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
        }

        tracing::debug!("AppMonitorAdapter initialized");
        true
    }

    /// Returns whether the adapter is initialized.
    #[must_use]
    pub fn is_initialized(&self) -> bool { self.initialized.load(Ordering::SeqCst) }

    /// Handles an app launch event.
    fn on_app_launched(&self, pid: i32, bundle_id: Option<String>, name: Option<String>) {
        let bundle_id = bundle_id.unwrap_or_default();
        let name = name.unwrap_or_default();

        tracing::debug!("App launched: pid={pid}, bundle={bundle_id}, name={name}");

        // Create AX observer for the new app (must happen on main thread)
        if should_observe_app(&bundle_id, &name)
            && let Err(e) = add_observer_for_pid(pid)
        {
            tracing::warn!("Failed to add observer for pid {pid}: {e}");
        }

        self.processor.on_app_launched(pid, bundle_id, name);
    }

    /// Handles an app termination event.
    fn on_app_terminated(&self, pid: i32, bundle_id: Option<&str>, name: Option<&str>) {
        tracing::debug!("App terminated: pid={pid}, bundle={bundle_id:?}, name={name:?}");

        // Remove the AX observer for this app (must happen on main thread)
        remove_observer_for_pid(pid);

        self.processor.on_app_terminated(pid);
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

    let (pid, bundle_id, app_name) = extract_app_info(notification);
    if pid <= 0 {
        return;
    }

    if let Some(adapter) = get_installed_adapter() {
        adapter.on_app_launched(pid, bundle_id, app_name);
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

    let (pid, bundle_id, app_name) = extract_app_info(notification);
    if pid <= 0 {
        return;
    }

    if let Some(adapter) = get_installed_adapter() {
        adapter.on_app_terminated(pid, bundle_id.as_deref(), app_name.as_deref());
    }
}

/// Extracts app info from an `NSNotification`.
fn extract_app_info(notification: *mut Object) -> (i32, Option<String>, Option<String>) {
    if notification.is_null() {
        return (0, None, None);
    }

    unsafe {
        // Get userInfo dictionary from notification
        let user_info: *mut Object = msg_send![notification, userInfo];
        if user_info.is_null() {
            return (0, None, None);
        }

        // Get NSRunningApplication from userInfo
        let app_key = nsstring("NSWorkspaceApplicationKey");
        let running_app: *mut Object = msg_send![user_info, objectForKey: app_key];
        if running_app.is_null() {
            return (0, None, None);
        }

        // Get the PID
        let pid: i32 = msg_send![running_app, processIdentifier];
        if pid <= 0 {
            return (0, None, None);
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

        (pid, bundle_id, app_name)
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

    #[tokio::test]
    async fn test_adapter_creation() {
        let handle = StateActor::spawn();
        let processor = Arc::new(EventProcessor::new(handle.clone()));
        let adapter = AppMonitorAdapter::new(processor);

        assert!(!adapter.is_initialized());

        handle.shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_global_adapter_install() {
        let handle = StateActor::spawn();
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
        let (pid, bundle_id, name) = extract_app_info(std::ptr::null_mut());
        assert_eq!(pid, 0);
        assert!(bundle_id.is_none());
        assert!(name.is_none());
    }
}
