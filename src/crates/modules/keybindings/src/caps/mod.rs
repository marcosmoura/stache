//! The Caps Lock adapter: chord state, ownership and crash recovery.
//!
//! The Keybindings runtime drives these pieces; the only part the host starts
//! itself is [`recover_stale_caps_remap`], which it runs once per desktop launch
//! and which does nothing without Next's own journal.
//!
//! * [`parser`] re-exports the single `CapsLock+<key>` grammar and key table.
//! * [`state`] is the pure, lock-free chord state.
//! * [`handler`] adapts it to the shared input service; it can be invalidated
//!   so it never enqueues a command again.
//! * [`owner`] orders tap-before-remap and invalidate-before-restore.
//! * [`recover`] is the host's startup recovery entry point.
//!
//! # Stated limits
//!
//! * The remap is global and carries no per-keyboard identity: a physical F18
//!   key is swallowed like the Caps surrogate while the mapping is active.
//! * If the tap is unavailable while a restore is pending, physical Caps Lock
//!   may send F18 to applications until recovery. Module dispatch is never
//!   revived to hide that.
//! * `hidutil` has no compare-and-swap; the lease coordinates only cooperating
//!   Stache Next processes and external writers can race (see
//!   `stache_macos::caps_remap`).
//! * Recovery after a crash requires a relaunch.

pub mod handler;
pub mod owner;
pub mod parser;
pub mod recover;
pub mod state;
#[cfg(test)]
mod tests;

pub use handler::{CapsHandler, CapsSink, NoSink};
pub use owner::{CapsOwner, CapsStartError, CapsStatus, StopReport};
pub use parser::{CapsShortcut, parse_shortcut};
pub use recover::{CapsRecovery, CapsRecoveryStep, recover_stale_caps_remap};
pub use state::{CapsDecision, CapsInput, CapsState, F18_KEY};
