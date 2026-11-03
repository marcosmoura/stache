//! Safe macOS adapters.
//!
//! This is the only Stache crate allowed to contain project-authored unsafe
//! code. Each unsafe block states the ownership or thread invariant it relies
//! on; public functions are safe.
#![warn(missing_docs)]

pub mod accessibility;
pub mod activation;
pub mod audio;
pub mod display;
pub mod main_thread;

pub use accessibility::{AccessibilityPermission, is_trusted, request_trust};
pub use activation::ActivationObserver;
pub use audio::{
    AudioBackend, AudioChangeSubscription, AudioDevice, AudioDeviceId, AudioDirection, AudioError,
    AudioWakeup, CoreAudioBackend,
};
pub use display::is_screen_mirroring_active;
pub use main_thread::{MainThread, is_main_thread, run_on_main};
