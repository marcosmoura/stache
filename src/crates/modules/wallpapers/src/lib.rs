//! Wallpapers configuration, source resolution and the image pipeline.
//!
//! [`WallpapersConfig`] is the single authoritative source for
//! `wallpapers.toml`, its schema, template and migration. [`SourceContext`]
//! and [`SourcePool`] resolve the configured directory or list without decoding
//! an image, and [`resolve_explicit_file`] validates one explicit CLI file
//! independently of the configured pool.
//!
//! [`prepare`] and [`prepare_batch`] decode, resize, add effects and publish
//! validated PNG outputs under the identity cache; [`prepare_plan`] does the
//! same for a caller-resolved mixed-source per-display plan. [`Prepared::apply`]
//! sets them per display through an injected [`stache_macos::DesktopBackend`],
//! and [`Prepared::apply_guarded`] uses the cancellable, deadline-bounded
//! [`stache_macos::GuardedDesktopBackend`] with the prepared lease as a
//! keepalive. [`WallpapersService`], [`WallpapersModule`] and [`WallpapersIpc`]
//! implement the host-lifetime coordinator, rotation timer and typed
//! `wallpapers.set` handler; the application's `WallpapersFactory` mounts them.
//! The [`cli`] module owns the `stache wallpaper` clap tree and executor, which
//! the application contributes to the command catalog.
//!
//! # Safety
//!
//! This crate forbids unsafe code. The only native access is the safe
//! [`stache_macos`] adapter, and automated tests use its `FakeDesktop` instead
//! of constructing a real setter.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod apply;
mod batch;
mod cache;
pub mod cli;
mod config;
mod limits;
mod memo;
mod processing;
mod runtime;
mod selection;
mod source;
mod sources;

#[cfg(test)]
mod test_support;

pub use apply::{
    ApplyOutcome, PREPARE_LEASE_BUDGET, PlannedTarget, PrepareDeps, PrepareError, PreparePlanDeps,
    Prepared, PreparedTarget, prepare, prepare_plan,
};
pub use batch::{BatchDeps, BatchFailure, BatchItem, BatchOutput, BatchReport, prepare_batch};
pub use cache::{CacheError, WALLPAPER_CACHE_DIRECTORY};
pub use config::{WallpaperMode, WallpapersConfig};
pub use limits::{
    MAX_BLUR_RADIUS, MAX_CONCURRENT_WORKERS, MAX_DECODE_DIMENSION, MAX_DECODE_PIXELS,
    MAX_INTERMEDIATE_DIMENSION, MAX_INTERMEDIATE_PIXELS, MAX_SOURCE_BYTES, MAX_TARGET_DIMENSION,
    MAX_TARGET_PIXELS, ResourceError,
};
pub use memo::{MemoCounts, PrepareMemo};
pub use processing::{Effects, PROCESSING_VERSION, ProcessError};
pub use runtime::{
    Processor, RandomSource, RealProcessor, RuntimeClock, ServiceConfig, ServiceError,
    ServiceStatus, SessionStatus, SystemRandom, SystemRuntimeClock, WALLPAPERS_CLIENT_BUDGET,
    WALLPAPERS_SERVER_BUDGET, WaitOutcome, WallpaperScreen, WallpaperSelection, WallpapersIpc,
    WallpapersModule, WallpapersOutcomeCause, WallpapersOutcomeStatus, WallpapersService,
    WallpapersSetOutcome, WallpapersSetParams, WallpapersTargetFailure, WallpapersTargetOutcome,
    WallpapersTargetResult,
};
pub use selection::{
    GeometryProblem, ScreenTarget, SelectedTarget, SelectionError, TargetPixels, resolve_targets,
    target_pixels,
};
pub use sources::{
    SourceContext, SourceError, SourcePool, find_pool_entry, is_supported_image,
    resolve_explicit_file, resolve_pool,
};
