//! `MenuAnywhere` configuration and runtime.
//!
//! [`MenuAnywhereConfig`] is the single authoritative source for
//! `menuAnywhere.toml`, schema, template and migration. [`MenuAnywhereRuntime`]
//! is the host-lifetime service: an enabled [`MenuAnywhereModule`] subscribes one
//! suppressing handler to the shared input service for the configured button and
//! exact modifier set, then reads the frontmost application's menu bar through
//! Accessibility and shows it as a native menu at the pointer.
//!
//! The crate owns policy only. Every native object stays behind `stache-macos`
//! and the [`MenuHost`] seam, so building the runtime is inert: no tap,
//! observer, thread, read or menu exists until an enabled module starts, and a
//! read happens only on a trigger.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod backend;
mod config;
mod runtime;
#[cfg(any(test, feature = "testing"))]
pub mod test_support;
#[cfg(test)]
mod tests;
mod trigger;

pub use backend::{MenuHost, SystemMenuHost};
pub use config::{MenuAnywhereConfig, MenuAnywhereModifier, MenuAnywhereMouseButton, ModifierMask};
#[cfg(any(test, feature = "testing"))]
pub use runtime::StageHook;
pub use runtime::{
    ACTION_BUDGET, CAPTURE_WINDOW, HEALTH_INTERVAL, MenuAnywhereModule, MenuAnywhereParts,
    MenuAnywhereRuntime, Stage, TriggerReport,
};
pub use trigger::{
    Admission, Counters, MIDDLE_BUTTON, Matcher, Message, Outcome, Published, PublishedCell,
    RIGHT_BUTTON, Request, TriggerHandler, button_number, held_modifiers, required_flags,
};
