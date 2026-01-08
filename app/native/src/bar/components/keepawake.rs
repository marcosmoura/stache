use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFRelease, CFTypeRef};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use core_foundation_sys::number::{CFBooleanGetValue, CFBooleanRef};
use keepawake::{Builder, KeepAwake};
use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::error::StacheError;
use crate::utils::thread::spawn_named_thread;
use crate::{constants, events};

const KEEP_AWAKE_REASON: &str = "Stache requested system wake lock";

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

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn toggle_system_awake(state: tauri::State<KeepAwakeController>) -> Result<bool, StacheError> {
    state.toggle_impl().map_err(StacheError::CommandError)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn is_system_awake(state: tauri::State<KeepAwakeController>) -> Result<bool, StacheError> {
    state.is_awake().map_err(StacheError::CommandError)
}

static LOCK_WATCHER_ONCE: OnceLock<()> = OnceLock::new();

pub fn init(window: &tauri::WebviewWindow) {
    let app_handle = window.app_handle();

    if let Err(err) = app_handle.state::<KeepAwakeController>().enable_awake() {
        eprintln!("Failed to acquire keep awake handle on startup: {err}");
    }

    if LOCK_WATCHER_ONCE.set(()).is_err() {
        return;
    }

    let app_handle = app_handle.clone();
    spawn_named_thread("lock-watcher", move || {
        if let Err(err) = watch_system_lock_state(&app_handle) {
            eprintln!("Failed to start system lock watcher: {err}");
        }
    });
}

const SCREEN_LOCKED_KEY: &str = "CGSSessionScreenIsLocked";

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(500);

fn watch_system_lock_state(app_handle: &tauri::AppHandle) -> Result<(), String> {
    let mut last_state: Option<bool> = None;

    loop {
        match is_session_locked() {
            Ok(is_locked) => {
                if Some(is_locked) != last_state {
                    last_state = Some(is_locked);
                    apply_lock_state(app_handle, is_locked);
                }
            }
            Err(err) => eprintln!("Failed to poll session lock state: {err}"),
        }

        thread::sleep(LOCK_POLL_INTERVAL);
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
                eprintln!("Failed to emit keep_awake_changed: {err}");
            }
        }
        Err(err) => eprintln!("Failed to update keep awake state: {err}"),
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
    fn test_lock_poll_interval() {
        assert_eq!(LOCK_POLL_INTERVAL.as_millis(), 500);
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
}
