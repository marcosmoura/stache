//! Optional external `JankyBorders` integration (Tiling part 09).
//!
//! `policy` is the pure argv and color policy ported from the legacy module.
//! `runtime` is the owned latest-update runner thread and its lifecycle handle
//! over the `stache_macos::borders` transport. Borders are default off: nothing
//! here allocates, spawns or looks anything up until an accepted enabled
//! configuration. Stache overwrites the shared settings it names under
//! `borders.enabled`, never launches the service and leaves its last settings
//! in place on stop.

mod policy;
mod runtime;

#[cfg(test)]
pub mod fakes;
#[cfg(test)]
mod tests;

pub use policy::{ANIMATION_FPS, FRAME_PERIOD, FocusInput};
pub use runtime::{
    BORDERS_THREAD_NAME, BordersControl, BordersParts, BordersWorker, ExecutorFactory,
    FRAME_SEND_DEADLINE, Fault, FaultKind, FaultSink, JOIN_BOUND, REAP_BOUND, TransportFactory,
};
