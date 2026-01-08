//! Audio device watcher using `CoreAudio` property listeners.
//!
//! This module handles monitoring for audio device changes and
//! automatically applying priority-based device switching.

use std::ffi::c_void;
use std::ptr::{NonNull, null};
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};

use objc2_core_audio::{
    AudioDeviceID, AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
    AudioObjectSetPropertyData, kAudioHardwareNoError, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyDevices,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};

use super::device::{
    get_default_input_device, get_default_output_device, get_input_devices, get_output_devices,
};
use super::priority;
use crate::config::ProxyAudioConfig;
use crate::utils::thread::spawn_named_thread;

/// Stores the Sender used by audio property listeners.
/// This is intentionally kept alive for the application's lifetime since the
/// `CoreAudio` property listeners need a valid pointer to send device change events.
/// The raw pointer is passed to `CoreAudio` callbacks and must remain valid.
static LISTENER_SENDER: OnceLock<Box<Sender<()>>> = OnceLock::new();

/// Ensures the audio watcher is only initialized once.
static AUDIO_WATCHER_ONCE: OnceLock<()> = OnceLock::new();

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

    if set_default_output_device(target.id) {
        println!("Default output device set to {}", target.name);
    } else {
        eprintln!("Failed to set default output device to {}", target.name);
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

    if set_default_input_device(target.id) {
        println!("Default input device set to {}", target.name);
    } else {
        eprintln!("Failed to set default input device to {}", target.name);
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

/// Registers listeners for audio device changes.
///
/// The `Sender` is stored in a static to ensure it lives for the application's
/// lifetime, as `CoreAudio` callbacks require a valid pointer.
fn register_audio_listeners(tx: Sender<()>) {
    // Store the sender in a static to ensure it lives for the app's lifetime.
    // CoreAudio callbacks will use this pointer to send device change events.
    let sender_box = LISTENER_SENDER.get_or_init(|| Box::new(tx));
    // Cast to *mut for CoreAudio API compatibility (the callback only reads from it)
    let tx_ptr: *mut c_void =
        std::ptr::from_ref::<Sender<()>>(sender_box.as_ref()).cast_mut().cast();

    // Listen for default output device changes
    let output_property_address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };

    unsafe {
        AudioObjectAddPropertyListener(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&output_property_address),
            Some(audio_device_property_listener),
            tx_ptr,
        );
    }

    // Listen for default input device changes
    let input_property_address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultInputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };

    unsafe {
        AudioObjectAddPropertyListener(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&input_property_address),
            Some(audio_device_property_listener),
            tx_ptr,
        );
    }

    // Listen for device list changes (connect/disconnect)
    let devices_property_address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };

    unsafe {
        AudioObjectAddPropertyListener(
            kAudioObjectSystemObject as AudioObjectID,
            NonNull::from(&devices_property_address),
            Some(audio_device_property_listener),
            tx_ptr,
        );
    }
}

/// Initializes the audio device watcher with the given configuration.
///
/// This function spawns a background thread that monitors for audio device
/// changes and automatically switches devices based on priority rules.
///
/// # Arguments
///
/// * `config` - Proxy audio configuration for device priority rules.
pub fn init_audio_device_watcher(config: ProxyAudioConfig) {
    spawn_named_thread("audio-device-watcher", move || {
        let (tx, rx) = channel();

        // Register all audio device listeners
        register_audio_listeners(tx);

        // Wait for device change events
        while rx.recv().is_ok() {
            on_audio_device_change(&config);
        }
    });
}

/// Starts the audio device watcher.
///
/// This is idempotent - calling it multiple times has no effect.
///
/// # Arguments
///
/// * `config` - Proxy audio configuration for device priority rules.
pub fn start(config: ProxyAudioConfig) {
    if AUDIO_WATCHER_ONCE.set(()).is_err() {
        return;
    }

    // Apply initial device configuration
    on_audio_device_change(&config);

    // Start watching for device changes
    init_audio_device_watcher(config);
}
