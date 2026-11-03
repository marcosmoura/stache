//! Audio device watcher using `CoreAudio` property listeners.
//!
//! This module handles monitoring for audio device changes and
//! automatically applying priority-based device switching.

use std::ffi::c_void;
use std::ptr::{NonNull, null};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};

use objc2_core_audio::{
    AudioDeviceID, AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectRemovePropertyListener, AudioObjectSetPropertyData, kAudioHardwareNoError,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};

use super::device::{
    get_default_input_device, get_default_output_device, get_input_devices, get_output_devices,
};
use super::priority;
use crate::config::ProxyAudioConfig;
use crate::modules::services::lifecycle::{LifecycleModule, ModuleStatus};

/// One active generation of audio property listeners: the exact three
/// `AudioObjectPropertyAddress` values, the owned `Sender` whose heap address
/// is the `client_data` passed to `CoreAudio`, and the watcher worker so pause
/// can join it. A fresh value is built on every resume; pause removes the
/// listeners, disconnects the channel, and joins the worker before `RUNTIME`
/// is left empty.
struct RetainedListeners {
    #[allow(dead_code)] // generation identity kept for diagnostics
    generation: u64,
    addresses: [AudioObjectPropertyAddress; 3],
    sender: Box<Sender<()>>,
    worker: std::thread::JoinHandle<()>,
}

static RUNTIME: Mutex<Option<RetainedListeners>> = Mutex::new(None);
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Size of `AudioDeviceID` in bytes as u32.
#[allow(clippy::cast_possible_truncation)] // AudioDeviceID is u32, so size is always 4 bytes
const AUDIO_DEVICE_ID_SIZE: u32 = std::mem::size_of::<AudioDeviceID>() as u32;

/// Sets the default output device.
///
/// Returns `true` if the device was set successfully.
fn set_default_output_device(device_id: AudioDeviceID) -> bool {
    let property_address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };

    let status = unsafe {
        AudioObjectSetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&property_address),
            0,
            null(),
            AUDIO_DEVICE_ID_SIZE,
            NonNull::from(&device_id).cast(),
        )
    };

    status == kAudioHardwareNoError
}

/// Sets the default input device.
///
/// Returns `true` if the device was set successfully.
fn set_default_input_device(device_id: AudioDeviceID) -> bool {
    let property_address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultInputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };

    let status = unsafe {
        AudioObjectSetPropertyData(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&property_address),
            0,
            null(),
            AUDIO_DEVICE_ID_SIZE,
            NonNull::from(&device_id).cast(),
        )
    };

    status == kAudioHardwareNoError
}

/// Handles output device changes by applying priority rules from config.
fn handle_output_device_change(config: &ProxyAudioConfig) {
    let Some(current) = get_default_output_device() else {
        return;
    };

    let devices = get_output_devices();
    let target = priority::get_target_output_device(&current, &devices, config);

    let Some(target) = target else {
        return;
    };

    if current.id == target.id {
        return;
    }

    let name = &target.name;
    if set_default_output_device(target.id) {
        tracing::info!(device = %name, "default output device changed");
    } else {
        tracing::error!(device = %name, "failed to set default output device");
    }
}

/// Handles input device changes by applying priority rules from config.
fn handle_input_device_change(config: &ProxyAudioConfig) {
    let Some(current) = get_default_input_device() else {
        return;
    };

    let devices = get_input_devices();
    let target = priority::get_target_input_device(&current, &devices, config);

    let Some(target) = target else {
        return;
    };

    if current.id == target.id {
        return;
    }

    let name = &target.name;
    if set_default_input_device(target.id) {
        tracing::info!(device = %name, "default input device changed");
    } else {
        tracing::error!(device = %name, "failed to set default input device");
    }
}

/// Handles all audio device changes.
///
/// This is called whenever an audio device is connected, disconnected,
/// or when the default device changes. Requires config to be present.
fn on_audio_device_change(config: &ProxyAudioConfig) {
    handle_output_device_change(config);
    handle_input_device_change(config);
}

/// Property listener callback for audio device changes.
///
/// # Safety
///
/// This function is called by `CoreAudio` and expects valid pointers.
unsafe extern "C-unwind" fn audio_device_property_listener(
    _in_object_id: AudioObjectID,
    _in_number_addresses: u32,
    _in_addresses: NonNull<AudioObjectPropertyAddress>,
    in_client_data: *mut c_void,
) -> i32 {
    if !in_client_data.is_null() {
        // SAFETY: We know in_client_data is a valid Sender pointer from init_audio_device_watcher
        let tx = unsafe { &*in_client_data.cast::<Sender<()>>() };
        let _ = tx.send(());
    }
    0 // kAudioHardwareNoError
}

/// Builds the three default-device/device-list property addresses.
const fn listener_addresses() -> [AudioObjectPropertyAddress; 3] {
    let output = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let input = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultInputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let devices = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    [output, input, devices]
}

/// Starts one generation of the audio watcher: registers the three listeners
/// with the exact same callback and `client_data` pointer, spawns the worker
/// thread, and publishes the runtime. Rolls back any already-registered
/// listeners on partial failure.
fn start_generation(config: &ProxyAudioConfig) -> Result<(), String> {
    if RUNTIME.lock().unwrap().is_some() {
        return Err("proxyAudio watcher already running".into());
    }

    let (tx, rx) = channel();
    let sender = Box::new(tx);
    // The Box's heap address is stable; this is the pointer CoreAudio holds.
    let tx_ptr: *mut c_void = std::ptr::from_ref::<Sender<()>>(sender.as_ref()).cast_mut().cast();

    let addresses = listener_addresses();
    let mut registered = 0usize;
    for addr in &addresses {
        let status = unsafe {
            AudioObjectAddPropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(addr),
                Some(audio_device_property_listener),
                tx_ptr,
            )
        };
        if status != kAudioHardwareNoError {
            break;
        }
        registered += 1;
    }

    if registered != addresses.len() {
        for addr in &addresses[..registered] {
            unsafe {
                AudioObjectRemovePropertyListener(
                    kAudioObjectSystemObject as AudioObjectID,
                    NonNull::from(addr),
                    Some(audio_device_property_listener),
                    tx_ptr,
                );
            }
        }
        return Err("failed to register audio property listeners".into());
    }

    let generation = NEXT_GENERATION.fetch_add(1, Ordering::SeqCst);
    let config = config.clone();
    let worker = std::thread::Builder::new()
        .name("stache-audio-device-watcher".into())
        .spawn(move || {
            while rx.recv().is_ok() {
                on_audio_device_change(&config);
            }
        })
        .map_err(|e| format!("failed to spawn audio watcher thread: {e}"))?;

    *RUNTIME.lock().unwrap() = Some(RetainedListeners {
        generation,
        addresses,
        sender,
        worker,
    });
    Ok(())
}

/// Removes the current generation: unregisters the three listeners with the
/// exact callback/`client_data` used to register, disconnects the channel (the
/// worker's `recv` then errors and the worker exits), and joins the worker.
fn remove_generation() {
    let Some(runtime) = RUNTIME.lock().unwrap().take() else {
        return;
    };
    let tx_ptr: *mut c_void =
        std::ptr::from_ref::<Sender<()>>(runtime.sender.as_ref()).cast_mut().cast();

    for addr in &runtime.addresses {
        let status = unsafe {
            AudioObjectRemovePropertyListener(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(addr),
                Some(audio_device_property_listener),
                tx_ptr,
            )
        };
        if status != kAudioHardwareNoError {
            tracing::warn!(status, "proxyAudio: listener removal reported an error");
        }
    }

    drop(runtime.sender);
    if runtime.worker.join().is_err() {
        tracing::error!("proxyAudio: audio watcher worker panicked");
    }
}

/// Starts the audio device watcher (idempotent when a generation is running).
///
/// # Arguments
///
/// * `config` - Proxy audio configuration for device priority rules.
pub fn start(config: &ProxyAudioConfig) {
    if RUNTIME.lock().unwrap().is_some() {
        return;
    }
    on_audio_device_change(config);
    if let Err(e) = start_generation(config) {
        tracing::error!(error = %e, "proxyAudio: failed to start watcher");
    }
}

/// Pure status decision so the global `RUNTIME` slot is not required in tests.
const fn proxy_audio_status(config_enabled: bool, runtime_present: bool) -> ModuleStatus {
    if !config_enabled {
        return ModuleStatus::ConfiguredOff;
    }
    if runtime_present {
        ModuleStatus::Running
    } else {
        ModuleStatus::Paused
    }
}

/// Tray-toggleable lifecycle handle for proxyAudio.
pub struct ProxyAudioLifecycle;

impl LifecycleModule for ProxyAudioLifecycle {
    fn name(&self) -> &'static str { "Proxy Audio" }

    fn id(&self) -> &'static str { "proxyAudio" }

    fn start(&self) -> Result<(), String> {
        if RUNTIME.lock().unwrap().is_some() {
            return Ok(());
        }
        let config = crate::config::get_config().proxy_audio.clone();
        on_audio_device_change(&config);
        start_generation(&config)
    }

    fn pause(&self) -> Result<(), String> {
        remove_generation();
        Ok(())
    }

    fn resume(&self) -> Result<(), String> { self.start() }

    fn status(&self) -> ModuleStatus {
        let config_enabled = crate::config::get_config().proxy_audio.is_enabled();
        proxy_audio_status(config_enabled, RUNTIME.lock().unwrap().is_some())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_audio_status_maps_states() {
        assert_eq!(proxy_audio_status(false, true), ModuleStatus::ConfiguredOff);
        assert_eq!(proxy_audio_status(true, true), ModuleStatus::Running);
        assert_eq!(proxy_audio_status(true, false), ModuleStatus::Paused);
    }

    #[test]
    fn retained_listeners_holds_its_generation() {
        let (tx, _rx) = channel();
        let listeners = RetainedListeners {
            generation: 3,
            addresses: [AudioObjectPropertyAddress {
                mSelector: kAudioHardwarePropertyDefaultOutputDevice,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            }; 3],
            sender: Box::new(tx),
            worker: std::thread::spawn(|| {}),
        };
        assert_eq!(listeners.generation, 3);
        assert_eq!(listeners.addresses.len(), 3);
    }
}
