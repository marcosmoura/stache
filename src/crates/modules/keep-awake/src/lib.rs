//! Keep Awake configuration, runtime and published status.
//!
//! [`KeepAwakeConfig`] is the single authoritative source for
//! `keepAwake.jsonc`, its schema, template and migration.
//! [`KeepAwakeModule`] is the owned lock-aware runtime: it installs lock
//! observation, probes before acquiring and holds at most one complete
//! all-three assertion lease per generation.
//! [`KeepAwakeRuntimeStatus`] describes assertion ownership, lock knowledge,
//! observation mode and current conditions; [`KeepAwakeSnapshot`] is the
//! clonable read-only source the runtime publishes to.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod runtime;
mod status;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use config::KeepAwakeConfig;
pub use runtime::KeepAwakeModule;
pub use status::{
    AssertionState, KeepAwakeCondition, KeepAwakeConditionKind, KeepAwakeRuntimeStatus,
    KeepAwakeSnapshot, LockObservation, LockState,
};

pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
