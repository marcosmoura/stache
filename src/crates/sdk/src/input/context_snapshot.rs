//! Host-published, versioned, immutable context snapshots and the snapshot
//! cell the router reads without blocking.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, TryLockError};

/// Opaque exemption bits the host attaches to the frontmost application.
///
/// The SDK assigns no meaning to individual bits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExemptionFlags(pub u32);

/// The frontmost application as last published by the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmostApp {
    /// Bundle identifier.
    pub bundle_id: Arc<str>,
    /// Opaque exemption bits.
    pub exemptions: ExemptionFlags,
}

/// What is known about the frontmost application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontmostState {
    /// Freshly published.
    Known(FrontmostApp),
    /// Never published, or the router could not read the snapshot.
    Unknown,
    /// Published earlier and since marked out of date by the host.
    Stale,
}

/// One immutable context snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextSnapshot {
    version: u64,
    frontmost: FrontmostState,
}

impl ContextSnapshot {
    pub(crate) const fn unknown() -> Self {
        Self {
            version: 0,
            frontmost: FrontmostState::Unknown,
        }
    }

    /// Monotonic snapshot version. `0` means nothing was ever published.
    #[must_use]
    pub const fn version(&self) -> u64 { self.version }

    /// The frontmost-application state.
    #[must_use]
    pub const fn frontmost(&self) -> &FrontmostState { &self.frontmost }
}

/// A cell holding one immutable `Arc<T>` that a reader loads without blocking.
///
/// `arc-swap` is not available to this workspace offline and the SDK forbids
/// `unsafe`, so the cell is a `RwLock<Arc<T>>` that is only ever held for one
/// pointer swap or clone. Writers build the value before taking the lock. The
/// reader uses `try_read` and reports contention instead of waiting.
pub(crate) struct SnapshotCell<T> {
    current: RwLock<Arc<T>>,
}

impl<T> SnapshotCell<T> {
    pub(crate) const fn new(value: Arc<T>) -> Self { Self { current: RwLock::new(value) } }

    /// The current value, or `None` when a writer holds the cell right now.
    pub(crate) fn load_nonblocking(&self) -> Option<Arc<T>> {
        match self.current.try_read() {
            Ok(guard) => Some(Arc::clone(&guard)),
            Err(TryLockError::Poisoned(poisoned)) => Some(Arc::clone(&poisoned.into_inner())),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// Replaces the value. Called only off the tap path.
    pub(crate) fn store(&self, value: Arc<T>) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = value;
    }
}

/// The host-side handle that publishes context snapshots off the tap.
#[derive(Clone)]
pub struct ContextPublisher {
    cell: Arc<SnapshotCell<ContextSnapshot>>,
    version: Arc<AtomicU64>,
    last: Arc<std::sync::Mutex<FrontmostState>>,
}

impl ContextPublisher {
    pub(crate) fn new(cell: Arc<SnapshotCell<ContextSnapshot>>) -> Self {
        Self {
            cell,
            version: Arc::new(AtomicU64::new(0)),
            last: Arc::new(std::sync::Mutex::new(FrontmostState::Unknown)),
        }
    }

    /// Publishes a fresh frontmost application and returns the new version.
    #[allow(clippy::must_use_candidate)]
    pub fn publish_frontmost(&self, app: FrontmostApp) -> u64 {
        self.publish(FrontmostState::Known(app))
    }

    /// Marks the last published context as out of date and returns the version.
    #[allow(clippy::must_use_candidate)]
    pub fn mark_stale(&self) -> u64 { self.publish(FrontmostState::Stale) }

    /// Marks the context unknown and returns the version.
    #[allow(clippy::must_use_candidate)]
    pub fn mark_unknown(&self) -> u64 { self.publish(FrontmostState::Unknown) }

    /// The snapshot the router would read now. Tests only.
    #[cfg(any(test, feature = "testing"))]
    #[must_use]
    pub fn snapshot(&self) -> Arc<ContextSnapshot> {
        self.cell
            .load_nonblocking()
            .unwrap_or_else(|| Arc::new(ContextSnapshot::unknown()))
    }

    fn publish(&self, frontmost: FrontmostState) -> u64 {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        let version = self.version.fetch_add(1, Ordering::SeqCst) + 1;
        *last = frontmost.clone();
        self.cell.store(Arc::new(ContextSnapshot { version, frontmost }));
        version
    }
}
