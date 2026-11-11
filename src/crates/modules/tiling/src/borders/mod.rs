//! Stache-drawn Tiling borders.
//!
//! `appearance` maps the configuration to the overlay look, `model` decides
//! which windows get a border, `motion` the animations, `engine` diffs them on
//! the actor thread, `route` is the thread-safe half shared with the frame
//! paths, `spaces`/`confirm` the Spaces and Mission Control gate, and
//! `lifecycle` the stop-time ledger. Borders are default off: nothing native is
//! created until an accepted enabled configuration, and the overlays are made
//! lazily on the main thread.

mod appearance;
mod blog;
mod confirm;
mod engine;
mod lifecycle;
mod model;
#[allow(
    dead_code,
    reason = "the displayed-value sampler is used by a later part"
)]
mod motion;
mod route;
mod spaces;

#[cfg(test)]
mod engine_tests;

pub use appearance::FocusInput;
#[allow(
    unused_imports,
    reason = "the confirmation seam and its system implementation; the host composes the factory"
)]
pub use confirm::{ConfirmerFactory, OnScreenConfirm, system_confirmer_factory};
#[allow(
    unused_imports,
    reason = "the seam the native Space confirmation implements"
)]
pub use engine::{ActiveSpaceConfirm, ConfirmAll, ConfirmFailed};
#[allow(
    unused_imports,
    reason = "the budget constants are read by tests and docs"
)]
pub use engine::{BORDER_ACK_WAIT, MAIN_BUSY_DIAGNOSTIC};
pub use engine::{BorderDiagnostics, BorderEngine};
#[allow(
    unused_imports,
    reason = "some items are used by tests and the module only"
)]
pub use lifecycle::{
    BORDERS_RELEASE_WAIT, BorderLifecycle, OWNERS_STOP_WAIT, Owe, RetainedOwner, Settle,
};
pub use model::border_model;
pub use route::{BorderFrameKind, BorderFrames, NoteWaker, SharedBorderRoute, outcome_frame};
