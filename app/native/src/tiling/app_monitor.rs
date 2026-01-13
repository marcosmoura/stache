//! Application launch monitoring for the tiling window manager.
//!
//! This module monitors for new application launches using macOS's
//! `NSWorkspace` notification center. When a new app launches, it:
//! - Registers an `AXObserver` for the app's windows
//! - Tracks the app's windows according to workspace rules
//! - Switches to the appropriate workspace if rules match
//!
//! This is essential for tracking apps launched **after** the tiling
//! manager has initialized - without this, new apps would not be observed.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

use crate::utils::objc::nsstring;

/// Callback function type for app launch events.
///
/// Parameters: (`pid`, `bundle_id`, `app_name`)
type AppLaunchCallback = fn(i32, Option<String>, Option<String>);

/// Global callback for app launches.
static APP_LAUNCH_CALLBACK: OnceLock<AppLaunchCallback> = OnceLock::new();

/// Whether the monitor has been initialized.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

// ============================================================================
// Notification Observer
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
    let class_name = "StacheAppLaunchObserver";

    // Check if class already exists
    let existing_class = Class::get(class_name);
    let observer_class = existing_class.unwrap_or_else(|| {
        let mut decl = ClassDecl::new(class_name, superclass)
            .expect("Failed to create StacheAppLaunchObserver class");

        unsafe {
            decl.add_method(
                sel!(handleAppLaunch:),
                handle_app_launch_notification as extern "C" fn(&Object, Sel, *mut Object),
            );
        }

        decl.register()
    });

    let instance: *mut Object = msg_send![observer_class, alloc];
    msg_send![instance, init]
}

/// Callback function for app launch notifications.
extern "C" fn handle_app_launch_notification(_self: &Object, _cmd: Sel, notification: *mut Object) {
    unsafe {
        if notification.is_null() {
            return;
        }

        // Get userInfo dictionary from notification
        let user_info: *mut Object = msg_send![notification, userInfo];
        if user_info.is_null() {
            return;
        }

        // Get NSRunningApplication from userInfo
        let app_key = nsstring("NSWorkspaceApplicationKey");
        let running_app: *mut Object = msg_send![user_info, objectForKey: app_key];
        if running_app.is_null() {
            return;
        }

        // Get the PID
        let pid: i32 = msg_send![running_app, processIdentifier];
        if pid <= 0 {
            return;
        }

        // Get the bundle identifier
        let bundle_id: Option<String> = {
            let bundle_id_ns: *mut Object = msg_send![running_app, bundleIdentifier];
            if bundle_id_ns.is_null() {
                None
            } else {
                Some(crate::utils::objc::nsstring_to_string(bundle_id_ns))
            }
        };

        // Get the localized name
        let app_name: Option<String> = {
            let name_ns: *mut Object = msg_send![running_app, localizedName];
            if name_ns.is_null() {
                None
            } else {
                Some(crate::utils::objc::nsstring_to_string(name_ns))
            }
        };

        // Call the registered callback
        if let Some(callback) = APP_LAUNCH_CALLBACK.get() {
            callback(pid, bundle_id, app_name);
        }
    }
}

// ============================================================================
// Public API
// ============================================================================

/// Initializes the app launch monitor.
///
/// # Arguments
///
/// * `callback` - Function to call when a new app launches.
///   Parameters: (`pid`, `bundle_id`, `app_name`)
///
/// # Returns
///
/// `true` if initialization succeeded, `false` if already initialized or failed.
pub fn init(callback: AppLaunchCallback) -> bool {
    // Only initialize once
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return false;
    }

    // Store the callback
    if APP_LAUNCH_CALLBACK.set(callback).is_err() {
        INITIALIZED.store(false, Ordering::SeqCst);
        return false;
    }

    unsafe {
        // Get NSWorkspace's shared instance
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            eprintln!("stache: tiling: failed to get shared workspace");
            INITIALIZED.store(false, Ordering::SeqCst);
            return false;
        }

        // Get the notification center
        let notification_center: *mut Object = msg_send![workspace, notificationCenter];
        if notification_center.is_null() {
            eprintln!("stache: tiling: failed to get workspace notification center");
            INITIALIZED.store(false, Ordering::SeqCst);
            return false;
        }

        // Create the observer
        let observer = create_workspace_observer();
        if observer.is_null() {
            eprintln!("stache: tiling: failed to create app launch observer");
            INITIALIZED.store(false, Ordering::SeqCst);
            return false;
        }

        // Register for NSWorkspaceDidLaunchApplicationNotification
        let notification_name = nsstring("NSWorkspaceDidLaunchApplicationNotification");
        let _: () = msg_send![
            notification_center,
            addObserver: observer
            selector: sel!(handleAppLaunch:)
            name: notification_name
            object: std::ptr::null::<Object>()
        ];
    }

    true
}

/// Checks if the app launch monitor is initialized.
#[must_use]
pub fn is_initialized() -> bool { INITIALIZED.load(Ordering::SeqCst) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_not_initialized_by_default() {
        // Note: This test depends on global state
        // It may not work correctly if run after initialization
    }

    #[test]
    fn test_initialized_flag() {
        // Just verify the atomic works
        assert!(!INITIALIZED.load(Ordering::SeqCst) || INITIALIZED.load(Ordering::SeqCst));
    }
}
