//! noTunes Module for Barba Shell.
//!
//! This module prevents iTunes or Apple Music from launching automatically on macOS.
//! When media keys are pressed or Bluetooth headphones reconnect, macOS may try to
//! launch Apple Music - this module intercepts those launches and optionally opens
//! a preferred music player (Spotify) instead.
//!
//! Inspired by <https://github.com/tombonez/noTunes> (MIT License, Tom Taylor 2017).

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};

use crate::utils::thread::spawn_named_thread;

/// Bundle identifier for Apple Music.
const APPLE_MUSIC_BUNDLE_ID: &str = "com.apple.Music";

/// Bundle identifier for iTunes (legacy).
const ITUNES_BUNDLE_ID: &str = "com.apple.iTunes";

/// Path to the Spotify application.
const SPOTIFY_APP_PATH: &str = "/Applications/Spotify.app";

/// Spotify bundle identifier.
const SPOTIFY_BUNDLE_ID: &str = "com.spotify.client";

/// Flag indicating if the module is running.
static IS_RUNNING: AtomicBool = AtomicBool::new(false);

/// Initializes the noTunes module.
///
/// This sets up an observer for `NSWorkspace.willLaunchApplicationNotification`
/// to intercept and terminate Apple Music/iTunes launches, optionally starting
/// Spotify instead.
pub fn init() {
    if IS_RUNNING.swap(true, Ordering::SeqCst) {
        eprintln!("barba: notunes: module is already running");
        return;
    }

    spawn_named_thread("notunes-init", move || unsafe {
        setup_workspace_observer();
        // Also terminate any already-running instances
        terminate_music_apps();
    });

    eprintln!("barba: notunes: initialized - Apple Music/iTunes will be blocked");
}

/// Terminates any currently running Apple Music or iTunes instances.
unsafe fn terminate_music_apps() {
    let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
    let running_apps: *mut Object = msg_send![workspace, runningApplications];
    let count: usize = msg_send![running_apps, count];

    for i in 0..count {
        let app: *mut Object = msg_send![running_apps, objectAtIndex: i];
        let bundle_id: *mut Object = msg_send![app, bundleIdentifier];

        if bundle_id.is_null() {
            continue;
        }

        let bundle_id_str = unsafe { nsstring_to_string(bundle_id) };

        if bundle_id_str == APPLE_MUSIC_BUNDLE_ID || bundle_id_str == ITUNES_BUNDLE_ID {
            eprintln!("barba: notunes: terminating running instance of {bundle_id_str}");
            let _: () = msg_send![app, forceTerminate];
        }
    }
}

/// Sets up the `NSWorkspace` observer for app launch notifications.
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

        // Get the bundle identifier
        let bundle_id: *mut Object = msg_send![app, bundleIdentifier];
        if bundle_id.is_null() {
            return;
        }

        let bundle_id_str = nsstring_to_string(bundle_id);

        // Check if it's Apple Music or iTunes
        if bundle_id_str == APPLE_MUSIC_BUNDLE_ID || bundle_id_str == ITUNES_BUNDLE_ID {
            eprintln!("barba: notunes: blocking launch of {bundle_id_str}");

            // Force terminate the app
            let _: () = msg_send![app, forceTerminate];

            // Launch Spotify as replacement
            launch_spotify();
        }
    }
}

/// Launches Spotify as the replacement music player.
fn launch_spotify() {
    // Check if Spotify is installed
    let spotify_path = std::path::Path::new(SPOTIFY_APP_PATH);
    if !spotify_path.exists() {
        eprintln!("barba: notunes: Spotify not found at {SPOTIFY_APP_PATH}");
        return;
    }

    // Check if Spotify is already running
    unsafe {
        let workspace: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
        let running_apps: *mut Object = msg_send![workspace, runningApplications];
        let count: usize = msg_send![running_apps, count];

        for i in 0..count {
            let app: *mut Object = msg_send![running_apps, objectAtIndex: i];
            let bundle_id: *mut Object = msg_send![app, bundleIdentifier];

            if !bundle_id.is_null() {
                let bundle_id_str = nsstring_to_string(bundle_id);
                if bundle_id_str == SPOTIFY_BUNDLE_ID {
                    // Spotify is already running, no need to launch
                    return;
                }
            }
        }
    }

    // Launch Spotify using /usr/bin/open
    match std::process::Command::new("/usr/bin/open").arg(SPOTIFY_APP_PATH).spawn() {
        Ok(_) => eprintln!("barba: notunes: launched Spotify as replacement"),
        Err(e) => eprintln!("barba: notunes: failed to launch Spotify: {e}"),
    }
}

/// Creates an `NSString` from a Rust string.
unsafe fn nsstring(s: &str) -> *mut Object {
    let nsstring_class = class!(NSString);
    let bytes = s.as_ptr().cast::<c_void>();
    let len = s.len();
    let encoding: usize = 4; // NSUTF8StringEncoding

    msg_send![
        nsstring_class,
        stringWithBytes: bytes
        length: len
        encoding: encoding
    ]
}

/// Converts an `NSString` to a Rust String.
unsafe fn nsstring_to_string(nsstring: *mut Object) -> String {
    if nsstring.is_null() {
        return String::new();
    }

    let c_str: *const i8 = msg_send![nsstring, UTF8String];
    if c_str.is_null() {
        return String::new();
    }

    unsafe { std::ffi::CStr::from_ptr(c_str) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bundle_ids_are_correct() {
        assert_eq!(APPLE_MUSIC_BUNDLE_ID, "com.apple.Music");
        assert_eq!(ITUNES_BUNDLE_ID, "com.apple.iTunes");
    }

    #[test]
    fn test_spotify_path() {
        assert_eq!(SPOTIFY_APP_PATH, "/Applications/Spotify.app");
    }
}
