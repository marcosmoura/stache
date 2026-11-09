//! The noTunes configuration and blocker runtime.
//!
//! [`NoTunesConfig`] is the single authoritative source for `notunes.toml`,
//! schema, template and migration. [`NoTunesModule`] implements the runtime
//! over the safe application boundary in `stache-macos`: it observes launches,
//! blocks exact Apple Music and iTunes instances and can launch the one
//! replacement player the user configured.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod policy;
mod runtime;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use config::{NoTunesConfig, ReplacementTarget};
pub use policy::{ITUNES_BUNDLE_ID, MUSIC_BUNDLE_ID};
pub use runtime::NoTunesModule;

pub(crate) fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
