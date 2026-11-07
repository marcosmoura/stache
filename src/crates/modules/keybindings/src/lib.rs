//! The Keybindings configuration.
//!
//! [`KeybindingsConfig`] is the single authoritative source for
//! `keybindings.jsonc`, its schema, template and legacy migration. This crate
//! registers the module: registration and construction are inert, and nothing
//! here depends on Tauri.
//!
//! [`shortcut`] and [`plan`] parse and route shortcut spellings into a pure
//! [`Plan`]. Planning registers nothing: no hotkey is active.
//!
//! [`dispatch`] is the bounded command dispatcher: 16 active plus 64 waiting
//! sequences, generation invalidation and a spawn gate.
//!
//! [`runtime`] is the module itself: [`KeybindingsRuntime`] is the host-lifetime
//! service the composition root builds (inert until the module starts), its
//! [`KeybindingsModule`] implements the lifecycle, and [`status`] holds the
//! published runtime observation.
//!
//! [`caps`] holds the Caps Lock chord state, the tap-before-remap owner and the
//! host's crash-recovery step. Recovery runs at desktop start regardless of the
//! module; the runtime owns the remap only while Caps bindings are accepted.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod caps;
mod config;
pub mod dispatch;
pub mod plan;
pub mod runtime;
pub mod shortcut;
pub mod status;
#[cfg(any(test, feature = "testing"))]
pub mod test_support;
#[cfg(test)]
mod tests;

pub use config::KeybindingsConfig;
pub use dispatch::{
    Admission, DispatchSnapshot, Dispatcher, DispatcherConfig, Invalidated, Rejection,
    ShutdownReport,
};
pub use plan::{Plan, PlanDiagnostic, PlannedBinding};
pub use runtime::{
    CounterValues, KeybindingsModule, KeybindingsParts, KeybindingsRuntime, Router, RouterCounters,
    RuntimeTiming,
};
pub use shortcut::{CapsKey, PhysicalChord, Route, ShortcutError};
pub use status::{
    BindingRoute, BindingState, BindingStatus, DiagnosticLog, DispatchStatus,
    KeybindingsRuntimeStatus, KeybindingsSnapshot, RemapPhase, RemapStatus, RetainedStatus,
    RuntimeState, TapState,
};
