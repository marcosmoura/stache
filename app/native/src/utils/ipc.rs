//! Inter-Process Communication via `NSDistributedNotificationCenter`.
//!
//! This module provides utilities for sending and receiving notifications between
//! the CLI and desktop app using macOS's distributed notification system.
//!
//! The notification center allows different processes to communicate without
//! requiring a shared file or socket. This is ideal for CLI → desktop app
//! communication where the CLI needs to notify the running app about events.

use std::sync::OnceLock;

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use parking_lot::Mutex;

use super::objc::nsstring;

/// Notification name prefix for all Barba notifications.
const NOTIFICATION_PREFIX: &str = "com.marcosmoura.barba.";

/// Notification types that can be sent between CLI and desktop app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarbaNotification {
    /// Window focus changed event.
    WindowFocusChanged,
    /// Workspace changed event with the new workspace name.
    WorkspaceChanged(String),
    /// Reload configuration request.
    Reload,
}

impl BarbaNotification {
    /// Returns the notification name for this event.
    fn notification_name(&self) -> String {
        let suffix = match self {
            Self::WindowFocusChanged => "window-focus-changed",
            Self::WorkspaceChanged(_) => "workspace-changed",
            Self::Reload => "reload",
        };
        format!("{NOTIFICATION_PREFIX}{suffix}")
    }

    /// Returns the user info dictionary for this notification, if any.
    fn user_info(&self) -> Option<Vec<(&str, &str)>> {
        match self {
            Self::WorkspaceChanged(name) => Some(vec![("workspace", name.as_str())]),
            _ => None,
        }
    }

    /// Parses a notification from its name and user info.
    fn from_notification(
        name: &str,
        user_info: Option<&std::collections::HashMap<String, String>>,
    ) -> Option<Self> {
        let suffix = name.strip_prefix(NOTIFICATION_PREFIX)?;

        match suffix {
            "window-focus-changed" => Some(Self::WindowFocusChanged),
            "workspace-changed" => {
                let workspace =
                    user_info.and_then(|info| info.get("workspace")).cloned().unwrap_or_default();
                Some(Self::WorkspaceChanged(workspace))
            }
            "reload" => Some(Self::Reload),
            _ => None,
        }
    }
}

/// Sends a notification to the running Barba desktop app.
///
/// This function posts a distributed notification that can be received by
/// any process listening for Barba notifications.
///
/// # Arguments
///
/// * `notification` - The notification to send.
///
/// # Returns
///
/// `true` if the notification was sent successfully, `false` otherwise.
pub fn send_notification(notification: &BarbaNotification) -> bool {
    unsafe {
        let center: *mut Object = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];

        if center.is_null() {
            eprintln!("barba: failed to get NSDistributedNotificationCenter");
            return false;
        }

        let name = nsstring(&notification.notification_name());

        // Create user info dictionary if needed
        let user_info: *mut Object = notification
            .user_info()
            .map_or(std::ptr::null_mut(), |info| create_ns_dictionary(&info));

        // Post the notification
        // deliverImmediately: YES ensures the notification is sent immediately
        let _: () = msg_send![
            center,
            postNotificationName: name
            object: std::ptr::null::<Object>()
            userInfo: user_info
            deliverImmediately: true
        ];

        true
    }
}

/// Creates an `NSDictionary` from key-value pairs.
unsafe fn create_ns_dictionary(pairs: &[(&str, &str)]) -> *mut Object {
    let dict_class = class!(NSMutableDictionary);
    let dict: *mut Object = msg_send![dict_class, new];

    for (key, value) in pairs {
        let ns_key = unsafe { nsstring(key) };
        let ns_value = unsafe { nsstring(value) };
        let _: () = msg_send![dict, setObject: ns_value forKey: ns_key];
    }

    dict
}

// ============================================================================
// Notification Listener (for desktop app)
// ============================================================================

/// Callback type for notification handlers.
pub type NotificationHandler = Box<dyn Fn(BarbaNotification) + Send + Sync>;

/// Global storage for notification handlers.
static NOTIFICATION_HANDLERS: OnceLock<Mutex<Vec<NotificationHandler>>> = OnceLock::new();

/// Registers a handler to receive Barba notifications.
///
/// This should be called by the desktop app during initialization to receive
/// notifications from CLI commands.
///
/// # Arguments
///
/// * `handler` - A callback function that will be called when a notification is received.
pub fn register_notification_handler<F>(handler: F)
where F: Fn(BarbaNotification) + Send + Sync + 'static {
    let handlers = NOTIFICATION_HANDLERS.get_or_init(|| Mutex::new(Vec::new()));
    handlers.lock().push(Box::new(handler));
}

/// Starts listening for Barba notifications.
///
/// This sets up observers for all Barba notification types. When a notification
/// is received, all registered handlers will be called.
///
/// This function should be called once during desktop app initialization.
pub fn start_notification_listener() {
    unsafe {
        let center: *mut Object = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];

        if center.is_null() {
            eprintln!("barba: failed to get NSDistributedNotificationCenter for listener");
            return;
        }

        // Create observer object
        let observer = create_notification_observer();

        // Register for all Barba notifications using a wildcard-like approach
        // We'll register for each specific notification type
        let notifications = [
            format!("{NOTIFICATION_PREFIX}window-focus-changed"),
            format!("{NOTIFICATION_PREFIX}workspace-changed"),
            format!("{NOTIFICATION_PREFIX}reload"),
        ];

        for notification_name in &notifications {
            let name = nsstring(notification_name);
            let _: () = msg_send![
                center,
                addObserver: observer
                selector: sel!(handleNotification:)
                name: name
                object: std::ptr::null::<Object>()
            ];
        }
    }
}

/// Creates an Objective-C observer object for handling notifications.
unsafe fn create_notification_observer() -> *mut Object {
    let superclass = class!(NSObject);
    let class_name = "BarbaNotificationObserver";

    // Check if class already exists
    let existing_class = Class::get(class_name);
    let observer_class = existing_class.unwrap_or_else(|| {
        let mut decl = ClassDecl::new(class_name, superclass)
            .expect("Failed to create BarbaNotificationObserver class");

        unsafe {
            decl.add_method(
                sel!(handleNotification:),
                handle_notification as extern "C" fn(&Object, Sel, *mut Object),
            );
        }

        decl.register()
    });

    let instance: *mut Object = msg_send![observer_class, alloc];
    msg_send![instance, init]
}

/// Callback function for distributed notifications.
extern "C" fn handle_notification(_self: &Object, _cmd: Sel, notification: *mut Object) {
    unsafe {
        if notification.is_null() {
            return;
        }

        // Get notification name
        let name_obj: *mut Object = msg_send![notification, name];
        if name_obj.is_null() {
            return;
        }

        let name = super::objc::nsstring_to_string(name_obj);

        // Get user info
        let user_info_obj: *mut Object = msg_send![notification, userInfo];
        let user_info = if user_info_obj.is_null() {
            None
        } else {
            Some(parse_ns_dictionary(user_info_obj))
        };

        // Parse the notification
        if let Some(barba_notification) =
            BarbaNotification::from_notification(&name, user_info.as_ref())
        {
            // Call all registered handlers
            if let Some(handlers) = NOTIFICATION_HANDLERS.get() {
                let handlers = handlers.lock();
                for handler in handlers.iter() {
                    handler(barba_notification.clone());
                }
            }
        }
    }
}

/// Parses an `NSDictionary` into a `HashMap`.
unsafe fn parse_ns_dictionary(dict: *mut Object) -> std::collections::HashMap<String, String> {
    let mut result = std::collections::HashMap::new();

    let keys: *mut Object = msg_send![dict, allKeys];
    if keys.is_null() {
        return result;
    }

    let count: usize = msg_send![keys, count];
    for i in 0..count {
        let key: *mut Object = msg_send![keys, objectAtIndex: i];
        let value: *mut Object = msg_send![dict, objectForKey: key];

        if !key.is_null() && !value.is_null() {
            let key_str = unsafe { super::objc::nsstring_to_string(key) };
            let value_str = unsafe { super::objc::nsstring_to_string(value) };
            result.insert(key_str, value_str);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_name_window_focus_changed() {
        let notification = BarbaNotification::WindowFocusChanged;
        assert_eq!(
            notification.notification_name(),
            "com.marcosmoura.barba.window-focus-changed"
        );
    }

    #[test]
    fn test_notification_name_workspace_changed() {
        let notification = BarbaNotification::WorkspaceChanged("coding".to_string());
        assert_eq!(
            notification.notification_name(),
            "com.marcosmoura.barba.workspace-changed"
        );
    }

    #[test]
    fn test_notification_name_reload() {
        let notification = BarbaNotification::Reload;
        assert_eq!(notification.notification_name(), "com.marcosmoura.barba.reload");
    }

    #[test]
    fn test_user_info_window_focus_changed() {
        let notification = BarbaNotification::WindowFocusChanged;
        assert!(notification.user_info().is_none());
    }

    #[test]
    fn test_user_info_workspace_changed() {
        let notification = BarbaNotification::WorkspaceChanged("coding".to_string());
        let info = notification.user_info();
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0], ("workspace", "coding"));
    }

    #[test]
    fn test_user_info_reload() {
        let notification = BarbaNotification::Reload;
        assert!(notification.user_info().is_none());
    }

    #[test]
    fn test_from_notification_window_focus_changed() {
        let notification = BarbaNotification::from_notification(
            "com.marcosmoura.barba.window-focus-changed",
            None,
        );
        assert_eq!(notification, Some(BarbaNotification::WindowFocusChanged));
    }

    #[test]
    fn test_from_notification_workspace_changed() {
        let mut user_info = std::collections::HashMap::new();
        user_info.insert("workspace".to_string(), "coding".to_string());

        let notification = BarbaNotification::from_notification(
            "com.marcosmoura.barba.workspace-changed",
            Some(&user_info),
        );
        assert_eq!(
            notification,
            Some(BarbaNotification::WorkspaceChanged("coding".to_string()))
        );
    }

    #[test]
    fn test_from_notification_reload() {
        let notification =
            BarbaNotification::from_notification("com.marcosmoura.barba.reload", None);
        assert_eq!(notification, Some(BarbaNotification::Reload));
    }

    #[test]
    fn test_from_notification_unknown() {
        let notification =
            BarbaNotification::from_notification("com.marcosmoura.barba.unknown", None);
        assert!(notification.is_none());
    }

    #[test]
    fn test_from_notification_wrong_prefix() {
        let notification = BarbaNotification::from_notification("com.other.app.reload", None);
        assert!(notification.is_none());
    }
}
