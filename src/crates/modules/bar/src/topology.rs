//! The seam to the host's Bar-scoped screen-topology feed.
//!
//! Bar needs two things from the host: the attached displays (to compute its
//! one frame) and a topology generation the screen-reservation registry
//! accepts (to stage its reservation). The host owns both: only the app
//! applies a topology to the registry, and only while Bar runs (overview
//! Decision 18). This crate sees that through two traits so it never depends
//! on the app.
//!
//! A [`TopologyLease`] is the RAII form of "the feed is on for this Bar runtime
//! generation". Beginning one activates the feed; ending or dropping it must
//! apply an empty topology first, so Tiling returns to "no host topology" on
//! every end of the generation: stop, pause, disable, failure, a failed or
//! partial start and permission revocation all end the lease.

use std::sync::Arc;
use std::time::Duration;

use stache_macos::DisplayLayout;
use stache_sdk::{CancellationToken, DisplayId, TopologyGeneration};

/// One point-in-time view of the feed.
#[derive(Clone, Debug)]
pub struct TopologyReading {
    /// Changes whenever the layout, the primary or the error changes.
    pub revision: u64,
    /// The topology generation the host applied for this layout.
    pub generation: TopologyGeneration,
    /// Every attached display in native order; `None` when the host could not
    /// read it (see `error`).
    pub layout: Option<Arc<DisplayLayout>>,
    /// The SDK identity of the layout's primary display (`screens[0]`).
    pub primary: Option<DisplayId>,
    /// Why `layout` is unavailable, when it is.
    pub error: Option<String>,
}

/// How a bounded wait for a newer reading ended.
#[derive(Clone, Debug)]
pub enum TopologyWait {
    /// A reading with a different revision.
    Changed(TopologyReading),
    /// The timeout elapsed first.
    TimedOut,
    /// A supplied token fired first.
    Cancelled,
}

/// The feed while it is on for one Bar runtime generation.
pub trait TopologyLease: Send + Sync {
    /// The latest reading. Never blocks on native work.
    fn read(&self) -> TopologyReading;

    /// Waits for a reading whose revision differs from `after`, for `timeout`,
    /// or until any token fires.
    fn wait_change(
        &self,
        after: u64,
        timeout: Duration,
        cancel: &[&CancellationToken],
    ) -> TopologyWait;

    /// Ends the feed: applies an empty topology first, then removes callbacks
    /// and joins owned workers within a bound. Idempotent. `false` means
    /// something is still owed and the lease must be ended again; the empty
    /// topology is already applied either way.
    fn end(&self) -> bool;
}

/// The host component that starts a [`TopologyLease`].
pub trait TopologySource: Send + Sync {
    /// Turns the feed on and returns after the first reading exists.
    ///
    /// # Errors
    /// Why the feed could not start. Nothing stays acquired and no topology
    /// stays applied.
    fn begin(&self) -> Result<Arc<dyn TopologyLease>, String>;
}
