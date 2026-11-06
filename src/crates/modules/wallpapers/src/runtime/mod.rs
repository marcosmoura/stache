#![allow(clippy::redundant_pub_crate)]

//! Runtime module, coordinator service and typed `wallpapers.set` handler.
//!
//! The factory-owned [`WallpapersService`] is host-lifetime and inert until a
//! request or a running rotation uses it. [`WallpapersModule`] creates one
//! session with at most one timer; `Module.stop` invalidates only that session
//! and its queued rotation work. The typed handler serves explicit requests
//! even while the module is disabled or paused.

mod canonical;
mod clock;
mod selection;
mod service;
mod session;
mod types;
mod worker;

#[cfg(test)]
mod tests;

pub use clock::{RandomSource, RuntimeClock, SystemRandom, SystemRuntimeClock, WaitOutcome};
pub(crate) use service::prepare_failure;
pub use service::{
    Processor, RealProcessor, ServiceConfig, ServiceError, ServiceStatus, SessionStatus,
    WALLPAPERS_CLIENT_BUDGET, WALLPAPERS_SERVER_BUDGET, WallpapersService,
};
pub use types::{
    WallpaperScreen, WallpaperSelection, WallpapersOutcomeCause, WallpapersOutcomeStatus,
    WallpapersSetOutcome, WallpapersSetParams, WallpapersTargetFailure, WallpapersTargetOutcome,
    WallpapersTargetResult,
};
pub use worker::{WallpapersIpc, WallpapersModule};
