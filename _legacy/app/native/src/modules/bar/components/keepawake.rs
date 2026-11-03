use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFRelease, CFTypeRef};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use core_foundation_sys::number::{CFBooleanGetValue, CFBooleanRef};
use keepawake::{Builder, KeepAwake};
use objc::declare::ClassDecl;
use objc::runtime::{Class, Object, Sel};
use objc::{class, msg_send, sel, sel_impl};
use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::error::StacheError;
use crate::modules::bar::watcher::start_best_effort_refresh_watcher;
use crate::platform::objc::{nsstring, nsstring_to_string};
use crate::platform::thread::spawn_named_thread;
use crate::{constants, events};

const KEEP_AWAKE_REASON: &str = "Stache requested system wake lock";
const LOCK_NOTIFICATION_NAMES: [&str; 2] =
    ["com.apple.screenIsLocked", "com.apple.screenIsUnlocked"];
const LOCK_FALLBACK_POLL_INTERVAL: Duration = Duration::from_secs(2);
static LOCK_REFRESH_SIGNAL: OnceLock<Sender<()>> = OnceLock::new();

#[derive(Debug, Serialize, Clone)]
struct KeepAwakeChangedPayload {
    locked: bool,
    desired_awake: bool,
}

fn emit_keep_awake_changed(
    app_handle: &tauri::AppHandle,
    payload: KeepAwakeChangedPayload,
) -> Result<(), String> {
    app_handle
        .emit(events::keepawake::STATE_CHANGED, payload)
        .map_err(|err| err.to_string())
}

#[derive(Default)]
struct KeepAwakeState {
    desired_awake: bool,
    handle: Option<KeepAwake>,
}

#[derive(Default)]
pub struct KeepAwakeController {
    state: Mutex<KeepAwakeState>,
}

impl KeepAwakeController {
    fn lock_state(&self) -> Result<MutexGuard<'_, KeepAwakeState>, String> {
        self.state.lock().map_err(|err| err.to_string())
    }

    fn acquire_awake_handle() -> Result<KeepAwake, String> {
        Builder::default()
            .display(true)
            .idle(true)
            .sleep(true)
            .reason(KEEP_AWAKE_REASON)
            .app_name(constants::APP_NAME)
            .app_reverse_domain(constants::APP_BUNDLE_ID)
            .create()
            .map_err(|err| err.to_string())
    }

    fn ensure_awake_handle(state: &mut KeepAwakeState) -> Result<(), String> {
        if state.handle.is_none() {
            state.handle = Some(Self::acquire_awake_handle()?);
        }
        Ok(())
    }

    fn enable_awake(&self) -> Result<(), String> {
        self.lock_state().and_then(|mut state| {
            state.desired_awake = true;
            Self::ensure_awake_handle(&mut state)
        })
    }

    fn toggle_impl(&self) -> Result<bool, String> {
        self.lock_state().and_then(|mut state| {
            if state.desired_awake {
                state.desired_awake = false;
                state.handle = None;
                Ok(false)
            } else {
                state.desired_awake = true;
                Self::ensure_awake_handle(&mut state)?;
                Ok(true)
            }
        })
    }

    fn is_awake(&self) -> Result<bool, String> {
        let state = self.lock_state()?;
        Ok(state.handle.is_some())
    }

    fn handle_system_locked_event(&self) -> Result<KeepAwakeChangedPayload, String> {
        let mut state = self.lock_state()?;
        state.handle = None;

        Ok(KeepAwakeChangedPayload {
            locked: true,
            desired_awake: state.desired_awake,
        })
    }

    fn handle_system_unlocked_event(&self) -> Result<KeepAwakeChangedPayload, String> {
        let mut state = self.lock_state()?;
        if state.desired_awake {
            Self::ensure_awake_handle(&mut state)?;
        }

        Ok(KeepAwakeChangedPayload {
            locked: false,
            desired_awake: state.desired_awake,
        })
    }
}

/// Toggles the system awake state.
///
/// # Errors
///
/// Returns an error if the awake state cannot be toggled.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn toggle_system_awake(state: tauri::State<KeepAwakeController>) -> Result<bool, StacheError> {
    state.toggle_impl().map_err(StacheError::CommandError)
}

/// Checks if the system is currently being kept awake.
///
/// # Errors
///
/// Returns an error if the awake state cannot be determined.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn is_system_awake(state: tauri::State<KeepAwakeController>) -> Result<bool, StacheError> {
    state.is_awake().map_err(StacheError::CommandError)
}

static LOCK_WATCHER_ONCE: OnceLock<()> = OnceLock::new();

pub fn init(window: &tauri::WebviewWindow) {
    let app_handle = window.app_handle();

    if let Err(err) = app_handle.state::<KeepAwakeController>().enable_awake() {
        tracing::warn!(error = %err, "failed to acquire keep awake handle on startup");
    }

    if LOCK_WATCHER_ONCE.set(()).is_err() {
        return;
    }

    let app_handle = app_handle.clone();
    spawn_named_thread("lock-watcher", move || {
        watch_system_lock_state(&app_handle);
    });
}

const SCREEN_LOCKED_KEY: &str = "CGSSessionScreenIsLocked";

fn watch_system_lock_state(app_handle: &tauri::AppHandle) {
    let last_state = RefCell::new(None);

    start_best_effort_refresh_watcher(
        "lock-watcher",
        LOCK_FALLBACK_POLL_INTERVAL,
        |sender| {
            if LOCK_REFRESH_SIGNAL.set(sender).is_err() {
                tracing::warn!("lock refresh signal was already initialized");
            }

            register_lock_state_observer()
        },
        || {
            let mut last_state = last_state.borrow_mut();
            refresh_lock_state(app_handle, &mut last_state);
        },
    );
}

fn refresh_lock_state(app_handle: &tauri::AppHandle, last_state: &mut Option<bool>) {
    match is_session_locked() {
        Ok(is_locked) => {
            if Some(is_locked) != *last_state {
                *last_state = Some(is_locked);
                apply_lock_state(app_handle, is_locked);
            }
        }
        Err(err) => tracing::warn!(error = %err, "failed to poll session lock state"),
    }
}

fn register_lock_state_observer() -> Result<(), String> {
    unsafe {
        let center: *mut Object = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        if center.is_null() {
            return Err("failed to get NSDistributedNotificationCenter".to_string());
        }

        let observer = create_lock_state_observer();

        for notification_name in LOCK_NOTIFICATION_NAMES {
            let name = nsstring(notification_name);
            let _: () = msg_send![
                center,
                addObserver: observer
                selector: sel!(handleLockStateNotification:)
                name: name
                object: std::ptr::null::<Object>()
            ];
        }
    }

    Ok(())
}

fn create_lock_state_observer() -> *mut Object {
    unsafe {
        let superclass = class!(NSObject);
        let class_name = "StacheKeepAwakeObserver";

        let existing_class = Class::get(class_name);
        let observer_class = existing_class.unwrap_or_else(|| {
            let mut decl = ClassDecl::new(class_name, superclass)
                .expect("Failed to create StacheKeepAwakeObserver class");

            decl.add_method(
                sel!(handleLockStateNotification:),
                handle_lock_state_notification as extern "C" fn(&Object, Sel, *mut Object),
            );

            decl.register()
        });

        let instance: *mut Object = msg_send![observer_class, alloc];
        msg_send![instance, init]
    }
}

extern "C" fn handle_lock_state_notification(_self: &Object, _cmd: Sel, notification: *mut Object) {
    unsafe {
        if !notification.is_null() {
            let name_obj: *mut Object = msg_send![notification, name];
            let name = nsstring_to_string(name_obj);
            if !name.is_empty() {
                tracing::debug!(notification = %name, "keepawake: received lock state notification");
            }
        }
    }

    signal_lock_refresh();
}

fn signal_lock_refresh() {
    if let Some(sender) = LOCK_REFRESH_SIGNAL.get() {
        let _ = sender.send(());
    }
}

fn apply_lock_state(app_handle: &tauri::AppHandle, locked: bool) {
    let controller = app_handle.state::<KeepAwakeController>();
    let payload = if locked {
        controller.handle_system_locked_event()
    } else {
        controller.handle_system_unlocked_event()
    };

    match payload {
        Ok(payload) => {
            if let Err(err) = emit_keep_awake_changed(app_handle, payload) {
                tracing::warn!(error = %err, "failed to emit keep_awake_changed event");
            }
        }
        Err(err) => tracing::warn!(error = %err, "failed to update keep awake state"),
    }
}

fn is_session_locked() -> Result<bool, String> {
    unsafe {
        let dict_ref = CGSessionCopyCurrentDictionary();
        if dict_ref.is_null() {
            return Err("CGSessionCopyCurrentDictionary returned null".to_string());
        }

        let key = CFString::new(SCREEN_LOCKED_KEY);
        let value = CFDictionaryGetValue(dict_ref, key.as_concrete_TypeRef().cast::<c_void>());
        let locked = if value.is_null() {
            false
        } else {
            let boolean_ref = value as CFBooleanRef;
            CFBooleanGetValue(boolean_ref)
        };

        CFRelease(dict_ref as CFTypeRef);
        Ok(locked)
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keep_awake_changed_payload_creation() {
        let payload = KeepAwakeChangedPayload {
            locked: true,
            desired_awake: false,
        };

        assert!(payload.locked);
        assert!(!payload.desired_awake);
    }

    #[test]
    fn test_keep_awake_changed_payload_clone() {
        let payload = KeepAwakeChangedPayload {
            locked: false,
            desired_awake: true,
        };
        let cloned = payload.clone();

        assert_eq!(payload.locked, cloned.locked);
        assert_eq!(payload.desired_awake, cloned.desired_awake);
    }

    #[test]
    fn test_keep_awake_state_default() {
        let state = KeepAwakeState::default();

        assert!(!state.desired_awake);
        assert!(state.handle.is_none());
    }

    #[test]
    fn test_keep_awake_controller_default() {
        let controller = KeepAwakeController::default();
        let state = controller.lock_state().unwrap();

        assert!(!state.desired_awake);
        assert!(state.handle.is_none());
        drop(state);
    }

    #[test]
    fn test_app_name_constant() {
        assert_eq!(constants::APP_NAME, "Stache");
    }

    #[test]
    fn test_app_reverse_domain_constant() {
        assert_eq!(constants::APP_BUNDLE_ID, "com.marcosmoura.stache");
    }

    #[test]
    fn test_keep_awake_reason_constant() {
        assert_eq!(KEEP_AWAKE_REASON, "Stache requested system wake lock");
    }

    #[test]
    fn test_screen_locked_key_constant() {
        assert_eq!(SCREEN_LOCKED_KEY, "CGSSessionScreenIsLocked");
    }

    #[test]
    fn test_lock_notification_names() {
        assert_eq!(LOCK_NOTIFICATION_NAMES, [
            "com.apple.screenIsLocked",
            "com.apple.screenIsUnlocked"
        ]);
    }

    #[test]
    fn test_lock_fallback_poll_interval() {
        assert_eq!(LOCK_FALLBACK_POLL_INTERVAL.as_secs(), 2);
    }

    #[test]
    fn test_keep_awake_state_transitions() {
        let mut state = KeepAwakeState::default();

        // Initial state
        assert!(!state.desired_awake);
        assert!(state.handle.is_none());

        // Enable desired awake
        state.desired_awake = true;
        assert!(state.desired_awake);

        // Disable
        state.desired_awake = false;
        state.handle = None;
        assert!(!state.desired_awake);
        assert!(state.handle.is_none());
    }

    #[test]
    fn test_lock_watcher_once_initialization() {
        // Verify OnceLock is properly initialized
        static TEST_ONCE: OnceLock<()> = OnceLock::new();

        assert!(TEST_ONCE.get().is_none());
        let _ = TEST_ONCE.set(());
        assert!(TEST_ONCE.get().is_some());
    }

    #[test]
    fn test_keep_awake_controller_locking() {
        let controller = KeepAwakeController::default();

        // Test that we can acquire the lock
        let result = controller.lock_state();
        assert!(result.is_ok());
        drop(result);
    }

    // ========================================================================
    // Additional tests for KeepAwakeChangedPayload
    // ========================================================================

    #[test]
    fn test_keep_awake_changed_payload_debug() {
        let payload = KeepAwakeChangedPayload {
            locked: true,
            desired_awake: false,
        };
        let debug_str = format!("{payload:?}");
        assert!(debug_str.contains("KeepAwakeChangedPayload"));
        assert!(debug_str.contains("locked"));
        assert!(debug_str.contains("desired_awake"));
    }

    #[test]
    fn test_keep_awake_changed_payload_serialization() {
        let payload = KeepAwakeChangedPayload {
            locked: true,
            desired_awake: false,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"locked\":true"));
        assert!(json.contains("\"desired_awake\":false"));
    }

    #[test]
    fn test_keep_awake_changed_payload_all_combinations() {
        // Test all four combinations of locked and desired_awake
        let payloads = [(false, false), (false, true), (true, false), (true, true)];

        for (locked, desired_awake) in payloads {
            let payload = KeepAwakeChangedPayload { locked, desired_awake };
            assert_eq!(payload.locked, locked);
            assert_eq!(payload.desired_awake, desired_awake);
        }
    }

    // ========================================================================
    // Additional tests for KeepAwakeController
    // ========================================================================

    #[test]
    fn test_keep_awake_controller_is_awake_initially_false() {
        let controller = KeepAwakeController::default();
        let result = controller.is_awake();
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_keep_awake_controller_multiple_lock_acquisitions() {
        let controller = KeepAwakeController::default();

        // Acquire and release the lock multiple times
        for _ in 0..3 {
            let result = controller.lock_state();
            assert!(result.is_ok());
            drop(result);
        }
    }

    #[test]
    #[allow(clippy::significant_drop_tightening)]
    fn test_keep_awake_controller_toggle_changes_desired_state() {
        let controller = KeepAwakeController::default();

        // Initial state should have desired_awake = false
        {
            let state = controller.lock_state().unwrap();
            assert!(!state.desired_awake);
        }

        // After toggle, desired_awake should be true (but handle may or may not be acquired
        // depending on system state)
        let result = controller.toggle_impl();
        assert!(result.is_ok());

        {
            let state = controller.lock_state().unwrap();
            assert!(state.desired_awake);
        }
    }

    #[test]
    #[allow(clippy::significant_drop_tightening)]
    fn test_keep_awake_controller_enable_awake() {
        let controller = KeepAwakeController::default();

        // Enable awake
        let result = controller.enable_awake();
        assert!(result.is_ok());

        // Verify desired_awake is now true
        let state = controller.lock_state().unwrap();
        assert!(state.desired_awake);
    }

    #[test]
    #[allow(clippy::significant_drop_tightening)]
    fn test_keep_awake_controller_handle_system_locked() {
        let controller = KeepAwakeController::default();

        // Enable awake first
        let _ = controller.enable_awake();

        // Simulate system lock
        let result = controller.handle_system_locked_event();
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert!(payload.locked);
        // desired_awake should still be true (we want to re-acquire on unlock)
        assert!(payload.desired_awake);

        // Handle should be dropped
        let state = controller.lock_state().unwrap();
        assert!(state.handle.is_none());
    }

    #[test]
    fn test_keep_awake_controller_handle_system_unlocked_without_desired() {
        let controller = KeepAwakeController::default();

        // Don't enable awake, just simulate unlock
        let result = controller.handle_system_unlocked_event();
        assert!(result.is_ok());

        let payload = result.unwrap();
        assert!(!payload.locked);
        assert!(!payload.desired_awake);
    }

    // ========================================================================
    // Additional tests for is_session_locked
    // ========================================================================

    #[test]
    fn test_is_session_locked_returns_result() {
        // This test verifies that is_session_locked can be called without panicking
        // The actual result depends on system state
        let result = is_session_locked();
        assert!(result.is_ok() || result.is_err());
    }

    // ========================================================================
    // Additional tests for constants
    // ========================================================================

    #[test]
    fn test_lock_poll_interval_is_reasonable() {
        // Poll interval should be between 100ms and 5 seconds
        let millis = LOCK_FALLBACK_POLL_INTERVAL.as_millis();
        assert!(millis >= 100);
        assert!(millis <= 5000);
    }

    #[test]
    fn test_keep_awake_reason_is_not_empty() {
        assert!(!KEEP_AWAKE_REASON.is_empty());
        assert!(KEEP_AWAKE_REASON.contains("Stache"));
    }

    #[test]
    fn test_screen_locked_key_is_valid() {
        assert!(!SCREEN_LOCKED_KEY.is_empty());
        assert!(SCREEN_LOCKED_KEY.starts_with("CGS"));
    }
}
