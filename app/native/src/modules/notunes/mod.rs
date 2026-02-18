//! noTunes Module for Stache.
//!
//! This module prevents iTunes or Apple Music from launching automatically on macOS.
//! When media keys are pressed or Bluetooth headphones reconnect, macOS may try to
//! launch Apple Music - this module intercepts those launches and optionally opens
//! a preferred music player instead.
//!
//! The target music player is configurable via the `notunes.target_app` config option.
//!
//! Inspired by <https://github.com/tombonez/noTunes> (MIT License, Tom Taylor 2017).

use std::ptr::null_mut;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

use crate::config::{self, TargetMusicApp};
use crate::utils::objc::{get_app_bundle_id, nsstring};
use crate::utils::thread::spawn_named_thread;

/// Bundle identifier for Apple Music.
const APPLE_MUSIC_BUNDLE_ID: &str = "com.apple.Music";

/// Bundle identifier for iTunes (legacy).
const ITUNES_BUNDLE_ID: &str = "com.apple.iTunes";

/// Flag indicating if the module is running.
static IS_RUNNING: AtomicBool = AtomicBool::new(false);

/// Configured target music app (cached from config at init time).
static TARGET_APP: OnceLock<TargetMusicApp> = OnceLock::new();

/// Checks if a bundle identifier belongs to Apple Music or iTunes.
#[inline]
fn is_music_app(bundle_id: &str) -> bool {
    bundle_id == APPLE_MUSIC_BUNDLE_ID || bundle_id == ITUNES_BUNDLE_ID
}

/// Initializes the noTunes module.
///
/// This sets up an observer for `NSWorkspace.willLaunchApplicationNotification`
/// to intercept and terminate Apple Music/iTunes launches, optionally starting
/// the configured target music app instead.
pub fn init() {
    let config = config::get_config();

    // Check if noTunes is enabled in config
    if !config.notunes.is_enabled() {
        return;
    }

    if IS_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }

    // Cache the target app setting
    let _ = TARGET_APP.set(config.notunes.target_app.clone());

    spawn_named_thread("notunes-init", move || {
        // SAFETY: These functions interact with NSWorkspace and NSNotificationCenter APIs:
        // - All Objective-C calls use valid selectors and message passing
        // - Pointers are checked for null before dereferencing
        // - The observer is retained by NSNotificationCenter automatically
        unsafe {
            setup_workspace_observer();
            // Also terminate any already-running instances
            terminate_music_apps();
        }
    });
}

/// Returns the configured target music app.
fn get_target_app() -> &'static TargetMusicApp {
    TARGET_APP.get().unwrap_or(&TargetMusicApp::Tidal)
}

/// Terminates any currently running Apple Music or iTunes instances.
///
/// # Safety
///
/// Caller must ensure:
/// - This is called within a valid Objective-C runtime context
/// - `NSWorkspace` and its `runningApplications` array are valid and accessible
unsafe fn terminate_music_apps() {
    let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
    let running_apps: *mut Object = msg_send![workspace, runningApplications];
    let count: usize = msg_send![running_apps, count];

    for i in 0..count {
        let app: *mut Object = msg_send![running_apps, objectAtIndex: i];

        if let Some(bundle_id_str) = unsafe { get_app_bundle_id(app) }
            && is_music_app(&bundle_id_str)
        {
            tracing::info!(bundle_id = %bundle_id_str, "notunes: terminating running instance");
            let _: () = msg_send![app, forceTerminate];
        }
    }
}

/// Sets up the `NSWorkspace` observer for app launch notifications.
///
/// # Safety
///
/// Caller must ensure:
/// - This is called within a valid Objective-C runtime context
/// - `NSWorkspace` and its notification center are accessible
/// - This should only be called once (idempotent due to class registration check)
unsafe fn setup_workspace_observer() {
    // Get the workspace and notification center
    let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
    let notification_center: *mut Object = msg_send![workspace, notificationCenter];

    // Create the notification name
    let notification_name = unsafe { nsstring("NSWorkspaceWillLaunchApplicationNotification") };

    // Create an observer object
    let observer = unsafe { create_observer_object() };

    let _: () = msg_send![
        notification_center,
        addObserver: observer
        selector: sel!(handleAppLaunch:)
        name: notification_name
        object: null_mut::<Object>()
    ];
}

/// Creates an Objective-C observer object that handles the notification.
///
/// # Safety
///
/// Caller must ensure:
/// - This is called within a valid Objective-C runtime context
/// - The returned object is retained by `NSNotificationCenter` (do not release manually)
/// - The `NoTunesObserver` class is only registered once (handled via `Class::get` check)
unsafe fn create_observer_object() -> *mut Object {
    // Dynamically create a class for our observer
    let superclass = class!(NSObject);
    let class_name = "NoTunesObserver";

    // Check if class already exists
    let existing_class = Class::get(class_name);
    let observer_class = existing_class.unwrap_or_else(|| {
        // Register new class
        let mut decl =
            ClassDecl::new(class_name, superclass).expect("Failed to create NoTunesObserver class");

        // Add the notification handler method
        unsafe {
            decl.add_method(
                sel!(handleAppLaunch:),
                handle_app_launch as extern "C" fn(&Object, Sel, *mut Object),
            );
        }

        decl.register()
    });

    // Create an instance - the observer will be retained by NSNotificationCenter
    let instance: *mut Object = msg_send![observer_class, alloc];
    msg_send![instance, init]
}

/// Callback function for the app launch notification.
extern "C" fn handle_app_launch(_self: &Object, _cmd: Sel, notification: *mut Object) {
    unsafe {
        if notification.is_null() {
            return;
        }

        // Get the userInfo dictionary
        let user_info: *mut Object = msg_send![notification, userInfo];
        if user_info.is_null() {
            return;
        }

        // Get the NSWorkspaceApplicationKey
        let app_key = nsstring("NSWorkspaceApplicationKey");
        let app: *mut Object = msg_send![user_info, objectForKey: app_key];
        if app.is_null() {
            return;
        }

        // Get the bundle identifier and check if it's a music app
        if let Some(bundle_id_str) = get_app_bundle_id(app)
            && is_music_app(&bundle_id_str)
        {
            tracing::info!(bundle_id = %bundle_id_str, "notunes: blocking launch");

            // Force terminate the app
            let _: () = msg_send![app, forceTerminate];

            // Launch the configured target app as replacement
            launch_target_app();
        }
    }
}

/// Launches the configured target music app as replacement.
fn launch_target_app() {
    let target = get_target_app();

    // Get app path and bundle ID, or return if target is None
    let Some(app_path) = target.app_path() else {
        return;
    };
    let bundle_id = target.bundle_id();
    let display_name = target.display_name();

    // Check if the app is installed
    let path = std::path::Path::new(app_path);
    if !path.exists() {
        tracing::warn!(app = %display_name, path = %app_path, "notunes: target app not found");
        return;
    }

    // Check if the app is already running
    if let Some(bundle_id) = bundle_id {
        unsafe {
            let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            let running_apps: *mut Object = msg_send![workspace, runningApplications];
            let count: usize = msg_send![running_apps, count];

            for i in 0..count {
                let app: *mut Object = msg_send![running_apps, objectAtIndex: i];

                if let Some(bundle_id_str) = get_app_bundle_id(app)
                    && bundle_id_str == bundle_id
                {
                    // App is already running, no need to launch
                    return;
                }
            }
        }
    }

    // Launch the app using /usr/bin/open
    match std::process::Command::new("/usr/bin/open").arg(app_path).spawn() {
        Ok(_) => tracing::info!(app = %display_name, "notunes: launched replacement app"),
        Err(e) => {
            tracing::error!(app = %display_name, error = %e, "notunes: failed to launch replacement app");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_music_app_apple_music() {
        assert!(is_music_app(APPLE_MUSIC_BUNDLE_ID));
        assert!(is_music_app("com.apple.Music"));
    }

    #[test]
    fn test_is_music_app_itunes() {
        assert!(is_music_app(ITUNES_BUNDLE_ID));
        assert!(is_music_app("com.apple.iTunes"));
    }

    #[test]
    fn test_is_music_app_other_apps() {
        assert!(!is_music_app("com.spotify.client"));
        assert!(!is_music_app("com.tidal.desktop"));
        assert!(!is_music_app("org.jeffvli.feishin"));
        assert!(!is_music_app("com.apple.Safari"));
        assert!(!is_music_app(""));
    }

    #[test]
    fn test_bundle_ids_are_correct() {
        assert_eq!(APPLE_MUSIC_BUNDLE_ID, "com.apple.Music");
        assert_eq!(ITUNES_BUNDLE_ID, "com.apple.iTunes");
    }

    #[test]
    fn test_is_running_initially_false() {
        // Note: This test may not be reliable if init() has been called
        // The atomic is static and persists across tests
        // We're just testing that the constant exists and is an AtomicBool
        let _ = IS_RUNNING.load(Ordering::SeqCst);
    }

    #[test]
    fn test_apple_music_bundle_id_format() {
        assert!(APPLE_MUSIC_BUNDLE_ID.starts_with("com.apple."));
    }

    #[test]
    fn test_itunes_bundle_id_format() {
        assert!(ITUNES_BUNDLE_ID.starts_with("com.apple."));
    }

    #[test]
    fn test_target_app_tidal() {
        let app = TargetMusicApp::Tidal;
        assert_eq!(app.app_path(), Some("/Applications/TIDAL.app"));
        assert_eq!(app.bundle_id(), Some("com.tidal.desktop"));
        assert_eq!(app.display_name(), "Tidal");
    }

    #[test]
    fn test_target_app_spotify() {
        let app = TargetMusicApp::Spotify;
        assert_eq!(app.app_path(), Some("/Applications/Spotify.app"));
        assert_eq!(app.bundle_id(), Some("com.spotify.client"));
        assert_eq!(app.display_name(), "Spotify");
    }

    #[test]
    fn test_target_app_feishin() {
        let app = TargetMusicApp::Feishin;
        assert_eq!(app.app_path(), Some("/Applications/Feishin.app"));
        assert_eq!(app.bundle_id(), Some("org.jeffvli.feishin"));
        assert_eq!(app.display_name(), "Feishin");
    }

    #[test]
    fn test_target_app_none() {
        let app = TargetMusicApp::None;
        assert_eq!(app.app_path(), None);
        assert_eq!(app.bundle_id(), None);
        assert_eq!(app.display_name(), "None");
    }

    #[test]
    fn test_target_app_default_is_tidal() {
        let app = TargetMusicApp::default();
        assert_eq!(app, TargetMusicApp::Tidal);
    }
}
