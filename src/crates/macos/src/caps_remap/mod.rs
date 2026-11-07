//! Safe ownership of the global Caps Lock to F18 `hidutil` mapping.
//!
//! A `CGEventTap` cannot reliably stop Caps Lock's toggle and LED, so the
//! Keybindings Caps adapter maps the physical key to F18 at the HID layer. The
//! mapping is **global and survives the process**, so ownership is a durable
//! transaction rather than a toggle:
//!
//! ```text
//! lease (5 s, identity-wide) -> strict read -> journal (write-ahead, atomic)
//!      -> hidutil set (5 s / 1 MiB / 64 KiB) -> reread + verify -> phase / retire
//! ```
//!
//! * [`mapping`] strictly parses `hidutil` output and plans edits that preserve
//!   every unrelated entry exactly.
//! * [`hidutil`] is the bounded child-process boundary over the shared command
//!   executor.
//! * [`journal`] and [`storage`] define the versioned journal and its durable,
//!   no-follow storage, plus the lease.
//! * [`engine`] is the transaction: [`CapsRemap::apply`] and
//!   [`CapsRemap::restore`], with truthful outcomes.
//!
//! This module never decides *when* to remap. It does not know about event
//! taps: the caller must hold a confirmed tap first. Nothing here runs at
//! start-up except what the host explicitly invokes, and no default test runs
//! the real `hidutil`.

pub mod engine;
pub mod hidutil;
pub mod journal;
pub mod mapping;
pub mod storage;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod tests;

pub use engine::{ApplyError, CapsRemap, RestoreOutcome, STARTUP_RECOVERY_BOUND};
pub use hidutil::{
    HIDUTIL_DEADLINE, HIDUTIL_PATH, HIDUTIL_STDERR_CAP, HIDUTIL_STDOUT_CAP, Hidutil, HidutilError,
    HidutilErrorKind, ProcessHidutil,
};
pub use journal::{Journal, JournalError, Phase};
pub use mapping::{
    CAPS_LOCK_USAGE, CapsState, F18_USAGE, MAX_ENTRIES, Mapping, MappingError, OWNED, caps_state,
    parse_user_key_mapping, render_set_payload, with_surrogate, without_surrogate,
};
pub use storage::{
    CapsLeases, DIRECTORY, FsJournalStorage, FsLeases, JOURNAL_FILE, JOURNAL_MAX_BYTES,
    JournalStorage, LEASE_BUDGET, LOCK_FILE, LeaseFailure, LeaseGuard, StorageError,
};
