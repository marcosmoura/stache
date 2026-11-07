//! Hold-to-Quit configuration and runtime.
//!
//! [`CommandQuitConfig`] is the single authoritative source for
//! `commandQuit.jsonc`, its schema, template and typed migration.
//! [`HoldToQuitRuntime`] is the host-lifetime runtime service: it subscribes one
//! suppressing handler to the shared input service for a Command-only `Cmd+Q`,
//! shows the native HUD for the press and asks the exact application instance
//! that was frontmost at key-down to quit gracefully once the hold reaches
//! `holdDuration`.
//!
//! The crate owns policy only. Every native object stays behind `stache-macos`
//! (the HUD, application observation and graceful terminate) and the host
//! supplies the frontmost-context lease, so building the runtime is inert: no
//! tap, observer, panel, lease, subscription or worker exists until an enabled
//! module starts.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod policy;
mod runtime;

#[cfg(any(test, feature = "testing"))]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use config::{CommandQuitConfig, HudAnimation, HudPosition};
pub use policy::{HOLD_KEYS, HoldClock, KEY_Q, PassReason, SystemHoldClock, is_command_only};
#[cfg(any(test, feature = "testing"))]
pub use runtime::{AdoptHook, ReconfigureHook};
pub use runtime::{
    FrontmostLeaser, HoldToQuitModule, HoldToQuitParts, HoldToQuitRuntime, HudHost, HudHostError,
    IssueReport, LeaseHandle,
};
