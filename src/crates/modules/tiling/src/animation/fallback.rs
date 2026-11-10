//! The instant-fallback set: applications that move without animation.
//!
//! An application enters the set when it cannot keep up (the coordinator, from
//! its metrics), when its Enhanced UI lease cannot be held (the actor), or when
//! the smooth path is unavailable for it. It is sticky for that exact
//! application instance until the application terminates or animations are
//! turned off; it never affects another application and never changes the
//! configuration. The planner reads a copy of the ids before each transition.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

use stache_macos::ApplicationId;

/// Why an application moves without animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// Wrote fewer than 15 frames per second in 2 of its last 3 layout changes.
    BelowMinimumRate,
    /// Ended more than 250 ms late in 2 of its last 3 layout changes.
    LateEnd,
    /// It turns Enhanced UI back on.
    ReEnablesEnhancedUi,
    /// Enhanced UI could not be turned off.
    EnhancedUiStuck,
    /// It shares the overflow AX thread and cannot keep up.
    Overflow,
    /// Its display has no frame clock and it cannot keep up.
    NoFrameClock,
}

impl fmt::Display for FallbackReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BelowMinimumRate => {
                "writes fewer than 15 frames per second in 2 of its last 3 layout changes"
            }
            Self::LateEnd => "ends more than 250 ms late in 2 of its last 3 layout changes",
            Self::ReEnablesEnhancedUi => "turns Enhanced UI back on",
            Self::EnhancedUiStuck => "Enhanced UI could not be turned off",
            Self::Overflow => "shares the overflow AX thread and cannot keep up",
            Self::NoFrameClock => "its display has no frame clock and it cannot keep up",
        })
    }
}

/// The applications in fallback, shared by the coordinator, the actor and
/// status. The lock is never held across a call-out.
#[derive(Debug, Default)]
pub struct FallbackSet {
    inner: Mutex<BTreeMap<ApplicationId, FallbackReason>>,
}

impl FallbackSet {
    fn map(&self) -> MutexGuard<'_, BTreeMap<ApplicationId, FallbackReason>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Puts `application` in fallback; the first reason stays. `true` when it
    /// was not in the set.
    pub fn insert(&self, application: ApplicationId, reason: FallbackReason) -> bool {
        let mut map = self.map();
        if map.contains_key(&application) {
            return false;
        }
        map.insert(application, reason);
        true
    }

    /// Whether `application` moves without animation.
    #[must_use]
    pub fn contains(&self, application: ApplicationId) -> bool {
        self.map().contains_key(&application)
    }

    /// The applications in fallback.
    #[must_use]
    pub fn ids(&self) -> BTreeSet<ApplicationId> { self.map().keys().copied().collect() }

    /// Each application in fallback with its reason.
    #[must_use]
    pub fn entries(&self) -> Vec<(ApplicationId, FallbackReason)> {
        self.map().iter().map(|(app, reason)| (*app, *reason)).collect()
    }

    /// Releases one application (it terminated).
    pub fn forget(&self, application: ApplicationId) { self.map().remove(&application); }

    /// Releases everything (animations were turned off).
    pub fn clear(&self) { self.map().clear(); }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool { self.map().is_empty() }
}
