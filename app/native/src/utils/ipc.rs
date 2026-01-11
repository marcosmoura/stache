//! Inter-Process Communication via `NSDistributedNotificationCenter`.
//!
//! This module provides utilities for sending and receiving notifications between
//! the CLI and desktop app using macOS's distributed notification system.
//!
//! The notification center allows different processes to communicate without
//! requiring a shared file or socket. This is ideal for CLI -> desktop app
//! communication where the CLI needs to notify the running app about events.

use std::sync::OnceLock;

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use parking_lot::Mutex;

use super::objc::nsstring;

/// Notification name prefix for all Stache notifications.
const NOTIFICATION_PREFIX: &str = "com.marcosmoura.stache.";

/// Notification types that can be sent between CLI and desktop app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StacheNotification {
    /// Window focus changed event.
    WindowFocusChanged,
    /// Workspace changed event with the new workspace name.
    WorkspaceChanged(String),
    /// Reload configuration request.
    Reload,

    // Tiling window manager notifications
    /// Focus a workspace by name.
    TilingFocusWorkspace(String),
    /// Change layout of focused workspace.
    TilingSetLayout(String),
    /// Focus window in direction or by ID.
    TilingWindowFocus(String),
    /// Swap focused window with neighbor in direction.
    TilingWindowSwap(String),
    /// Resize focused window.
    TilingWindowResize { dimension: String, amount: i32 },
    /// Apply floating preset to focused window.
    TilingWindowPreset(String),
    /// Send focused window to workspace.
    TilingWindowSendToWorkspace(String),
    /// Send focused window to screen.
    TilingWindowSendToScreen(String),
    /// Balance focused workspace.
    TilingWorkspaceBalance,
    /// Send focused workspace to screen.
    TilingWorkspaceSendToScreen(String),
}

impl StacheNotification {
    /// Returns the notification name for this event.
    fn notification_name(&self) -> String {
        let suffix = match self {
            Self::WindowFocusChanged => "window-focus-changed",
            Self::WorkspaceChanged(_) => "workspace-changed",
            Self::Reload => "reload",
            // Tiling notifications
            Self::TilingFocusWorkspace(_) => "tiling-focus-workspace",
            Self::TilingSetLayout(_) => "tiling-set-layout",
            Self::TilingWindowFocus(_) => "tiling-window-focus",
            Self::TilingWindowSwap(_) => "tiling-window-swap",
            Self::TilingWindowResize { .. } => "tiling-window-resize",
            Self::TilingWindowPreset(_) => "tiling-window-preset",
            Self::TilingWindowSendToWorkspace(_) => "tiling-window-send-to-workspace",
            Self::TilingWindowSendToScreen(_) => "tiling-window-send-to-screen",
            Self::TilingWorkspaceBalance => "tiling-workspace-balance",
            Self::TilingWorkspaceSendToScreen(_) => "tiling-workspace-send-to-screen",
        };
        format!("{NOTIFICATION_PREFIX}{suffix}")
    }

    /// Returns the user info dictionary for this notification, if any.
    fn user_info(&self) -> Option<Vec<(&str, String)>> {
        match self {
            Self::WorkspaceChanged(name) => Some(vec![("workspace", name.clone())]),
            // Tiling notifications with parameters
            Self::TilingFocusWorkspace(workspace) => Some(vec![("workspace", workspace.clone())]),
            Self::TilingSetLayout(layout) => Some(vec![("layout", layout.clone())]),
            Self::TilingWindowFocus(target) => Some(vec![("target", target.clone())]),
            Self::TilingWindowSwap(direction) => Some(vec![("direction", direction.clone())]),
            Self::TilingWindowResize { dimension, amount } => Some(vec![
                ("dimension", dimension.clone()),
                ("amount", amount.to_string()),
            ]),
            Self::TilingWindowPreset(preset) => Some(vec![("preset", preset.clone())]),
            Self::TilingWindowSendToWorkspace(workspace) => {
                Some(vec![("workspace", workspace.clone())])
            }
            Self::TilingWindowSendToScreen(screen) | Self::TilingWorkspaceSendToScreen(screen) => {
                Some(vec![("screen", screen.clone())])
            }
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
            // Tiling notifications
            "tiling-focus-workspace" => {
                let workspace =
                    user_info.and_then(|info| info.get("workspace")).cloned().unwrap_or_default();
                Some(Self::TilingFocusWorkspace(workspace))
            }
            "tiling-set-layout" => {
                let layout =
                    user_info.and_then(|info| info.get("layout")).cloned().unwrap_or_default();
                Some(Self::TilingSetLayout(layout))
            }
            "tiling-window-focus" => {
                let target =
                    user_info.and_then(|info| info.get("target")).cloned().unwrap_or_default();
                Some(Self::TilingWindowFocus(target))
            }
            "tiling-window-swap" => {
                let direction =
                    user_info.and_then(|info| info.get("direction")).cloned().unwrap_or_default();
                Some(Self::TilingWindowSwap(direction))
            }
            "tiling-window-resize" => {
                let dimension =
                    user_info.and_then(|info| info.get("dimension")).cloned().unwrap_or_default();
                let amount = user_info
                    .and_then(|info| info.get("amount"))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                Some(Self::TilingWindowResize { dimension, amount })
            }
            "tiling-window-preset" => {
                let preset =
                    user_info.and_then(|info| info.get("preset")).cloned().unwrap_or_default();
                Some(Self::TilingWindowPreset(preset))
            }
            "tiling-window-send-to-workspace" => {
                let workspace =
                    user_info.and_then(|info| info.get("workspace")).cloned().unwrap_or_default();
                Some(Self::TilingWindowSendToWorkspace(workspace))
            }
            "tiling-window-send-to-screen" => {
                let screen =
                    user_info.and_then(|info| info.get("screen")).cloned().unwrap_or_default();
                Some(Self::TilingWindowSendToScreen(screen))
            }
            "tiling-workspace-balance" => Some(Self::TilingWorkspaceBalance),
            "tiling-workspace-send-to-screen" => {
                let screen =
                    user_info.and_then(|info| info.get("screen")).cloned().unwrap_or_default();
                Some(Self::TilingWorkspaceSendToScreen(screen))
            }
            _ => None,
        }
    }
}

/// Sends a notification to the running Stache desktop app.
///
/// This function posts a distributed notification that can be received by
/// any process listening for Stache notifications.
///
/// # Arguments
///
/// * `notification` - The notification to send.
///
/// # Returns
///
/// `true` if the notification was sent successfully, `false` otherwise.
pub fn send_notification(notification: &StacheNotification) -> bool {
    // SAFETY: We are calling well-defined Objective-C APIs via FFI:
    // - NSDistributedNotificationCenter is thread-safe and can be called from any thread
    // - All pointers are checked for null before use
    // - The notification center handles memory management for posted notifications
    unsafe {
        let center: *mut Object = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];

        if center.is_null() {
            eprintln!("stache: failed to get NSDistributedNotificationCenter");
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
///
/// # Safety
///
/// Caller must ensure:
/// - This is called within a valid Objective-C runtime context
/// - The returned pointer is either used immediately or properly retained
unsafe fn create_ns_dictionary(pairs: &[(&str, String)]) -> *mut Object {
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
pub type NotificationHandler = Box<dyn Fn(StacheNotification) + Send + Sync>;

/// Global storage for notification handlers.
static NOTIFICATION_HANDLERS: OnceLock<Mutex<Vec<NotificationHandler>>> = OnceLock::new();

/// Registers a handler to receive Stache notifications.
///
/// This should be called by the desktop app during initialization to receive
/// notifications from CLI commands.
///
/// # Arguments
///
/// * `handler` - A callback function that will be called when a notification is received.
pub fn register_notification_handler<F>(handler: F)
where F: Fn(StacheNotification) + Send + Sync + 'static {
    let handlers = NOTIFICATION_HANDLERS.get_or_init(|| Mutex::new(Vec::new()));
    handlers.lock().push(Box::new(handler));
}

/// Starts listening for Stache notifications.
///
/// This sets up observers for all Stache notification types. When a notification
/// is received, all registered handlers will be called.
///
/// This function should be called once during desktop app initialization.
pub fn start_notification_listener() {
    // SAFETY: We are setting up NSDistributedNotificationCenter observers:
    // - The notification center is obtained via the standard defaultCenter method
    // - The observer object is retained by the notification center
    // - All string parameters are valid NSStrings created via nsstring()
    // - This function is idempotent and can be called multiple times safely
    unsafe {
        let center: *mut Object = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];

        if center.is_null() {
            eprintln!("stache: failed to get NSDistributedNotificationCenter for listener");
            return;
        }

        // Create observer object
        let observer = create_notification_observer();

        // Register for all Stache notifications using a wildcard-like approach
        // We'll register for each specific notification type
        let notifications = [
            format!("{NOTIFICATION_PREFIX}window-focus-changed"),
            format!("{NOTIFICATION_PREFIX}workspace-changed"),
            format!("{NOTIFICATION_PREFIX}reload"),
            // Tiling notifications
            format!("{NOTIFICATION_PREFIX}tiling-focus-workspace"),
            format!("{NOTIFICATION_PREFIX}tiling-set-layout"),
            format!("{NOTIFICATION_PREFIX}tiling-window-focus"),
            format!("{NOTIFICATION_PREFIX}tiling-window-swap"),
            format!("{NOTIFICATION_PREFIX}tiling-window-resize"),
            format!("{NOTIFICATION_PREFIX}tiling-window-preset"),
            format!("{NOTIFICATION_PREFIX}tiling-window-send-to-workspace"),
            format!("{NOTIFICATION_PREFIX}tiling-window-send-to-screen"),
            format!("{NOTIFICATION_PREFIX}tiling-workspace-balance"),
            format!("{NOTIFICATION_PREFIX}tiling-workspace-send-to-screen"),
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
///
/// # Safety
///
/// Caller must ensure:
/// - This is called within a valid Objective-C runtime context
/// - The returned object is retained by `NSNotificationCenter` (do not release manually)
/// - The class is only registered once (handled internally via `Class::get` check)
unsafe fn create_notification_observer() -> *mut Object {
    let superclass = class!(NSObject);
    let class_name = "StacheNotificationObserver";

    // Check if class already exists
    let existing_class = Class::get(class_name);
    let observer_class = existing_class.unwrap_or_else(|| {
        let mut decl = ClassDecl::new(class_name, superclass)
            .expect("Failed to create StacheNotificationObserver class");

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
        if let Some(stache_notification) =
            StacheNotification::from_notification(&name, user_info.as_ref())
        {
            // Call all registered handlers
            if let Some(handlers) = NOTIFICATION_HANDLERS.get() {
                let handlers = handlers.lock();
                for handler in handlers.iter() {
                    handler(stache_notification.clone());
                }
            }
        }
    }
}

/// Parses an `NSDictionary` into a `HashMap`.
///
/// # Safety
///
/// Caller must ensure:
/// - `dict` is a valid pointer to an `NSDictionary` (or null, which returns empty `HashMap`)
/// - The dictionary contains only `NSString` keys and values
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
        let notification = StacheNotification::WindowFocusChanged;
        assert_eq!(
            notification.notification_name(),
            "com.marcosmoura.stache.window-focus-changed"
        );
    }

    #[test]
    fn test_notification_name_workspace_changed() {
        let notification = StacheNotification::WorkspaceChanged("coding".to_string());
        assert_eq!(
            notification.notification_name(),
            "com.marcosmoura.stache.workspace-changed"
        );
    }

    #[test]
    fn test_notification_name_reload() {
        let notification = StacheNotification::Reload;
        assert_eq!(notification.notification_name(), "com.marcosmoura.stache.reload");
    }

    #[test]
    fn test_user_info_window_focus_changed() {
        let notification = StacheNotification::WindowFocusChanged;
        assert!(notification.user_info().is_none());
    }

    #[test]
    fn test_user_info_workspace_changed() {
        let notification = StacheNotification::WorkspaceChanged("coding".to_string());
        let info = notification.user_info();
        assert!(info.is_some());
        let info = info.unwrap();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0], ("workspace", "coding".to_string()));
    }

    #[test]
    fn test_user_info_reload() {
        let notification = StacheNotification::Reload;
        assert!(notification.user_info().is_none());
    }

    #[test]
    fn test_from_notification_window_focus_changed() {
        let notification = StacheNotification::from_notification(
            "com.marcosmoura.stache.window-focus-changed",
            None,
        );
        assert_eq!(notification, Some(StacheNotification::WindowFocusChanged));
    }

    #[test]
    fn test_from_notification_workspace_changed() {
        let mut user_info = std::collections::HashMap::new();
        user_info.insert("workspace".to_string(), "coding".to_string());

        let notification = StacheNotification::from_notification(
            "com.marcosmoura.stache.workspace-changed",
            Some(&user_info),
        );
        assert_eq!(
            notification,
            Some(StacheNotification::WorkspaceChanged("coding".to_string()))
        );
    }

    #[test]
    fn test_from_notification_reload() {
        let notification =
            StacheNotification::from_notification("com.marcosmoura.stache.reload", None);
        assert_eq!(notification, Some(StacheNotification::Reload));
    }

    #[test]
    fn test_from_notification_unknown() {
        let notification =
            StacheNotification::from_notification("com.marcosmoura.stache.unknown", None);
        assert!(notification.is_none());
    }

    #[test]
    fn test_from_notification_wrong_prefix() {
        let notification = StacheNotification::from_notification("com.other.app.reload", None);
        assert!(notification.is_none());
    }
}
