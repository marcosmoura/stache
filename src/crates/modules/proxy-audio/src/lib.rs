//! The proxyAudio pilot: policy, runtime, CLI and read-only socket surface.
//!
//! [`ProxyAudioConfig`] is the single authoritative configuration type used by
//! schema generation, migration and the runtime. [`ProxyAudioModule`] owns one
//! listener generation and one cancellable worker; [`cli`] and [`ipc`] expose
//! read-only device listings without starting either.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cli;
mod config;
pub mod device;
pub mod ipc;
pub mod list;
pub mod priority;
pub mod watcher;

#[cfg(test)]
mod test_support;

pub use config::{AudioDeviceDependency, AudioDevicePriority, MatchStrategy, ProxyAudioConfig};
pub use device::{AudioDevice, AudioDeviceType};
pub use ipc::{AudioIpc, AudioListKind, AudioListParams};
pub use list::{AudioDeviceInfo, DeviceFilter, format_devices_table, list_devices};
pub use watcher::ProxyAudioModule;
