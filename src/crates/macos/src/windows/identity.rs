//! Opaque identities: adapter, runtime epoch and window token.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::applications::ApplicationId;

static NEXT_ADAPTER: AtomicU64 = AtomicU64::new(1);

/// The identity of one window adapter (one AX owner thread).
///
/// A token minted by one adapter is never accepted by another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AdapterId(u64);

impl AdapterId {
    pub(super) fn allocate() -> Self { Self(NEXT_ADAPTER.fetch_add(1, Ordering::Relaxed)) }

    /// A fresh id that no running adapter owns. A token carrying it never names
    /// a live window of any adapter, so every operation on it is stale.
    #[must_use]
    pub fn detached() -> Self { Self::allocate() }

    /// A fresh synthetic adapter id for fakes.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn for_testing() -> Self { Self::allocate() }
}

/// One install-to-shutdown lifetime of an adapter. Strictly increasing per
/// adapter and never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeEpoch(u64);

impl RuntimeEpoch {
    pub(super) const fn new(value: u64) -> Self { Self(value) }

    /// A synthetic epoch for fakes.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub const fn for_testing(value: u64) -> Self { Self(value) }

    /// The numeric value, diagnostic only.
    #[must_use]
    pub const fn value(self) -> u64 { self.0 }
}

/// An opaque, `Copy`, thread-safe token for one window instance.
///
/// It is `(adapter, runtime epoch, exact application instance, serial)`. The
/// serial is allocated monotonically by the owner when it first adopts the AX
/// element; it is never reused, so a destroyed window's token can never name a
/// later window, even one that reuses the same `CGWindowID`. The retained AX
/// element lives only in the owner thread's map; the token carries no pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowInstance {
    adapter: AdapterId,
    epoch: RuntimeEpoch,
    application: ApplicationId,
    serial: u64,
}

impl WindowInstance {
    pub(super) const fn new(
        adapter: AdapterId,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        serial: u64,
    ) -> Self {
        Self {
            adapter,
            epoch,
            application,
            serial,
        }
    }

    /// A synthetic token for fakes.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub const fn for_testing(
        adapter: AdapterId,
        epoch: RuntimeEpoch,
        application: ApplicationId,
        serial: u64,
    ) -> Self {
        Self::new(adapter, epoch, application, serial)
    }

    /// The adapter that minted the token.
    #[must_use]
    pub const fn adapter(&self) -> AdapterId { self.adapter }

    /// The runtime epoch that minted the token.
    #[must_use]
    pub const fn epoch(&self) -> RuntimeEpoch { self.epoch }

    /// The exact application instance that owns the window.
    #[must_use]
    pub const fn application(&self) -> ApplicationId { self.application }

    /// The monotonically allocated AX instance serial (diagnostic only).
    #[must_use]
    pub const fn serial(&self) -> u64 { self.serial }
}
